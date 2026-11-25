//! B0's C fixture plugin, compiled per test against
//! `include/vosu_plugin.h` with its marker directory baked in.
//!
//! Core drives it with `DEMAND_CHANGED` frames whose `handle` is
//! `0xB0DE_0000 | n`: the fixture reads the request file `request-<n>`
//! (`<mode> <opcode> <flags> <correlation>` on the first line, the payload
//! in hex on the second) and `send`s exactly those bytes — from a thread of
//! its own (`thread`) or from the dispatch worker itself (`dispatch`). It
//! then writes the marker `response-<n>-<pid>`: `send`'s status, whether a
//! `READ_PLAN_RESULT` reached that same thread before `send` returned
//! (`same_thread=1`, aligned), that result's payload in hex, and the
//! transport census: every byte each ring carried while `send` ran
//! (`p2h=`, `h2p=`, `-` outside a plugin host). The mode `exit` ends the
//! plugin host at once, the way a crash would, without running `destroy`.
//! Every other host → plugin message is appended to `log-<pid>`.
//!
//! The census maps both rings of the inherited endpoint read-only, exactly
//! as any library loaded into a plugin host could, and copies the bytes
//! between each ring's write cursor before and after the `send`. The ring
//! constants mirror `crates/vosu-core/src/plugin/process/ring.rs`
//! (plugin-abi.md §7.2); the frame decoder below fails the test if they
//! drift.

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::{Duration, Instant};

const SOURCE: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "fixture_config.h"
#include "vosu_plugin.h"

#ifdef _WIN32
#include <windows.h>
#define FIXTURE_EXPORT __declspec(dllexport)
static unsigned long fixture_pid(void) { return (unsigned long)GetCurrentProcessId(); }
static SRWLOCK LOG_LOCK = SRWLOCK_INIT;
static void log_lock(void) { AcquireSRWLockExclusive(&LOG_LOCK); }
static void log_unlock(void) { ReleaseSRWLockExclusive(&LOG_LOCK); }
static DWORD WINAPI request_entry(LPVOID arg);
static int start_thread(void *arg) {
    HANDLE thread = CreateThread(NULL, 0, request_entry, arg, 0, NULL);
    if (!thread) return 0;
    CloseHandle(thread);
    return 1;
}
static void exit_now(void) { TerminateProcess(GetCurrentProcess(), 70); }
#define TLS __declspec(thread)
#else
#include <pthread.h>
#include <sys/mman.h>
#include <unistd.h>
#define FIXTURE_EXPORT __attribute__((visibility("default")))
static unsigned long fixture_pid(void) { return (unsigned long)getpid(); }
static pthread_mutex_t LOG_LOCK = PTHREAD_MUTEX_INITIALIZER;
static void log_lock(void) { pthread_mutex_lock(&LOG_LOCK); }
static void log_unlock(void) { pthread_mutex_unlock(&LOG_LOCK); }
static void *request_entry(void *arg);
static int start_thread(void *arg) {
    pthread_t thread;
    if (pthread_create(&thread, NULL, request_entry, arg) != 0) return 0;
    pthread_detach(thread);
    return 1;
}
static void exit_now(void) { _exit(70); }
#define TLS __thread
#endif

#define RING_HEADER 256u
#define RING_CAPACITY (2u * 1024u * 1024u)
#define RING_WRITE_CURSOR 64u
#define CENSUS_MAX (64u * 1024u)

/* Both directions of the inherited endpoint, mapped read-only; NULL when
   this library runs in-process. */
static const volatile uint8_t *RING_H2P;
static const volatile uint8_t *RING_P2H;

#ifdef _WIN32
static void map_rings(void) {
    const char *at = strstr(GetCommandLineA(), "h2p=");
    unsigned long long h2p, h2p_event, p2h, p2h_event;
    if (!at || sscanf(at, "h2p=%llx:%llx,p2h=%llx:%llx", &h2p, &h2p_event, &p2h, &p2h_event) != 4) return;
    RING_H2P = MapViewOfFile((HANDLE)(uintptr_t)h2p, FILE_MAP_READ, 0, 0, RING_HEADER + RING_CAPACITY);
    RING_P2H = MapViewOfFile((HANDLE)(uintptr_t)p2h, FILE_MAP_READ, 0, 0, RING_HEADER + RING_CAPACITY);
}
#else
static void map_rings(void) {
    /* The plugin host's last argument names its four descriptors:
       host → plugin mapping and doorbell, then plugin → host. */
    static char line[16384];
    FILE *file = fopen("/proc/self/cmdline", "rb");
    if (!file) return;
    size_t n = fread(line, 1, sizeof line - 1, file);
    fclose(file);
    if (n == 0) return;
    line[n] = 0;
    size_t end = line[n - 1] == 0 ? n - 1 : n;
    size_t start = end;
    while (start > 0 && line[start - 1] != 0) start--;
    int fds[4];
    char tail;
    if (sscanf(line + start, "%d,%d,%d,%d%c", &fds[0], &fds[1], &fds[2], &fds[3], &tail) != 4) return;
    void *h2p = mmap(NULL, RING_HEADER + RING_CAPACITY, PROT_READ, MAP_SHARED, fds[0], 0);
    void *p2h = mmap(NULL, RING_HEADER + RING_CAPACITY, PROT_READ, MAP_SHARED, fds[2], 0);
    if (h2p != MAP_FAILED) RING_H2P = h2p;
    if (p2h != MAP_FAILED) RING_P2H = p2h;
}
#endif

static uint64_t ring_cursor(const volatile uint8_t *ring) {
    if (!ring) return 0;
    return __atomic_load_n((const uint64_t *)(ring + RING_WRITE_CURSOR), __ATOMIC_ACQUIRE);
}

typedef struct fixture {
    const vosu_host_services *host;
    void *host_ctx;
} fixture;

typedef struct request {
    fixture *self;
    unsigned n;
    char mode[16];
    uint32_t opcode;
    uint32_t flags;
    unsigned long long correlation;
    uint8_t *payload;
    uint32_t len;
} request;

static TLS int tl_waiting;
static TLS int tl_got;
static TLS char *tl_result;

static void path_of(char *path, size_t size, const char *name, int with_pid) {
    if (with_pid) snprintf(path, size, "%s/%s-%lu", FIXTURE_DIR, name, fixture_pid());
    else snprintf(path, size, "%s/%s", FIXTURE_DIR, name);
}

static void marker(const char *probe, const char *text) {
    char path[4096];
    char partial[4200];
    path_of(path, sizeof path, probe, 1);
    snprintf(partial, sizeof partial, "%s.partial", path);
    FILE *file = fopen(partial, "wb");
    if (!file) return;
    fputs(text, file);
    fclose(file);
    rename(partial, path);
}

static void log_message(const vosu_message *message) {
    char path[4096];
    path_of(path, sizeof path, "log", 1);
    log_lock();
    FILE *file = fopen(path, "ab");
    if (file) {
        fprintf(file, "%u %llu ", message->opcode, (unsigned long long)message->correlation_id);
        for (uint32_t i = 0; i < message->payload.len; i++) fprintf(file, "%02x", message->payload.data[i]);
        fputc('\n', file);
        fclose(file);
    }
    log_unlock();
}

static char *hex_of(const uint8_t *data, uint32_t len) {
    char *text = malloc((size_t)len * 2 + 1);
    if (!text) return NULL;
    for (uint32_t i = 0; i < len; i++) snprintf(text + i * 2, 3, "%02x", data[i]);
    text[(size_t)len * 2] = 0;
    return text;
}

/* The bytes `ring` carried between the write cursors `from` and `to`. */
static char *census(const volatile uint8_t *ring, uint64_t from, uint64_t to) {
    const char *fixed = !ring ? "-" : (to - from > CENSUS_MAX ? "overflow" : NULL);
    if (fixed) {
        char *text = malloc(strlen(fixed) + 1);
        if (text) strcpy(text, fixed);
        return text;
    }
    uint32_t len = (uint32_t)(to - from);
    uint8_t *bytes = malloc(len ? len : 1);
    if (!bytes) return NULL;
    for (uint32_t i = 0; i < len; i++) bytes[i] = ring[RING_HEADER + ((from + i) & (RING_CAPACITY - 1))];
    char *text = hex_of(bytes, len);
    free(bytes);
    return text;
}

static request *read_request(fixture *self, unsigned n) {
    char name[64];
    char path[4096];
    snprintf(name, sizeof name, "request-%u", n);
    path_of(path, sizeof path, name, 0);
    FILE *file = fopen(path, "rb");
    if (!file) return NULL;
    request *r = calloc(1, sizeof *r);
    if (!r) { fclose(file); return NULL; }
    r->self = self;
    r->n = n;
    if (fscanf(file, "%15s %u %u %llu", r->mode, &r->opcode, &r->flags, &r->correlation) != 4) {
        fclose(file);
        free(r);
        return NULL;
    }
    r->payload = malloc(1 << 20);
    unsigned byte;
    while (r->payload && r->len < (1u << 20) && fscanf(file, "%2x", &byte) == 1) r->payload[r->len++] = (uint8_t)byte;
    fclose(file);
    return r;
}

static void run_request(request *r) {
    vosu_message message;
    message.opcode = r->opcode;
    message.flags = r->flags;
    message.correlation_id = r->correlation;
    message.payload.data = r->payload;
    message.payload.len = r->len;
    tl_waiting = 1;
    tl_got = 0;
    tl_result = NULL;
    uint64_t p2h_before = ring_cursor(RING_P2H);
    uint64_t h2p_before = ring_cursor(RING_H2P);
    uint32_t status = (uint32_t)r->self->host->send(r->self->host_ctx, &message);
    uint64_t p2h_after = ring_cursor(RING_P2H);
    uint64_t h2p_after = ring_cursor(RING_H2P);
    tl_waiting = 0;
    char *p2h = census(RING_P2H, p2h_before, p2h_after);
    char *h2p = census(RING_H2P, h2p_before, h2p_after);
    size_t size = 256 + (tl_result ? strlen(tl_result) : 1) + (p2h ? strlen(p2h) : 1) + (h2p ? strlen(h2p) : 1);
    char *text = malloc(size);
    if (text) {
        snprintf(text, size, "status=%u same_thread=%d result=%s p2h=%s h2p=%s\n", status, tl_got,
            tl_result ? tl_result : "-", p2h ? p2h : "-", h2p ? h2p : "-");
        char name[64];
        snprintf(name, sizeof name, "response-%u", r->n);
        marker(name, text);
        free(text);
    }
    free(p2h);
    free(h2p);
    free(tl_result);
    free(r->payload);
    free(r);
}

#ifdef _WIN32
static DWORD WINAPI request_entry(LPVOID arg) { run_request(arg); return 0; }
#else
static void *request_entry(void *arg) { run_request(arg); return NULL; }
#endif

static void *fixture_create(const vosu_host_services *host, void *host_ctx, vosu_bytes init) {
    (void)init;
    fixture *self = calloc(1, sizeof *self);
    if (!self) return NULL;
    self->host = host;
    self->host_ctx = host_ctx;
    map_rings();
    return self;
}

static vosu_status fixture_dispatch(void *ctx, const vosu_message *message) {
    fixture *self = ctx;
    if (message->opcode == VOSU_OP_READ_PLAN_RESULT && tl_waiting) {
        tl_got = ((uintptr_t)message->payload.data % 8 == 0) ? 1 : 2;
        tl_result = hex_of(message->payload.data, message->payload.len);
        return VOSU_OK;
    }
    if (message->opcode == VOSU_OP_DEMAND_CHANGED && message->payload.len >= 8) {
        uint64_t which;
        memcpy(&which, message->payload.data, 8);
        if ((which & 0xFFFF0000u) == 0xB0DE0000u && (which >> 32) == 0) {
            request *r = read_request(self, (unsigned)(which & 0xFFFFu));
            if (!r) { marker("bad-request", "1\n"); return VOSU_OK; }
            if (strcmp(r->mode, "exit") == 0) exit_now();
            if (strcmp(r->mode, "dispatch") == 0) run_request(r);
            else if (!start_thread(r)) marker("bad-thread", "1\n");
            return VOSU_OK;
        }
    }
    log_message(message);
    return VOSU_OK;
}

static void fixture_destroy(void *ctx) { free(ctx); }

static const vosu_plugin_vtable VTABLE = {
    sizeof(vosu_plugin_vtable),
    VOSU_PLUGIN_ABI_MAJOR,
    fixture_create,
    fixture_dispatch,
    fixture_destroy,
};

FIXTURE_EXPORT const vosu_plugin_vtable *vosu_plugin_entry_v1(void) { return &VTABLE; }
"#;

/// `$CC`, `cc`, `clang` or `gcc` (Windows needs Clang). A missing compiler
/// fails the test: the fixture is required evidence.
fn compiler() -> String {
    let mut candidates = Vec::new();
    if let Ok(cc) = std::env::var("CC") {
        candidates.push(cc);
    }
    candidates.extend(["cc", "clang", "gcc"].map(str::to_owned));
    candidates
        .into_iter()
        .find(|candidate| {
            Process::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .expect("a C compiler ($CC, cc, clang or gcc) is required to build the B0 fixture")
}

/// The library file name for this target.
pub fn library_name() -> &'static str {
    if cfg!(windows) {
        "b0_fixture.dll"
    } else {
        "libb0_fixture.so"
    }
}

/// Builds the fixture in `build_dir`, writing markers into `markers`, and
/// returns the library's bytes.
pub fn build(build_dir: &Path, markers: &Path) -> Vec<u8> {
    let escaped = markers
        .to_str()
        .expect("UTF-8 temp path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    std::fs::write(
        build_dir.join("fixture_config.h"),
        format!("#define FIXTURE_DIR \"{escaped}\"\n"),
    )
    .unwrap();
    let source = build_dir.join("fixture.c");
    std::fs::write(&source, SOURCE).unwrap();
    let output_path = build_dir.join(library_name());
    let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
    let compiler = compiler();
    let mut command = Process::new(&compiler);
    command.arg("-shared");
    if !cfg!(windows) {
        command.args(["-fPIC", "-pthread"]);
    }
    let output = command
        .arg("-I")
        .arg(&include)
        .arg("-I")
        .arg(build_dir)
        .arg("-o")
        .arg(&output_path)
        .arg(&source)
        .output()
        .unwrap_or_else(|error| panic!("running {compiler}: {error}"));
    assert!(
        output.status.success(),
        "{compiler} failed to build the B0 fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&output_path).unwrap()
}

/// Waits up to `bound` for the one `<probe>-<pid>` marker, returning its
/// pid and contents.
#[expect(
    clippy::disallowed_methods,
    reason = "markers are files another process writes; the wait polls them until a bounded deadline"
)]
pub fn wait_marker(dir: &Path, probe: &str, bound: Duration) -> Option<(u32, String)> {
    let deadline = Instant::now() + bound;
    loop {
        if let Some(found) = marker(dir, probe) {
            return Some(found);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The `<probe>-<pid>` marker, if one was written.
pub fn marker(dir: &Path, probe: &str) -> Option<(u32, String)> {
    let prefix = format!("{probe}-");
    std::fs::read_dir(dir).unwrap().find_map(|entry| {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let pid = name.strip_prefix(&prefix)?.parse::<u32>().ok()?;
        Some((pid, std::fs::read_to_string(dir.join(&name)).unwrap()))
    })
}

/// The `key=value` pairs of a marker line.
pub fn field(text: &str, key: &str) -> String {
    text.split_whitespace()
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("{key} missing from {text:?}"))
        .to_owned()
}

/// One logged host → plugin message: `(opcode, correlation, payload)`.
pub type Logged = (u32, u64, Vec<u8>);

/// Every logged message of the process `pid`, in dispatch order; a line
/// still being written is left out.
pub fn log(dir: &Path, pid: u32) -> Vec<Logged> {
    let Ok(text) = std::fs::read_to_string(dir.join(format!("log-{pid}"))) else {
        return Vec::new();
    };
    text.split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n'))
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let opcode = parts.next().unwrap().parse().unwrap();
            let correlation = parts.next().unwrap().parse().unwrap();
            (opcode, correlation, hex(parts.next().unwrap_or("")))
        })
        .collect()
}

/// The processes that logged anything into `dir`.
pub fn logging_processes(dir: &Path) -> Vec<u32> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.strip_prefix("log-")?.parse().ok()
        })
        .collect()
}

/// Bytes of a hex string.
pub fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// One frame a ring carried: `(opcode, flags, correlation, payload)`.
pub type Frame = (u32, u32, u64, Vec<u8>);

/// Splits census bytes into whole frames (`docs/v2/plugin-abi.md` §7.2
/// "Framing": `u32 total_len; u32 opcode; u32 flags; u64 correlation;
/// u32 payload_len; payload`, `total_len` excluding its own 4 bytes). The
/// census must end on a frame boundary.
pub fn frames(census: &str) -> Vec<Frame> {
    let bytes = hex(census);
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let mut frames = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        assert!(at + 24 <= bytes.len(), "a partial frame head in {census}");
        let total = word(at) as usize;
        let payload_len = word(at + 20) as usize;
        assert_eq!(
            total,
            payload_len + 20,
            "a malformed frame head in {census}"
        );
        let end = at + 24 + payload_len;
        assert!(end <= bytes.len(), "a partial frame in {census}");
        let correlation = u64::from_le_bytes(bytes[at + 12..at + 20].try_into().unwrap());
        frames.push((
            word(at + 4),
            word(at + 8),
            correlation,
            bytes[at + 24..end].to_vec(),
        ));
        at = end;
    }
    frames
}

/// Writes the request file `request-<n>` the fixture reads.
pub fn write_request(dir: &Path, n: u16, mode: &str, opcode: u32, flags: u32, payload: &[u8]) {
    let hex: String = payload.iter().map(|byte| format!("{byte:02x}")).collect();
    std::fs::write(
        dir.join(format!("request-{n}")),
        format!("{mode} {opcode} {flags} 0\n{hex}\n"),
    )
    .unwrap();
}

/// A scratch directory under `root`, created.
pub fn dir(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

//! T1's C fixture plugin for the relay cases, compiled per test against
//! `include/vosu_plugin.h` with its marker directory baked in, so no state
//! is shared between tests and nothing depends on environment variables.
//!
//! Core drives it with `DEMAND_CHANGED` frames whose `handle` is one of the
//! [`Command`] values (real session handles are small, so they never
//! collide). Every other host → plugin message it receives on a dispatch
//! worker is appended to `log-<pid>` as `<opcode> <payload hex>` lines.
//! Markers are files `<probe>-<pid>` in the same directory; barriers are
//! files the test creates (`probe`, `release`).
//!
//! On `SESSION_ATTACHED` it remembers the session handle. While blocked in
//! [`Command::Block`] it starts a thread of its own that submits a
//! `VOSU_READ_PLAN_SYNC` plan reading 8 bytes at `0x1000` and records
//! (marker `sync`) its status, whether the result was dispatched on that
//! same submitting thread (`same_thread=1`, aligned), the value read, and
//! whether the test had already released the blocked dispatch then.

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::{Duration, Instant};

/// The commands, as `DEMAND_CHANGED` handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Command {
    /// On the dispatch worker: try a SYNC plan there (marker
    /// `dispatch_sync`), start the background SYNC submission, wait for
    /// `probe` and answer it with an acknowledged send (marker `probed`),
    /// then block until `release` (marker `unblocked`).
    Block = 0xC0DE_0001,
    /// Submit 64 asynchronous plans and a 65th (marker `plans`).
    Plans = 0xC0DE_0002,
    /// Submit one more asynchronous plan (marker `more`).
    More = 0xC0DE_0003,
    /// Requests core refuses: a scan and a plan naming handles never
    /// issued (marker `refused`).
    Refused = 0xC0DE_0004,
    /// Two concurrent rendezvous (markers `overlap-a`, `overlap-b`).
    OverlapA = 0xC0DE_0010,
    OverlapB = 0xC0DE_0011,
}

const SOURCE: &str = r#"
#include <stddef.h>
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
static void nap(unsigned ms) { Sleep(ms); }
static SRWLOCK LOG_LOCK = SRWLOCK_INIT;
static void log_lock(void) { AcquireSRWLockExclusive(&LOG_LOCK); }
static void log_unlock(void) { ReleaseSRWLockExclusive(&LOG_LOCK); }
typedef DWORD (WINAPI *thread_fn)(LPVOID);
static DWORD WINAPI background_entry(LPVOID arg);
static int start_background(void *arg) {
    HANDLE thread = CreateThread(NULL, 0, background_entry, arg, 0, NULL);
    if (!thread) return 0;
    CloseHandle(thread);
    return 1;
}
#else
#include <pthread.h>
#include <unistd.h>
#define FIXTURE_EXPORT __attribute__((visibility("default")))
static unsigned long fixture_pid(void) { return (unsigned long)getpid(); }
static void nap(unsigned ms) { usleep(ms * 1000u); }
static pthread_mutex_t LOG_LOCK = PTHREAD_MUTEX_INITIALIZER;
static void log_lock(void) { pthread_mutex_lock(&LOG_LOCK); }
static void log_unlock(void) { pthread_mutex_unlock(&LOG_LOCK); }
static void *background_entry(void *arg);
static int start_background(void *arg) {
    pthread_t thread;
    if (pthread_create(&thread, NULL, background_entry, arg) != 0) return 0;
    pthread_detach(thread);
    return 1;
}
#endif

static void path_of(char *path, size_t size, const char *name, int with_pid) {
    if (with_pid) snprintf(path, size, "%s/%s-%lu", FIXTURE_DIR, name, fixture_pid());
    else snprintf(path, size, "%s/%s", FIXTURE_DIR, name);
}

static void marker(const char *probe, const char *text) {
    char path[4096];
    char partial[4096];
    path_of(path, sizeof path, probe, 1);
    snprintf(partial, sizeof partial, "%s.partial", path);
    FILE *file = fopen(partial, "wb");
    if (!file) return;
    fputs(text, file);
    fclose(file);
    rename(partial, path);
}

static int exists(const char *name) {
    char path[4096];
    path_of(path, sizeof path, name, 0);
    FILE *file = fopen(path, "rb");
    if (!file) return 0;
    fclose(file);
    return 1;
}

/* Waits up to 60 s for the test to create `name`. */
static int wait_file(const char *name) {
    for (int i = 0; i < 6000; i++) {
        if (exists(name)) return 1;
        nap(10);
    }
    return 0;
}

static void log_message(const vosu_message *message) {
    char path[4096];
    path_of(path, sizeof path, "log", 1);
    log_lock();
    FILE *file = fopen(path, "ab");
    if (file) {
        fprintf(file, "%u ", message->opcode);
        for (uint32_t i = 0; i < message->payload.len; i++) fprintf(file, "%02x", message->payload.data[i]);
        fputc('\n', file);
        fclose(file);
    }
    log_unlock();
}

typedef struct fixture {
    const vosu_host_services *host;
    void *host_ctx;
    uint64_t handle;
    int32_t overlap;
} fixture;

#ifdef _WIN32
static __declspec(thread) int tl_waiting;
static __declspec(thread) int tl_got;
static __declspec(thread) uint64_t tl_value;
#else
static __thread int tl_waiting;
static __thread int tl_got;
static __thread uint64_t tl_value;
#endif

static uint32_t send_raw(fixture *self, uint32_t opcode, uint32_t flags, uint64_t correlation, const void *data, uint32_t len) {
    vosu_message message;
    message.opcode = opcode;
    message.flags = flags;
    message.correlation_id = correlation;
    message.payload.data = data;
    message.payload.len = len;
    return (uint32_t)self->host->send(self->host_ctx, &message);
}

typedef struct plan_payload {
    vosu_read_plan_submit_header header;
    vosu_stage_header stage;
    vosu_plan_op op;
} plan_payload;

static uint32_t submit_plan(fixture *self, uint64_t plan, uint64_t handle, uint32_t flags) {
    plan_payload p;
    memset(&p, 0, sizeof p);
    p.header.plan = plan;
    p.header.handle = handle;
    p.header.root.kind = VOSU_BASE_ABSOLUTE;
    p.header.root.address = 0x1000;
    p.header.stages_offset = (uint32_t)offsetof(plan_payload, stage);
    p.header.stage_count = 1;
    p.stage.base.kind = VOSU_STAGE_BASE_ROOT;
    p.stage.ops_offset = (uint32_t)offsetof(plan_payload, op);
    p.stage.op_count = 1;
    p.op.offset = 0;
    p.op.length = 8;
    return send_raw(self, VOSU_OP_READ_PLAN_SUBMIT, flags, 0, &p, sizeof p);
}

/* The unsigned integer after a CBOR text key `key`, or 0. */
static uint64_t cbor_uint_after(const uint8_t *data, uint32_t len, const char *key) {
    size_t key_len = strlen(key);
    for (uint32_t i = 0; i + 1 + key_len < len; i++) {
        if (data[i] != (uint8_t)(0x60 + key_len) || memcmp(data + i + 1, key, key_len) != 0) continue;
        uint32_t at = i + 1 + (uint32_t)key_len;
        uint8_t head = data[at];
        if (head < 0x18) return head;
        uint32_t width = head == 0x18 ? 1 : head == 0x19 ? 2 : head == 0x1a ? 4 : head == 0x1b ? 8 : 0;
        if (!width || at + width >= len) return 0;
        uint64_t value = 0;
        for (uint32_t b = 0; b < width; b++) value = (value << 8) | data[at + 1 + b];
        return value;
    }
    return 0;
}

static fixture *INSTANCE;

#ifdef _WIN32
static DWORD WINAPI background_entry(LPVOID arg)
#else
static void *background_entry(void *arg)
#endif
{
    fixture *self = arg;
    tl_waiting = 1;
    uint32_t status = submit_plan(self, 1, __atomic_load_n(&self->handle, __ATOMIC_SEQ_CST), VOSU_READ_PLAN_SYNC);
    tl_waiting = 0;
    char text[256];
    snprintf(text, sizeof text, "status=%u same_thread=%d value=%016llx released=%d\n",
        status, tl_got, (unsigned long long)tl_value, exists("release"));
    marker("sync", text);
#ifdef _WIN32
    return 0;
#else
    return NULL;
#endif
}

static void *fixture_create(const vosu_host_services *host, void *host_ctx, vosu_bytes init) {
    (void)init;
    fixture *self = calloc(1, sizeof *self);
    if (!self) return NULL;
    self->host = host;
    self->host_ctx = host_ctx;
    INSTANCE = self;
    return self;
}

static void overlap(fixture *self, const char *name) {
    int32_t arrived = __atomic_add_fetch(&self->overlap, 1, __ATOMIC_SEQ_CST);
    for (int i = 0; arrived < 2 && i < 5000; i++) {
        nap(1);
        arrived = __atomic_load_n(&self->overlap, __ATOMIC_SEQ_CST);
    }
    marker(name, arrived >= 2 ? "1\n" : "0\n");
}

/* {"handle": 12345}: never issued to this instance. */
static const uint8_t DECLINE[] = {0xa1, 0x66, 'h','a','n','d','l','e', 0x19, 0x30, 0x39};
/* {"session": 12345, "module": null, "pattern": "AA", "non_zero_mask": false, "all_matches": false} */
static const uint8_t SCAN[] = {0xa5,
    0x67, 's','e','s','s','i','o','n', 0x19, 0x30, 0x39,
    0x66, 'm','o','d','u','l','e', 0xf6,
    0x67, 'p','a','t','t','e','r','n', 0x62, 'A','A',
    0x6d, 'n','o','n','_','z','e','r','o','_','m','a','s','k', 0xf4,
    0x6b, 'a','l','l','_','m','a','t','c','h','e','s', 0xf4};

static void command(fixture *self, uint64_t which) {
    char text[256];
    switch (which) {
    case 0xC0DE0001u: {
        uint32_t on_dispatch = submit_plan(self, 2, self->handle, VOSU_READ_PLAN_SYNC);
        snprintf(text, sizeof text, "status=%u\n", on_dispatch);
        marker("dispatch_sync", text);
        if (!start_background(self)) marker("sync", "status=thread\n");
        if (wait_file("probe")) {
            uint32_t probe = send_raw(self, VOSU_OP_SESSION_DECLINE, 0, 0, DECLINE, sizeof DECLINE);
            snprintf(text, sizeof text, "status=%u\n", probe);
            marker("probed", text);
        }
        wait_file("release");
        marker("unblocked", "1\n");
        break;
    }
    case 0xC0DE0002u: {
        unsigned ok = 0;
        for (uint64_t plan = 100; plan < 164; plan++) {
            if (submit_plan(self, plan, self->handle, 0) == VOSU_OK) ok++;
        }
        uint32_t extra = submit_plan(self, 164, self->handle, 0);
        snprintf(text, sizeof text, "ok=%u extra=%u\n", ok, extra);
        marker("plans", text);
        break;
    }
    case 0xC0DE0003u:
        snprintf(text, sizeof text, "status=%u\n", submit_plan(self, 200, self->handle, 0));
        marker("more", text);
        break;
    case 0xC0DE0004u: {
        uint32_t scan = send_raw(self, VOSU_OP_SCAN_REQUEST, 0, 7, SCAN, sizeof SCAN);
        uint32_t plan = submit_plan(self, 300, 54321, 0);
        snprintf(text, sizeof text, "scan=%u plan=%u\n", scan, plan);
        marker("refused", text);
        break;
    }
    case 0xC0DE0010u: overlap(self, "overlap-a"); break;
    case 0xC0DE0011u: overlap(self, "overlap-b"); break;
    default: break;
    }
}

static vosu_status fixture_dispatch(void *ctx, const vosu_message *message) {
    fixture *self = ctx;
    if (message->opcode == VOSU_OP_READ_PLAN_RESULT && tl_waiting) {
        const uint8_t *data = message->payload.data;
        vosu_read_plan_result_header header;
        vosu_read_result_flat result;
        if (message->payload.len >= sizeof header) {
            memcpy(&header, data, sizeof header);
            if (header.result_count == 1 && header.results_offset + sizeof result <= message->payload.len) {
                memcpy(&result, data + header.results_offset, sizeof result);
                if (result.kind == VOSU_READ_OK && result.bytes_len == 8
                    && result.bytes_offset + 8 <= message->payload.len) {
                    memcpy(&tl_value, data + result.bytes_offset, 8);
                }
            }
        }
        tl_got = ((uintptr_t)data % 8 == 0) ? 1 : 2;
        return VOSU_OK;
    }
    if (message->opcode == VOSU_OP_DEMAND_CHANGED && message->payload.len >= 8) {
        uint64_t which;
        memcpy(&which, message->payload.data, 8);
        if ((which & 0xFFFFFF00u) == 0xC0DE0000u) { command(self, which); return VOSU_OK; }
    }
    if (message->opcode == VOSU_OP_SESSION_ATTACHED) {
        uint64_t handle = cbor_uint_after(message->payload.data, message->payload.len, "handle");
        __atomic_store_n(&self->handle, handle, __ATOMIC_SEQ_CST);
        char text[64];
        snprintf(text, sizeof text, "%llu\n", (unsigned long long)handle);
        marker("attached", text);
    }
    log_message(message);
    return VOSU_OK;
}

static void fixture_destroy(void *ctx) {
    (void)ctx;
    marker("destroy", "ok\n");
}

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
        .expect("a C compiler ($CC, cc, clang or gcc) is required to build the T1 fixture")
}

/// The library file name for this target.
pub fn library_name() -> &'static str {
    if cfg!(windows) {
        "t1_fixture.dll"
    } else {
        "libt1_fixture.so"
    }
}

/// Builds the fixture in `build_dir`, writing markers into `markers`.
/// Returns the library's bytes.
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
        "{compiler} failed to build the T1 fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&output_path).unwrap()
}

/// Waits up to `bound` for the one `<probe>-<pid>` marker, returning its
/// pid and contents.
pub fn wait_marker(dir: &Path, probe: &str, bound: Duration) -> Option<(u32, String)> {
    let prefix = format!("{probe}-");
    let deadline = Instant::now() + bound;
    loop {
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if let Some(pid) = name
                .strip_prefix(&prefix)
                .and_then(|pid| pid.parse::<u32>().ok())
            {
                return Some((pid, std::fs::read_to_string(dir.join(&name)).unwrap()));
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The `key=value` pairs of a marker line.
pub fn field(text: &str, key: &str) -> String {
    text.split_whitespace()
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("{key} missing from {text:?}"))
        .to_owned()
}

/// Every logged `(opcode, payload)` of the child `pid`, in dispatch order;
/// a line still being written is left out.
pub fn log(dir: &Path, pid: u32) -> Vec<(u32, Vec<u8>)> {
    let Ok(text) = std::fs::read_to_string(dir.join(format!("log-{pid}"))) else {
        return Vec::new();
    };
    text.split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n'))
        .map(|line| {
            let (opcode, hex) = line.split_once(' ').unwrap();
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
                .collect();
            (opcode.parse().unwrap(), bytes)
        })
        .collect()
}

/// Creates the barrier file `name` the fixture waits on.
pub fn open_barrier(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), b"1").unwrap();
}

/// A scratch path helper so callers keep all fixture output under one dir.
pub fn dir(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

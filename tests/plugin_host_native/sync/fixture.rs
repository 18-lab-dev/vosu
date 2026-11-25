//! X0's C fixture plugin for the `sync` and `backpressure` cases, compiled
//! per test against `include/vosu_plugin.h` with its marker directory baked
//! in, so no state is shared between tests and nothing depends on
//! environment variables (the T1 relay fixture's convention). The same
//! library runs under either runtime, so every case can compare a
//! `vosu plugin-host` child with the in-process runtime.
//!
//! Core drives it with `DEMAND_CHANGED` frames whose `handle` is a
//! [`Command`]. Markers are files `<probe>-<pid>`; barriers are files the
//! test creates (`go`, `release`, `retry`). Every other message the
//! fixture's ordinary dispatch receives is appended to `log-<pid>` as
//! `<opcode> <payload hex>`; a `READ_PLAN_RESULT` dispatched on no waiting
//! submitter is also counted in the `stray` marker.
//!
//! Thread identities are the OS thread ids (`gettid`, `GetCurrentThreadId`).
//! Session memory: 8 bytes at `0x1000` answer at once, 8 bytes at `0x2000`
//! answer only once the test opens its read gate.

use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::{Duration, Instant};

/// The commands, as `DEMAND_CHANGED` handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Command {
    /// On the dispatch thread: a SYNC plan there (marker `dispatch`, with
    /// its tid and status), a background submitter started, then blocked
    /// until `release` (marker `unblocked`). The submitter waits for `go`,
    /// submits a SYNC plan at `0x1000` and, inside that result's dispatch on
    /// its own thread, a nested SYNC plan (marker `sync`).
    Hold = 0xC0DE_0101,
    /// A background submitter: a SYNC plan at `0x2000` (marker `late`, with
    /// its status and elapsed milliseconds), then after `retry` a SYNC plan
    /// at `0x1000` (marker `retry`).
    Late = 0xC0DE_0102,
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
static unsigned long fixture_tid(void) { return (unsigned long)GetCurrentThreadId(); }
static unsigned long long now_ms(void) { return (unsigned long long)GetTickCount64(); }
static void nap(unsigned ms) { Sleep(ms); }
static SRWLOCK LOG_LOCK = SRWLOCK_INIT;
static void log_lock(void) { AcquireSRWLockExclusive(&LOG_LOCK); }
static void log_unlock(void) { ReleaseSRWLockExclusive(&LOG_LOCK); }
static DWORD WINAPI thread_entry(LPVOID arg);
static int start_thread(void *arg) {
    HANDLE thread = CreateThread(NULL, 0, thread_entry, arg, 0, NULL);
    if (!thread) return 0;
    CloseHandle(thread);
    return 1;
}
#define THREAD_LOCAL __declspec(thread)
#else
#include <pthread.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#define FIXTURE_EXPORT __attribute__((visibility("default")))
static unsigned long fixture_pid(void) { return (unsigned long)getpid(); }
static unsigned long fixture_tid(void) { return (unsigned long)syscall(SYS_gettid); }
static unsigned long long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (unsigned long long)ts.tv_sec * 1000ull + (unsigned long long)ts.tv_nsec / 1000000ull;
}
static void nap(unsigned ms) { usleep(ms * 1000u); }
static pthread_mutex_t LOG_LOCK = PTHREAD_MUTEX_INITIALIZER;
static void log_lock(void) { pthread_mutex_lock(&LOG_LOCK); }
static void log_unlock(void) { pthread_mutex_unlock(&LOG_LOCK); }
static void *thread_entry(void *arg);
static int start_thread(void *arg) {
    pthread_t thread;
    if (pthread_create(&thread, NULL, thread_entry, arg) != 0) return 0;
    pthread_detach(thread);
    return 1;
}
#define THREAD_LOCAL __thread
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
    uint32_t strays;
} fixture;

/* The submitter's view of its own SYNC results: set only while this thread
 * waits inside `send`, read back after it returns. */
typedef struct waiter {
    int depth;
    int got;
    int aligned;
    unsigned long result_tid;
    uint64_t value;
    uint64_t plan;
} waiter;

static THREAD_LOCAL waiter *tl_waiter;

static uint32_t send_raw(fixture *self, uint32_t opcode, uint32_t flags, const void *data, uint32_t len) {
    vosu_message message;
    message.opcode = opcode;
    message.flags = flags;
    message.correlation_id = 0;
    message.payload.data = data;
    message.payload.len = len;
    return (uint32_t)self->host->send(self->host_ctx, &message);
}

typedef struct plan_payload {
    vosu_read_plan_submit_header header;
    vosu_stage_header stage;
    vosu_plan_op op;
} plan_payload;

static uint32_t submit_sync(fixture *self, uint64_t plan, uint64_t address) {
    plan_payload p;
    memset(&p, 0, sizeof p);
    p.header.plan = plan;
    p.header.handle = __atomic_load_n(&self->handle, __ATOMIC_SEQ_CST);
    p.header.root.kind = VOSU_BASE_ABSOLUTE;
    p.header.root.address = address;
    p.header.stages_offset = (uint32_t)offsetof(plan_payload, stage);
    p.header.stage_count = 1;
    p.stage.base.kind = VOSU_STAGE_BASE_ROOT;
    p.stage.ops_offset = (uint32_t)offsetof(plan_payload, op);
    p.stage.op_count = 1;
    p.op.offset = 0;
    p.op.length = 8;
    return send_raw(self, VOSU_OP_READ_PLAN_SUBMIT, VOSU_READ_PLAN_SYNC, &p, sizeof p);
}

typedef struct two_stage_payload {
    vosu_read_plan_submit_header header;
    vosu_stage_header stages[2];
    vosu_plan_op ops[2];
} two_stage_payload;

/* Two root-based stages, each reading 8 bytes at `address`: the in-process
 * runtime checks the budget between stages, so a first stage held past it
 * times the whole call out in both runtimes. */
static uint32_t submit_two_stage_sync(fixture *self, uint64_t plan, uint64_t address) {
    two_stage_payload p;
    memset(&p, 0, sizeof p);
    p.header.plan = plan;
    p.header.handle = __atomic_load_n(&self->handle, __ATOMIC_SEQ_CST);
    p.header.root.kind = VOSU_BASE_ABSOLUTE;
    p.header.root.address = address;
    p.header.stages_offset = (uint32_t)offsetof(two_stage_payload, stages);
    p.header.stage_count = 2;
    for (uint32_t i = 0; i < 2; i++) {
        p.stages[i].base.kind = VOSU_STAGE_BASE_ROOT;
        p.stages[i].ops_offset = (uint32_t)(offsetof(two_stage_payload, ops) + i * sizeof(vosu_plan_op));
        p.stages[i].op_count = 1;
        p.ops[i].offset = 0;
        p.ops[i].length = 8;
    }
    return send_raw(self, VOSU_OP_READ_PLAN_SUBMIT, VOSU_READ_PLAN_SYNC, &p, sizeof p);
}

static fixture *INSTANCE;

typedef struct job {
    fixture *self;
    uint64_t which;
} job;

static void hold_submitter(fixture *self) {
    char text[512];
    if (!wait_file("go")) { marker("sync", "status=go_timeout\n"); return; }
    waiter outer;
    memset(&outer, 0, sizeof outer);
    outer.depth = 1;
    unsigned long submit_tid = fixture_tid();
    tl_waiter = &outer;
    uint32_t status = submit_sync(self, 11, 0x1000);
    tl_waiter = NULL;
    /* `released` is whether the test had already let the blocked dispatch
     * go when this SYNC returned; the nested read writes `nested`. */
    snprintf(text, sizeof text,
        "status=%u submit_tid=%lu got=%d result_tid=%lu aligned=%d value=%016llx plan=%llu released=%d\n",
        status, submit_tid, outer.got, outer.result_tid, outer.aligned,
        (unsigned long long)outer.value, (unsigned long long)outer.plan, exists("release"));
    marker("sync", text);
}

static void late_submitter(fixture *self) {
    char text[512];
    waiter late;
    memset(&late, 0, sizeof late);
    late.depth = 2;
    tl_waiter = &late;
    unsigned long long started = now_ms();
    uint32_t status = submit_two_stage_sync(self, 21, 0x2000);
    unsigned long long elapsed = now_ms() - started;
    tl_waiter = NULL;
    snprintf(text, sizeof text, "status=%u elapsed_ms=%llu got=%d\n", status, elapsed, late.got);
    marker("late", text);
    if (!wait_file("retry")) return;
    waiter retry;
    memset(&retry, 0, sizeof retry);
    retry.depth = 2;
    unsigned long submit_tid = fixture_tid();
    tl_waiter = &retry;
    status = submit_sync(self, 22, 0x1000);
    tl_waiter = NULL;
    snprintf(text, sizeof text,
        "status=%u got=%d submit_tid=%lu result_tid=%lu value=%016llx plan=%llu\n",
        status, retry.got, submit_tid, retry.result_tid,
        (unsigned long long)retry.value, (unsigned long long)retry.plan);
    marker("retry", text);
}

#ifdef _WIN32
static DWORD WINAPI thread_entry(LPVOID arg)
#else
static void *thread_entry(void *arg)
#endif
{
    job *work = arg;
    if (work->which == 0xC0DE0101u) hold_submitter(work->self);
    else late_submitter(work->self);
    free(work);
#ifdef _WIN32
    return 0;
#else
    return NULL;
#endif
}

static int start(fixture *self, uint64_t which) {
    job *work = malloc(sizeof *work);
    if (!work) return 0;
    work->self = self;
    work->which = which;
    if (start_thread(work)) return 1;
    free(work);
    return 0;
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

static void read_result(const vosu_message *message, uint64_t *value, uint64_t *plan) {
    vosu_read_plan_result_header header;
    vosu_read_result_flat result;
    const uint8_t *data = message->payload.data;
    if (message->payload.len < sizeof header) return;
    memcpy(&header, data, sizeof header);
    *plan = header.plan;
    if (header.result_count != 1 || header.results_offset + sizeof result > message->payload.len) return;
    memcpy(&result, data + header.results_offset, sizeof result);
    if (result.kind == VOSU_READ_OK && result.bytes_len == 8 && result.bytes_offset + 8 <= message->payload.len) {
        memcpy(value, data + result.bytes_offset, 8);
    }
}

static vosu_status fixture_dispatch(void *ctx, const vosu_message *message) {
    fixture *self = ctx;
    char text[512];
    if (message->opcode == VOSU_OP_READ_PLAN_RESULT) {
        waiter *current = tl_waiter;
        if (!current) {
            /* Delivered to no waiting submitter: a stray (late) result. */
            uint64_t value = 0, plan = 0;
            read_result(message, &value, &plan);
            uint32_t strays = __atomic_add_fetch(&self->strays, 1, __ATOMIC_SEQ_CST);
            snprintf(text, sizeof text, "count=%u plan=%llu\n", strays, (unsigned long long)plan);
            marker("stray", text);
            return VOSU_OK;
        }
        current->got += 1;
        current->result_tid = fixture_tid();
        current->aligned = ((uintptr_t)message->payload.data % 8 == 0) ? 1 : 0;
        read_result(message, &current->value, &current->plan);
        if (current->depth == 1) {
            /* Re-entrancy: a nested SYNC from inside this result's dispatch
             * on the submitting thread, before the outer `send` returns. */
            waiter nested;
            memset(&nested, 0, sizeof nested);
            nested.depth = 2;
            tl_waiter = &nested;
            uint32_t status = submit_sync(self, 12, 0x1000);
            tl_waiter = current;
            snprintf(text, sizeof text,
                "status=%u got=%d result_tid=%lu value=%016llx plan=%llu\n",
                status, nested.got, nested.result_tid,
                (unsigned long long)nested.value, (unsigned long long)nested.plan);
            marker("nested", text);
        }
        return VOSU_OK;
    }
    if (message->opcode == VOSU_OP_DEMAND_CHANGED && message->payload.len >= 8) {
        uint64_t which;
        memcpy(&which, message->payload.data, 8);
        if (which == 0xC0DE0101u) {
            uint32_t on_dispatch = submit_sync(self, 10, 0x1000);
            snprintf(text, sizeof text, "tid=%lu sync_status=%u\n", fixture_tid(), on_dispatch);
            marker("dispatch", text);
            if (!start(self, which)) marker("sync", "status=thread\n");
            wait_file("release");
            marker("unblocked", "1\n");
            return VOSU_OK;
        }
        if (which == 0xC0DE0102u) {
            if (!start(self, which)) marker("late", "status=thread\n");
            return VOSU_OK;
        }
    }
    if (message->opcode == VOSU_OP_SESSION_ATTACHED) {
        uint64_t handle = cbor_uint_after(message->payload.data, message->payload.len, "handle");
        __atomic_store_n(&self->handle, handle, __ATOMIC_SEQ_CST);
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
        .expect("a C compiler ($CC, cc, clang or gcc) is required to build the X0 fixture")
}

/// The library file name for this target.
pub fn library_name() -> &'static str {
    if cfg!(windows) {
        "x0_sync_fixture.dll"
    } else {
        "libx0_sync_fixture.so"
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
        "{compiler} failed to build the X0 fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&output_path).unwrap()
}

/// The one `<probe>-<pid>` marker, if written.
pub fn marker(dir: &Path, probe: &str) -> Option<(u32, String)> {
    let prefix = format!("{probe}-");
    std::fs::read_dir(dir).unwrap().find_map(|entry| {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        let pid = name.strip_prefix(&prefix)?.parse::<u32>().ok()?;
        Some((pid, std::fs::read_to_string(dir.join(&name)).unwrap()))
    })
}

/// Waits up to `bound` for the `<probe>-<pid>` marker.
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

/// The value of `key` in a marker line.
pub fn field(text: &str, key: &str) -> String {
    text.split_whitespace()
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("{key} missing from {text:?}"))
        .to_owned()
}

/// Every logged `(opcode, payload)` of child `pid`, in dispatch order; a
/// line still being written is left out.
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

/// A fresh directory `name` under `root`.
pub fn dir(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

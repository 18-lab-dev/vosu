//! E0's focused C fixture plugin, compiled per test against
//! `include/vosu_plugin.h` with its behavior and its per-test marker
//! directory baked in, so no state is shared between tests and nothing
//! depends on environment variables of the test process.
//!
//! Markers are files `<probe>-<pid>` in the test's own directory:
//!
//! - `constructor`: written by the load-time constructor (`DllMain` on
//!   Windows). On Linux it holds the process's `/proc/self/status` as the
//!   constructor saw it, so it records the confinement before any plugin
//!   code beyond the constructor ran.
//! - `create`, `destroy`: written when each runs.
//! - `dispatch`: written by the first `dispatch`, holding the status each
//!   probe `send` returned (see [`SENDS`]).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// What the fixture does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal = 0,
    /// `create` returns NULL.
    CreateRejected = 1,
    /// `create` never returns.
    CreateHangs = 2,
    /// `destroy` never returns.
    DestroyHangs = 3,
    /// The vtable claims ABI major 2.
    WrongAbi = 4,
    /// No `vosu_plugin_entry_v1` export.
    NoEntry = 5,
}

/// The probe sends the first `dispatch` makes, in marker order.
pub const SENDS: [&str; 8] = [
    "timer", "decline", "private", "unknown", "flags", "reverse", "log", "oversize",
];

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
static void pause_forever(void) { for (;;) Sleep(1000); }
#else
#include <unistd.h>
#define FIXTURE_EXPORT __attribute__((visibility("default")))
static unsigned long fixture_pid(void) { return (unsigned long)getpid(); }
static void pause_forever(void) { for (;;) sleep(1); }
#endif

static void marker(const char *probe, const char *text) {
    char path[4096];
    snprintf(path, sizeof path, "%s/%s-%lu", FIXTURE_DIR, probe, fixture_pid());
    FILE *file = fopen(path, "wb");
    if (file) { fputs(text, file); fclose(file); }
}

#ifdef _WIN32
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance; (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) marker("constructor", "loaded\n");
    return TRUE;
}
#else
__attribute__((constructor)) static void fixture_constructor(void) {
    static char status[16384];
    size_t used = 0;
    FILE *file = fopen("/proc/self/status", "rb");
    if (file) { used = fread(status, 1, sizeof status - 1, file); fclose(file); }
    status[used] = 0;
    marker("constructor", status);
}
#endif

typedef struct fixture { const vosu_host_services *host; void *host_ctx; int dispatched; } fixture;

static uint32_t send_raw(fixture *self, uint32_t opcode, uint32_t flags, const uint8_t *data, uint32_t len) {
    vosu_message message;
    message.opcode = opcode;
    message.flags = flags;
    message.correlation_id = 0;
    message.payload.data = data;
    message.payload.len = len;
    return (uint32_t)self->host->send(self->host_ctx, &message);
}

/* {"timer": 1, "interval_ms": 1000} */
static const uint8_t TIMER_SET[] = {0xa2, 0x65, 't','i','m','e','r', 0x01,
    0x6b, 'i','n','t','e','r','v','a','l','_','m','s', 0x19, 0x03, 0xe8};
/* {"handle": 12345}: a session this instance was never offered. */
static const uint8_t DECLINE[] = {0xa1, 0x66, 'h','a','n','d','l','e', 0x19, 0x30, 0x39};
/* {"handle": 7, "focused": true} */
static const uint8_t FOCUS[] = {0xa2, 0x66, 'h','a','n','d','l','e', 0x07,
    0x67, 'f','o','c','u','s','e','d', 0xf5};
/* {"level": "info", "message": "e0", "fields": {}} */
static const uint8_t LOG[] = {0xa3, 0x65, 'l','e','v','e','l', 0x64, 'i','n','f','o',
    0x67, 'm','e','s','s','a','g','e', 0x62, 'e','0', 0x66, 'f','i','e','l','d','s', 0xa0};

static void *fixture_create(const vosu_host_services *host, void *host_ctx, vosu_bytes init) {
    (void)init;
    if (FIXTURE_MODE == 1) { marker("create", "null\n"); return NULL; }
    if (FIXTURE_MODE == 2) { marker("create", "hang\n"); pause_forever(); }
    fixture *self = calloc(1, sizeof *self);
    if (!self) return NULL;
    self->host = host;
    self->host_ctx = host_ctx;
    marker("create", "ok\n");
    return self;
}

static vosu_status fixture_dispatch(void *ctx, const vosu_message *message) {
    fixture *self = ctx;
    if (self->dispatched++) return VOSU_OK;
    uint32_t s[8];
    s[0] = send_raw(self, VOSU_OP_TIMER_SET, 0, TIMER_SET, sizeof TIMER_SET);
    s[1] = send_raw(self, VOSU_OP_SESSION_DECLINE, 0, DECLINE, sizeof DECLINE);
    s[2] = send_raw(self, VOSU_OPCODE_PRIVATE_BIT | 1u, 0, NULL, 0);
    s[3] = send_raw(self, 999u, 0, NULL, 0);
    s[4] = send_raw(self, VOSU_OP_TIMER_SET, 1u, TIMER_SET, sizeof TIMER_SET);
    s[5] = send_raw(self, VOSU_OP_FOCUS_CHANGED, 0, FOCUS, sizeof FOCUS);
    s[6] = send_raw(self, VOSU_OP_LOG_RECORD, 0, LOG, sizeof LOG);
    s[7] = send_raw(self, VOSU_OP_LOG_RECORD, 0, LOG, VOSU_FRAME_MAX_BYTES + 1u);
    char text[512];
    snprintf(text, sizeof text,
        "timer=%u decline=%u private=%u unknown=%u flags=%u reverse=%u log=%u oversize=%u opcode=%u\n",
        s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7], message->opcode);
    marker("dispatch", text);
    return VOSU_OK;
}

static void fixture_destroy(void *ctx) {
    marker("destroy", "ok\n");
    if (FIXTURE_MODE == 3) pause_forever();
    free(ctx);
}

static const vosu_plugin_vtable VTABLE = {
    sizeof(vosu_plugin_vtable),
    FIXTURE_MODE == 4 ? 2u : VOSU_PLUGIN_ABI_MAJOR,
    fixture_create,
    fixture_dispatch,
    fixture_destroy,
};

#if FIXTURE_MODE == 5
FIXTURE_EXPORT const vosu_plugin_vtable *fixture_entry_without_abi_name(void) { return &VTABLE; }
#else
FIXTURE_EXPORT const vosu_plugin_vtable *vosu_plugin_entry_v1(void) { return &VTABLE; }
#endif
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
            Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .expect("a C compiler ($CC, cc, clang or gcc) is required to build the E0 fixture")
}

/// The library file name for this target.
pub fn library_name() -> &'static str {
    if cfg!(windows) {
        "e0_fixture.dll"
    } else {
        "libe0_fixture.so"
    }
}

/// Builds the fixture in `build_dir` with `mode`, writing markers into
/// `markers`. Returns the library's bytes.
pub fn build(build_dir: &Path, markers: &Path, mode: Mode) -> Vec<u8> {
    let escaped = markers
        .to_str()
        .expect("UTF-8 temp path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    std::fs::write(
        build_dir.join("fixture_config.h"),
        format!(
            "#define FIXTURE_DIR \"{escaped}\"\n#define FIXTURE_MODE {}\n",
            mode as u32
        ),
    )
    .unwrap();
    let source = build_dir.join("fixture.c");
    std::fs::write(&source, SOURCE).unwrap();
    let output_path = build_dir.join(library_name());
    let include = Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
    let compiler = compiler();
    let mut command = Command::new(&compiler);
    command.arg("-shared");
    if !cfg!(windows) {
        command.arg("-fPIC");
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
        "{compiler} failed to build the E0 fixture:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(&output_path).unwrap()
}

/// The markers named `<probe>-<pid>` in `dir`, as (pid, contents).
pub fn markers(dir: &Path, probe: &str) -> Vec<(u32, String)> {
    let prefix = format!("{probe}-");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(pid) = name.strip_prefix(&prefix).and_then(|pid| pid.parse().ok()) {
            found.push((pid, std::fs::read_to_string(entry.path()).unwrap()));
        }
    }
    found
}

/// Waits up to `bound` for exactly one `<probe>-<pid>` marker with nonempty
/// contents.
pub fn wait_marker(dir: &Path, probe: &str, bound: Duration) -> Option<(u32, String)> {
    let deadline = Instant::now() + bound;
    loop {
        let found = markers(dir, probe);
        if let [(pid, text)] = found.as_slice()
            && !text.is_empty()
        {
            return Some((*pid, text.clone()));
        }
        assert!(found.len() <= 1, "more than one {probe} marker: {found:?}");
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The `key=value` pairs of a marker line.
pub fn fields(text: &str) -> Vec<(String, String)> {
    text.split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// A scratch path helper so callers keep all fixture output under one dir.
pub fn dir(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    path
}

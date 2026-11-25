//! Real target processes for the broker cases: native x64 and x86
//! executables, compiled per test, each holding a pointer chain
//! `root[0] → mid[0] → tag` of its own pointer width and 16 tag bytes
//! derived from its seed, so two targets never hold the same bytes.
//!
//! Each prints `BASE <root> MID <mid> TAG <tag>` (hex) and waits until its standard
//! input closes. Linux x86 needs no 32-bit C library: the Linux targets are
//! freestanding static executables that use raw system calls. Windows
//! targets are ordinary console programs built by Clang for each
//! architecture. A missing compiler or architecture fails the test.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

/// A target's pointer width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    X64,
    X86,
}

impl Width {
    pub fn bytes(self) -> u32 {
        match self {
            Self::X64 => 8,
            Self::X86 => 4,
        }
    }

    pub fn bitness(self) -> vosu_plugin_api::Bitness {
        match self {
            Self::X64 => vosu_plugin_api::Bitness::X64,
            Self::X86 => vosu_plugin_api::Bitness::X86,
        }
    }

    pub fn process_bitness(self) -> vosu_plugin_api::memory::ProcessBitness {
        match self {
            Self::X64 => vosu_plugin_api::memory::ProcessBitness::X64,
            Self::X86 => vosu_plugin_api::memory::ProcessBitness::X86,
        }
    }
}

#[cfg(target_os = "linux")]
const SOURCE: &str = r#"
typedef __UINTPTR_TYPE__ uptr;
#ifdef __x86_64__
static long sys3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile ("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}
#define SYS_READ 0
#define SYS_WRITE 1
#define SYS_EXIT 60
#else
static long sys3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile ("int $0x80" : "=a"(r) : "a"(n), "b"(a), "c"(b), "d"(c) : "memory");
    return r;
}
#define SYS_READ 3
#define SYS_WRITE 4
#define SYS_EXIT 1
#endif
static volatile uptr root[2];
static volatile uptr mid[2];
static volatile unsigned char tag[16];
static int put_hex(char *line, int n, uptr value) {
    for (int shift = (int)sizeof(uptr) * 8 - 4; shift >= 0; shift -= 4)
        line[n++] = "0123456789abcdef"[(value >> shift) & 15];
    return n;
}
__attribute__((force_align_arg_pointer, noreturn)) void _start(void) {
    for (int i = 0; i < 16; i++) tag[i] = (unsigned char)(SEED * 31 + i * 7 + 1);
    mid[0] = (uptr)tag;
    root[0] = (uptr)mid;
    char line[80];
    int n = 0;
    line[n++] = 'B'; line[n++] = 'A'; line[n++] = 'S'; line[n++] = 'E'; line[n++] = ' ';
    n = put_hex(line, n, (uptr)root);
    line[n++] = ' '; line[n++] = 'M'; line[n++] = 'I'; line[n++] = 'D'; line[n++] = ' ';
    n = put_hex(line, n, (uptr)mid);
    line[n++] = ' '; line[n++] = 'T'; line[n++] = 'A'; line[n++] = 'G'; line[n++] = ' ';
    n = put_hex(line, n, (uptr)tag);
    line[n++] = '\n';
    sys3(SYS_WRITE, 1, (long)line, n);
    char c;
    while (sys3(SYS_READ, 0, (long)&c, 1) > 0) {}
    sys3(SYS_EXIT, 0, 0, 0);
    for (;;) {}
}
"#;

#[cfg(windows)]
const SOURCE: &str = r#"
#include <stdint.h>
#include <stdio.h>
static volatile uintptr_t root[2];
static volatile uintptr_t mid[2];
static volatile unsigned char tag[16];
int main(void) {
    for (int i = 0; i < 16; i++) tag[i] = (unsigned char)(SEED * 31 + i * 7 + 1);
    mid[0] = (uintptr_t)tag;
    root[0] = (uintptr_t)mid;
    printf("BASE %llx MID %llx TAG %llx\n", (unsigned long long)(uintptr_t)root,
        (unsigned long long)(uintptr_t)mid, (unsigned long long)(uintptr_t)tag);
    fflush(stdout);
    while (getchar() != EOF) {}
    return 0;
}
"#;

/// The tag bytes a target built with `seed` holds.
pub fn tag(seed: u8) -> [u8; 16] {
    std::array::from_fn(|i| (u32::from(seed) * 31 + i as u32 * 7 + 1) as u8)
}

fn compiler() -> String {
    let mut candidates = Vec::new();
    if let Ok(cc) = std::env::var("CC") {
        candidates.push(cc);
    }
    candidates.extend(["clang", "cc", "gcc"].map(str::to_owned));
    candidates
        .into_iter()
        .find(|candidate| {
            Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .expect("a C compiler ($CC, clang, cc or gcc) is required to build the broker targets")
}

/// Builds the `width` target with `seed` into `dir`.
fn build(dir: &Path, width: Width, seed: u8) -> std::path::PathBuf {
    let source = dir.join(format!("target-{seed}.c"));
    std::fs::write(&source, SOURCE).unwrap();
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let output = dir.join(format!("target-{seed}{suffix}"));
    let compiler = compiler();
    let mut command = Command::new(&compiler);
    command.arg(format!("-DSEED={seed}")).arg("-O0");
    if cfg!(windows) {
        command.arg(match width {
            Width::X64 => "--target=x86_64-pc-windows-msvc",
            Width::X86 => "--target=i686-pc-windows-msvc",
        });
    } else {
        command.args([
            "-nostdlib",
            "-static",
            "-ffreestanding",
            "-fno-builtin",
            "-fno-stack-protector",
            "-fno-pic",
            "-no-pie",
        ]);
        if width == Width::X86 {
            command.arg("-m32");
        }
    }
    let result = command
        .arg("-o")
        .arg(&output)
        .arg(&source)
        .output()
        .unwrap_or_else(|error| panic!("running {compiler}: {error}"));
    assert!(
        result.status.success(),
        "{compiler} failed to build the {width:?} broker target:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    output
}

/// One running target. Dropping it closes its input and reaps it.
pub struct Target {
    pub pid: u32,
    pub width: Width,
    pub seed: u8,
    /// The address of `root`, the chain's first pointer.
    pub root: u64,
    /// The address of `mid`, which `root[0]` holds.
    pub mid: u64,
    /// The address of the 16 tag bytes, which `mid[0]` holds.
    pub tag_address: u64,
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Target {
    /// Builds and starts a `width` target holding [`tag`]`(seed)`.
    pub fn start(dir: &Path, width: Width, seed: u8) -> Self {
        let program = build(dir, width, seed);
        let mut child = Command::new(&program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("starting {}: {error}", program.display()));
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let fields: Vec<&str> = line.split_whitespace().collect();
        let ["BASE", root, "MID", mid, "TAG", tag_address] = fields[..] else {
            panic!("unexpected target banner {line:?}");
        };
        let stdin = child.stdin.take();
        Self {
            pid: child.id(),
            width,
            seed,
            root: u64::from_str_radix(root, 16).unwrap(),
            mid: u64::from_str_radix(mid, 16).unwrap(),
            tag_address: u64::from_str_radix(tag_address, 16).unwrap(),
            child,
            stdin,
        }
    }

    pub fn tag(&self) -> [u8; 16] {
        tag(self.seed)
    }

    /// Ends the target and reaps it.
    pub fn stop(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.flush();
        }
        let status = self.child.wait().unwrap();
        assert!(status.success(), "target exited with {status}");
    }
}

impl std::fmt::Debug for Target {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?} target {}", self.width, self.pid)
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        if self.stdin.take().is_some() {
            let _ = self.child.wait();
        }
    }
}

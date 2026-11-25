//! The synthetic target process (`examples/brokered_reads_target.rs`): its
//! base address, its independently computed expected result hashes, and a
//! real native memory handle to it.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

use vosu_plugin_api::memory::ProcessMemory;

pub struct Target {
    pub child: Child,
    pub root: u64,
    pub process: Box<dyn ProcessMemory>,
    /// `(shape, sha256)` the target computed over its own memory, without
    /// any remote read or transport involved.
    expected: Vec<(String, String)>,
}

/// `CARGO_BIN_EXE_<name>` covers only `[[bin]]` targets; this bench's
/// executable sits at `target/<profile>/deps/<hash>`, one level below the
/// `examples` directory the helper is built into.
fn target_binary_path() -> std::path::PathBuf {
    let mut dir = std::env::current_exe()
        .expect("locate this bench's own executable")
        .parent()
        .expect("bench executable has a parent directory")
        .to_path_buf();
    dir.pop();
    dir.push("examples");
    dir.push(format!(
        "brokered_reads_target{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        dir.is_file(),
        "missing {dir:?}; run `cargo build --release --locked --example brokered_reads_target` first"
    );
    dir
}

impl Target {
    pub fn spawn() -> Self {
        let mut child = Command::new(target_binary_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn synthetic target process");
        let mut stdout = BufReader::new(child.stdout.take().expect("target stdout is piped"));
        let mut root = None;
        let mut expected = Vec::new();
        loop {
            let mut line = String::new();
            stdout.read_line(&mut line).expect("read target line");
            let line = line.trim_end();
            if line == "READY" {
                break;
            }
            let fields: Vec<&str> = line.split(' ').collect();
            match fields.as_slice() {
                ["BASE", hex] => {
                    root = Some(
                        u64::from_str_radix(hex.trim_start_matches("0x"), 16)
                            .expect("parse base address"),
                    );
                }
                ["EXPECT", shape, sha] => expected.push(((*shape).to_owned(), (*sha).to_owned())),
                _ => panic!("unexpected target line {line:?}"),
            }
        }
        let process = open(child.id());
        Self {
            child,
            root: root.expect("target reports BASE <hex>"),
            process,
            expected,
        }
    }

    pub fn expected_sha256(&self, shape: &str) -> &str {
        self.expected
            .iter()
            .find(|(name, _)| name == shape)
            .map(|(_, sha)| sha.as_str())
            .unwrap_or_else(|| panic!("target reported no EXPECT for {shape}"))
    }
}

#[cfg(target_os = "linux")]
fn open(pid: u32) -> Box<dyn ProcessMemory> {
    Box::new(vosu_sys::process::open_process(pid).expect("open target via process_vm_readv"))
}

#[cfg(windows)]
fn open(pid: u32) -> Box<dyn ProcessMemory> {
    Box::new(
        vosu_sys::process::WindowsProcessMemory::open(pid)
            .expect("open target via ReadProcessMemory"),
    )
}

impl Drop for Target {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

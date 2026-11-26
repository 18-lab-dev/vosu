//! Native target for short process-path smokes. No broker memory hook is installed.
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use crate::{Result, fail};

#[path = "../../benches/support/stable_fixture.rs"]
#[expect(
    dead_code,
    reason = "the shared fixture's capture assertions live in the plan benches"
)]
mod stable_fixture;

const CHILD: &str = "--process-fixture-target";

pub(super) struct Fixture {
    child: Child,
    pub base: usize,
}

impl Fixture {
    pub(super) fn start() -> Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg(CHILD)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let child = vosu_sys::process_spawn::spawn(&mut command)?;
        let mut owned = Self { child, base: 0 };
        let stdout = owned
            .child
            .stdout
            .take()
            .ok_or_else(|| fail("missing target stdout"))?;
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = tx.send(result);
        });
        let line = rx.recv_timeout(Duration::from_secs(10))??;
        owned.base = line
            .trim()
            .parse()
            .map_err(|error| fail(format!("fixture base {line:?}: {error}")))?;
        if owned.base == 0 {
            return Err(fail("fixture base is zero"));
        }
        Ok(owned)
    }

    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const BYTES: usize = 0x1_0000;

#[cfg(target_os = "linux")]
fn low_mapping() -> io::Result<*mut u8> {
    // SAFETY: allocate a private, writable guest of the x86 fixture's address space.
    // The (owned) child keeps the mapping until exit.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            BYTES,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        Err(io::Error::last_os_error())
    } else {
        Ok(ptr.cast())
    }
}

#[cfg(windows)]
fn low_mapping() -> io::Result<*mut u8> {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
    };
    for address in (0x1000_0000..0x7000_0000).step_by(0x0200_0000) {
        // SAFETY: request an aligned, unused range in this process only. A collision
        // fails the allocation; VirtualAlloc never replaces an existing range.
        let ptr = unsafe {
            VirtualAlloc(
                address as *const _,
                BYTES,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        if !ptr.is_null() {
            return Ok(ptr.cast());
        }
    }
    Err(io::Error::last_os_error())
}

pub(super) fn target() -> Result<()> {
    let ptr = low_mapping()?;
    let graph = stable_fixture::bytes_at(ptr as usize);
    assert_eq!(graph.len(), BYTES);
    // SAFETY: both allocations are disjoint, initialized/readable or writable for BYTES.
    unsafe { std::ptr::copy_nonoverlapping(graph.as_ptr(), ptr, BYTES) };
    println!("{}", ptr as usize);
    io::stdout().flush()?;
    io::stdin().read_exact(&mut [0]).or_else(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    Ok(())
}

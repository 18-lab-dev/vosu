//! The one way this process creates child processes
//! (`docs/v2/plugin-abi.md` §7.4).
//!
//! On Windows a plugin-host spawn briefly holds inheritable duplicates of
//! its ring objects, and any `CreateProcess` call with
//! `bInheritHandles = TRUE` and no handle list would inherit them. Rust's
//! `std::process::Command` (and so `tokio::process::Command`) passes exactly
//! that, under a lock private to the standard library. Every child-process
//! creation in the daemon therefore goes through this module, which holds
//! one process-wide gate around the creation itself; the plugin-host spawn
//! holds the same gate from creating its duplicates until it has closed
//! them. Waiting for a child happens outside the gate, so a long-running
//! child never delays a plugin-host spawn.
//!
//! `clippy.toml` disallows the direct `spawn`/`output`/`status` calls and
//! the `open` launchers that create processes internally, naming these
//! wrappers. On Linux, where every descriptor Rust opens is close-on-exec,
//! the gate is a passthrough and the wrappers only keep one convention.

use std::ffi::OsStr;
use std::io;
use std::process::{Child, Command, ExitStatus, Output, Stdio};

#[cfg(windows)]
static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Proof that this thread holds the creation gate.
#[must_use = "the gate is released when the guard drops"]
pub(crate) struct GateGuard {
    #[cfg(windows)]
    _guard: std::sync::MutexGuard<'static, ()>,
}

/// Takes the process-wide creation gate. Hold it only around the system
/// calls that create a process (and, for a plugin-host spawn, its
/// inheritable duplicates), never across a wait for a child.
pub(crate) fn hold() -> GateGuard {
    GateGuard {
        // The gate guards no data, so a panic while it was held leaves
        // nothing inconsistent.
        #[cfg(windows)]
        _guard: GATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    }
}

/// `open::that_detached` under the gate: opens `target` with its default
/// application without waiting for the launcher.
#[expect(
    clippy::disallowed_methods,
    reason = "the gated wrapper for open's detached launcher, which only spawns"
)]
pub fn open_detached(target: impl AsRef<OsStr>) -> io::Result<()> {
    let _gate = hold();
    open::that_detached(target)
}

/// `open::that`: tries each of `open`'s launchers for `target` in turn,
/// each spawned under the gate and awaited outside it, and succeeds on the
/// first that exits successfully.
pub fn open(target: impl AsRef<OsStr>) -> io::Result<()> {
    let mut last = None;
    for mut command in open::commands(target) {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match status(&mut command) {
            Ok(exit) if exit.success() => return Ok(()),
            Ok(exit) => {
                return Err(io::Error::other(format!(
                    "launcher {command:?} failed with {exit}"
                )));
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no launcher available")))
}

/// `Command::spawn` under the gate.
#[expect(
    clippy::disallowed_methods,
    reason = "the gated wrapper every other spawn site goes through"
)]
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    let _gate = hold();
    command.spawn()
}

/// `Command::status`: spawns under the gate (with `status`'s inherited
/// standard handles) and waits outside it.
pub fn status(command: &mut Command) -> io::Result<ExitStatus> {
    spawn(command)?.wait()
}

/// `Command::output`: spawns under the gate with `output`'s standard
/// handles (stdin null, stdout and stderr captured) and waits outside it.
/// A caller that needs other standard handles uses [`spawn`] and
/// `Child::wait_with_output`.
pub fn output(command: &mut Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn(command)?.wait_with_output()
}

/// `tokio::process::Command::spawn` under the gate. The gate is a short
/// synchronous critical section around `CreateProcess`, which tokio's own
/// `spawn` already performs synchronously on the calling thread.
#[expect(
    clippy::disallowed_methods,
    reason = "the gated wrapper every other spawn site goes through"
)]
pub fn spawn_tokio(command: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    let _gate = hold();
    command.spawn()
}

/// `tokio::process::Command::output`: spawns under the gate with `output`'s
/// standard handles and awaits the output outside it. Dropping the future
/// drops the child, so `kill_on_drop` behaves as with `output`.
pub async fn output_tokio(command: &mut tokio::process::Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn_tokio(command)?.wait_with_output().await
}

#[cfg(test)]
mod tests;

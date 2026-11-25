//! The §7.3 refusal control's environment (`docs/testing/verification.md`,
//! "N0 packaged Linux drop procedure" step 5): core runs under a wrapper
//! seccomp filter that refuses `seccomp(2)`, so every plugin host it starts
//! fails its step 3 with `security_unavailable`.
//!
//! The filter hands each `seccomp(2)` call to the test through a seccomp
//! user notification, and the test answers `EPERM`. While a call waits for
//! that answer its caller is alive and stopped in the syscall, so the test
//! opens a pidfd on every refused plugin host before refusing it: the
//! host's later end is read from that pidfd, never inferred from PID
//! absence, however quickly it exits.

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use super::tracked::{Tracked, argv1_of};

/// The descriptor the listener is placed on in core.
const LISTENER_FD: i32 = 900;
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
const SECCOMP_FILTER_FLAG_NEW_LISTENER: libc::c_ulong = 1 << 3;
/// `_IOWR('!', 0, struct seccomp_notif)` and `_IOWR('!', 1, struct
/// seccomp_notif_resp)`.
const SECCOMP_IOCTL_NOTIF_RECV: libc::c_ulong = 0xC050_2100;
const SECCOMP_IOCTL_NOTIF_SEND: libc::c_ulong = 0xC018_2101;

/// `struct seccomp_notif`.
#[repr(C)]
#[derive(Default)]
struct Notification {
    id: u64,
    pid: u32,
    _flags: u32,
    _nr: i32,
    _arch: u32,
    _instruction_pointer: u64,
    _args: [u64; 6],
}

/// `struct seccomp_notif_resp`.
#[repr(C)]
struct Response {
    _id: u64,
    _val: i64,
    _error: i32,
    _flags: u32,
}

/// Starts core under the refusing filter. The filter needs
/// `no_new_privs`, so a file-capability core started this way holds no
/// capabilities; that does not matter to this refusal control.
pub fn refuse_seccomp(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    const NR: u32 = 0;
    const ARCH: u32 = 4;
    let statement = |code: u32, k: u32| libc::sock_filter {
        code: code as u16,
        jt: 0,
        jf: 0,
        k,
    };
    let jump = |k: u32, jt: u8, jf: u8| libc::sock_filter {
        code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
        jt,
        jf,
        k,
    };
    let load = libc::BPF_LD | libc::BPF_W | libc::BPF_ABS;
    let ret = libc::BPF_RET | libc::BPF_K;
    let filter = [
        statement(load, ARCH),
        jump(AUDIT_ARCH_X86_64, 0, 2),
        statement(load, NR),
        jump(libc::SYS_seccomp as u32, 1, 0),
        statement(ret, SECCOMP_RET_ALLOW),
        statement(ret, SECCOMP_RET_USER_NOTIF),
    ];
    // SAFETY: the closure runs in the forked child before `execve` and
    // makes only raw syscalls on memory copied into the child.
    unsafe {
        command.pre_exec(move || {
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_ptr().cast_mut(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let listener = libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                SECCOMP_FILTER_FLAG_NEW_LISTENER,
                &program as *const libc::sock_fprog,
            );
            // A fixed descriptor without close-on-exec survives `execve`,
            // so the test can copy it out of core.
            if listener < 0 || libc::dup2(listener as i32, LISTENER_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(listener as i32);
            Ok(())
        });
    }
}

/// Answers every `seccomp(2)` call under core's filter with `EPERM`, and
/// holds a pidfd for every refused `plugin-host`.
pub struct Refusals {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Vec<Tracked>>>,
}

impl Refusals {
    /// Copies the listener out of `core` and starts answering.
    pub fn start(core: &Tracked) -> Self {
        let listener = core.copy_fd(LISTENER_FD);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::spawn(move || answer(&listener, &stopping));
        Self {
            stop,
            thread: Some(thread),
        }
    }

    /// Stops answering and returns the refused plugin hosts.
    pub fn finish(&mut self) -> Vec<Tracked> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .map(|thread| thread.join().unwrap())
            .unwrap_or_default()
    }
}

impl Drop for Refusals {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn answer(listener: &OwnedFd, stop: &AtomicBool) -> Vec<Tracked> {
    let mut refused = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let mut poll = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut poll, 1, 20) };
        if ready <= 0 {
            continue;
        }
        if poll.revents & libc::POLLHUP != 0 {
            // Core and every process under its filter are gone.
            break;
        }
        let mut notification = Notification::default();
        // SAFETY: a live listener and a writable `struct seccomp_notif`.
        let received = unsafe {
            libc::ioctl(
                listener.as_raw_fd(),
                SECCOMP_IOCTL_NOTIF_RECV,
                &mut notification,
            )
        };
        if received != 0 {
            continue;
        }
        // The caller waits in the syscall until answered, so it is alive.
        if argv1_of(notification.pid).as_deref() == Some("plugin-host")
            && let Some(host) = Tracked::try_open(notification.pid)
        {
            refused.push(host);
        }
        let mut response = Response {
            _id: notification.id,
            _val: 0,
            _error: -libc::EPERM,
            _flags: 0,
        };
        // SAFETY: a live listener and a readable `struct seccomp_notif_resp`.
        unsafe {
            libc::ioctl(
                listener.as_raw_fd(),
                SECCOMP_IOCTL_NOTIF_SEND,
                &mut response,
            )
        };
    }
    refused
}

impl Tracked {
    /// A copy of descriptor `fd` of this process (`pidfd_getfd`).
    pub fn copy_fd(&self, fd: i32) -> OwnedFd {
        // SAFETY: plain syscall on a live pidfd; the result is checked.
        let copied = unsafe { libc::syscall(libc::SYS_pidfd_getfd, self.pidfd(), fd, 0) };
        assert!(
            copied >= 0,
            "pidfd_getfd({}, {fd}): {}",
            self.pid,
            std::io::Error::last_os_error()
        );
        // SAFETY: a fresh descriptor this value now owns.
        unsafe { OwnedFd::from_raw_fd(copied as i32) }
    }
}

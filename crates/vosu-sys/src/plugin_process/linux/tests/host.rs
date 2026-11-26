//! The test plugin host: this test binary re-executed by the spawn tests.
//!
//! An `.init_array` entry runs before `main` (and so before the test
//! harness starts any thread). When `argv[1]` is exactly `plugin-host` it
//! plays the child side of one scenario, named by [`SCENARIO`], writes a
//! `key=value` report to [`REPORT`] and `_exit`s, never reaching the
//! harness. When `argv[1]` is `fd-probe` it is a grandchild started by a
//! plugin host and reports only its own descriptors and confinement.

use std::ffi::{CStr, OsString};
use std::fmt::Write as _;
use std::os::unix::ffi::OsStringExt;

use super::super::{InheritedEndpoints, arm_parent_death, drop_privileges};

pub(super) const SCENARIO: &str = "VOSU_SYS_TEST_PLUGIN_HOST_SCENARIO";
pub(super) const REPORT: &str = "VOSU_SYS_TEST_PLUGIN_HOST_REPORT";
pub(super) const REGION: &str = "VOSU_SYS_TEST_PLUGIN_HOST_REGION";
pub(super) const IDENTITY: &str = "vosu-sys-plugin-host-test";
pub(super) const H2P_MARK: &[u8; 8] = b"H2P-MARK";
pub(super) const P2H_MARK: &[u8; 8] = b"P2H-MARK";
pub(super) const CHILD_MARK: &[u8; 8] = b"CHILD-OK";
/// Exit status after a refused drop: the report holds the failure.
pub(super) const EXIT_REFUSED: i32 = 3;
/// Exit status when the user-namespace precondition was unavailable.
pub(super) const EXIT_NO_USERNS: i32 = 4;
/// Exit status when the parent check refused the launch.
pub(super) const EXIT_PARENT: i32 = 5;

type InitArray = extern "C" fn(libc::c_int, *const *const libc::c_char, *const *const libc::c_char);

#[used]
#[unsafe(link_section = ".init_array")]
static PLUGIN_HOST_TEST_ENTRY: InitArray = entry;

extern "C" fn entry(
    argc: libc::c_int,
    argv: *const *const libc::c_char,
    _envp: *const *const libc::c_char,
) {
    if argc < 2 || argv.is_null() {
        return;
    }
    let args: Vec<OsString> = (0..argc as usize)
        .map(|index| {
            // SAFETY: glibc passes `argc` valid NUL-terminated strings to
            // `.init_array` entries.
            let arg = unsafe { CStr::from_ptr(*argv.add(index)) };
            OsString::from_vec(arg.to_bytes().to_vec())
        })
        .collect();
    let code = match args[1].to_str() {
        Some("plugin-host") => plugin_host(&args),
        Some("fd-probe") => fd_probe(),
        _ => return,
    };
    // SAFETY: ends this re-executed process without running the harness.
    unsafe { libc::_exit(code) }
}

struct Report(String);

impl Report {
    fn put(&mut self, key: &str, value: impl std::fmt::Display) {
        let _ = writeln!(self.0, "{key}={value}");
    }

    fn status(&mut self, prefix: &str) {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        for line in status.lines() {
            for field in [
                "CapInh",
                "CapPrm",
                "CapEff",
                "CapBnd",
                "CapAmb",
                "NoNewPrivs",
                "Seccomp",
                "Uid",
                "Threads",
            ] {
                if let Some(value) = line.strip_prefix(field).and_then(|r| r.strip_prefix(':')) {
                    self.put(
                        &format!("{prefix}.{field}"),
                        value.trim().replace('\t', " "),
                    );
                }
            }
        }
    }

    fn finish(self) {
        if let Some(path) = std::env::var_os(REPORT) {
            let _ = std::fs::write(path, self.0);
        }
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn open_fds() -> String {
    (0..1024)
        // SAFETY: `F_GETFD` only reads a descriptor's flags.
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0)
        .map(|fd| fd.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn cloexec_flags() -> String {
    (3..=6)
        // SAFETY: `F_GETFD` only reads a descriptor's flags.
        .map(|fd| (unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC).to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn fd_probe() -> i32 {
    let mut report = Report(String::new());
    report.put("fds", open_fds());
    report.status("status");
    report.finish();
    0
}

fn plugin_host(args: &[OsString]) -> i32 {
    let mut report = Report(String::new());
    let scenario = std::env::var(SCENARIO).unwrap_or_default();
    report.put("argc", args.len());
    report.put(
        "argv",
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" "),
    );
    report.put("pre.fds", open_fds());
    for fd in 0..=6 {
        let link = std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .map(|link| link.display().to_string())
            .unwrap_or_else(|error| format!("error:{error}"));
        report.put(&format!("pre.fd.{fd}"), link);
    }
    report.put("pre.cloexec", cloexec_flags());
    report.status("pre");
    // SAFETY: `getpgrp`/`getpid` have no preconditions.
    report.put("own_group", unsafe { libc::getpgrp() == libc::getpid() });

    let parent: u32 = args
        .get(3)
        .and_then(|arg| arg.to_str())
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(0);
    let expected_parent = if scenario == "parent-mismatch" {
        parent + 1
    } else {
        parent
    };
    if let Err(error) = arm_parent_death(expected_parent) {
        report.put("parent", format!("err:{error:?}"));
        report.finish();
        return EXIT_PARENT;
    }
    report.put("parent", "ok");
    let mut armed: libc::c_int = 0;
    // SAFETY: writes one `int` into the live local.
    unsafe {
        libc::prctl(
            libc::PR_GET_PDEATHSIG,
            &mut armed as *mut libc::c_int,
            0,
            0,
            0,
        )
    };
    report.put("pdeathsig", armed);

    let endpoints = match InheritedEndpoints::adopt(&args[4]) {
        Ok(endpoints) => endpoints,
        Err(error) => {
            report.put("adopt", format!("err:{error:?}"));
            report.finish();
            return 2;
        }
    };
    report.put("adopt", "ok");
    report.put("adopted.cloexec", cloexec_flags());
    report.put(
        "adopt_again",
        format!("{:?}", InheritedEndpoints::adopt(&args[4]).err()),
    );
    let region_len: usize = std::env::var(REGION)
        .ok()
        .and_then(|len| len.parse().ok())
        .unwrap_or(0);
    let to_plugin = endpoints.host_to_plugin.into_ring_objects(region_len);
    let to_host = endpoints.plugin_to_host.into_ring_objects(region_len);
    let (to_plugin, to_host) = match (to_plugin, to_host) {
        (Ok(to_plugin), Ok(to_host)) => (to_plugin, to_host),
        (to_plugin, to_host) => {
            report.put(
                "map",
                format!("err:{:?} {:?}", to_plugin.err(), to_host.err()),
            );
            report.finish();
            return 2;
        }
    };
    let mut mark = [0_u8; 8];
    let _ = to_plugin.region().read(0, &mut mark);
    report.put("region.h2p", String::from_utf8_lossy(&mark));
    let _ = to_host.region().read(0, &mut mark);
    report.put("region.p2h", String::from_utf8_lossy(&mark));
    let _ = to_host.region().write(8, CHILD_MARK);

    match scenario.as_str() {
        "wait" => {
            // Readiness, then answer every ping until killed.
            let _ = to_host.doorbell().ring();
            loop {
                if to_plugin.doorbell().wait(None).is_err() {
                    return 6;
                }
                let _ = to_host.doorbell().ring();
            }
        }
        "userns-caps" => {
            if let Err(code) = raise_capabilities_in_user_namespace(&mut report) {
                report.finish();
                return code;
            }
        }
        "fault-no-new-privs" => deny(
            &mut report,
            libc::SYS_prctl,
            Some(libc::PR_SET_NO_NEW_PRIVS),
        ),
        "fault-seccomp" => deny(&mut report, libc::SYS_seccomp, None),
        "fault-ambient" => deny(&mut report, libc::SYS_prctl, Some(libc::PR_CAP_AMBIENT)),
        "fault-capset" => deny(&mut report, libc::SYS_capset, None),
        "fault-threads" => {
            // A second thread that stays parked through the drop: its
            // sender is never dropped.
            let (hold, wait) = std::sync::mpsc::sync_channel::<()>(0);
            std::thread::spawn(move || {
                let _ = wait.recv();
            });
            std::mem::forget(hold);
        }
        _ => {}
    }

    let policy = if scenario == "allow-user-namespaces" {
        super::super::NamespacePolicy::AllowUserNamespaces
    } else {
        super::super::NamespacePolicy::DenyUserNamespaces
    };
    match drop_privileges(policy) {
        Ok(readback) => {
            report.put("drop", "ok");
            report.put("readback", format!("{readback:?}"));
            report.put("namespace_policy", readback.namespace_policy.as_str());
            report.put(
                "namespace_probe_errno",
                readback.namespace_probe_errno.unwrap_or(0),
            );
        }
        Err(failure) => {
            report.put(
                "drop",
                format!(
                    "err:{}:{}:{}",
                    failure.step.as_str(),
                    failure.code.as_str(),
                    failure.errno.unwrap_or(0)
                ),
            );
            report.status("post");
            report.finish();
            return EXIT_REFUSED;
        }
    }
    report.status("post");
    if policy == super::super::NamespacePolicy::DenyUserNamespaces {
        probes(&mut report);
    }
    // Only now would a plugin host open the plugin library.
    report.put("load", "reached");
    report.finish();
    0
}

/// Enters a new user namespace, where this process holds every capability,
/// and raises them into the inheritable and ambient sets too, so the drop
/// has nonzero sets to clear.
fn raise_capabilities_in_user_namespace(report: &mut Report) -> Result<(), i32> {
    // SAFETY: integer flags only; this process is single-threaded.
    if unsafe { libc::unshare(libc::CLONE_NEWUSER) } != 0 {
        report.put("userns", format!("err:{}", errno()));
        return Err(EXIT_NO_USERNS);
    }
    let last: u32 = std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(40);
    let full = if last >= 63 {
        u64::MAX
    } else {
        (1_u64 << (last + 1)) - 1
    };
    let word = |half: u32| (full >> (32 * half)) as u32;
    let header = [0x2008_0522_u32, 0];
    let data = [[word(0), word(0), word(0)], [word(1), word(1), word(1)]];
    // SAFETY: version-3 `capset` reads the live header and two data words.
    if unsafe { libc::syscall(libc::SYS_capset, header.as_ptr(), data.as_ptr()) } != 0 {
        report.put("raise", format!("capset err:{}", errno()));
        return Err(2);
    }
    for cap in 0..=last {
        // SAFETY: integer arguments only.
        if unsafe {
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_RAISE as libc::c_ulong,
                libc::c_ulong::from(cap),
                0,
                0,
            )
        } != 0
        {
            report.put("raise", format!("ambient {cap} err:{}", errno()));
            return Err(2);
        }
    }
    report.status("raised");
    Ok(())
}

/// Installs a filter that fails `nr` (when `arg0` is `Some`, only with that
/// first argument) with `EPERM`, standing in for an environment that
/// refuses one step of the drop.
fn deny(report: &mut Report, nr: libc::c_long, arg0: Option<libc::c_int>) {
    let load = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
    let jeq = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
    let ret = (libc::BPF_RET | libc::BPF_K) as u16;
    let insn = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    let errno_ret = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
    let mut program = match arg0 {
        None => vec![
            insn(load, 0, 0, 0),
            insn(jeq, 0, 1, nr as u32),
            insn(ret, 0, 0, errno_ret),
            insn(ret, 0, 0, libc::SECCOMP_RET_ALLOW),
        ],
        Some(arg0) => vec![
            insn(load, 0, 0, 0),
            insn(jeq, 0, 3, nr as u32),
            insn(load, 0, 0, 16),
            insn(jeq, 0, 1, arg0 as u32),
            insn(ret, 0, 0, errno_ret),
            insn(ret, 0, 0, libc::SECCOMP_RET_ALLOW),
        ],
    };
    let filter = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_mut_ptr(),
    };
    // SAFETY: the kernel copies the live program `filter` points at.
    let result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            0,
            &filter as *const libc::sock_fprog,
        )
    };
    report.put(
        "fault_filter",
        if result == 0 {
            "ok".to_owned()
        } else {
            format!("err:{}", errno())
        },
    );
}

/// What a confined plugin host can and cannot still do.
fn probes(report: &mut Report) {
    // SAFETY: `clone3` with a null argument: the filter answers first.
    let clone3 = unsafe { libc::syscall(libc::SYS_clone3, std::ptr::null::<u8>(), 0_usize) };
    report.put("probe.clone3", if clone3 < 0 { errno() } else { 0 });
    // SAFETY: `setns` on an invalid descriptor: the filter answers first.
    let setns = unsafe { libc::setns(-1, 0) };
    report.put("probe.setns", if setns < 0 { errno() } else { 0 });
    // SAFETY: integer flags only.
    let unshare = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
    report.put("probe.unshare", if unshare < 0 { errno() } else { 0 });
    // SAFETY: a fork-like `clone` asking for a new user namespace; a child
    // that nevertheless appears exits at once.
    let clone = unsafe {
        libc::syscall(
            libc::SYS_clone,
            (libc::CLONE_NEWUSER | libc::SIGCHLD) as libc::c_ulong,
            0_usize,
            0_usize,
            0_usize,
            0_usize,
        )
    };
    if clone == 0 {
        // SAFETY: the unexpected child exits immediately.
        unsafe { libc::_exit(0) };
    }
    report.put("probe.clone_newuser", if clone < 0 { errno() } else { 0 });

    let thread = std::thread::spawn(|| 7).join();
    report.put("probe.thread", matches!(thread, Ok(7)));

    let grandchild_report = std::env::var_os(REPORT).map(|path| {
        let mut path = path;
        path.push(".grandchild");
        path
    });
    if let (Ok(exe), Some(path)) = (std::env::current_exe(), grandchild_report) {
        let status = std::process::Command::new(exe)
            .arg("fd-probe")
            .env(REPORT, &path)
            .status();
        report.put(
            "probe.grandchild",
            format!("{:?}", status.map(|s| s.code())),
        );
    }

    #[cfg(target_arch = "x86_64")]
    report.put(
        "probe.x32",
        forked_signal(|| {
            // SAFETY: an x32-table `getpid`; the filter kills the process.
            unsafe { libc::syscall(0x4000_0000 | libc::SYS_getpid) };
        }),
    );
    #[cfg(target_arch = "x86_64")]
    report.put(
        "probe.i386",
        forked_signal(|| {
            // SAFETY: the 32-bit `getpid` entry; the filter kills the process
            // before the kernel runs it.
            unsafe {
                std::arch::asm!("int 0x80", inlateout("eax") 20_u32 => _, options(nostack));
            }
        }),
    );
}

#[cfg(target_arch = "x86_64")]
/// Runs `body` in a forked grandchild and returns the signal that ended it
/// (0 if it exited instead).
fn forked_signal(body: fn()) -> i32 {
    // SAFETY: the grandchild runs `body` (a raw syscall) and `_exit`s.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        body();
        // SAFETY: ends the grandchild without teardown.
        unsafe { libc::_exit(0) };
    }
    if pid < 0 {
        return -1;
    }
    let mut status = 0;
    // SAFETY: waits for our own child.
    unsafe { libc::waitpid(pid, &mut status, 0) };
    if libc::WIFSIGNALED(status) {
        libc::WTERMSIG(status)
    } else {
        0
    }
}

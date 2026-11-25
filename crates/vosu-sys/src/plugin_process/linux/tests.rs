//! Linux plugin-host launch, inherited-endpoint and privilege-drop tests.
//!
//! The pure checks cover the argument grammar, the readback verdict and the
//! seccomp program (through a small classic-BPF interpreter). The native
//! tests re-execute this test binary as a real plugin host through the real
//! launcher ([`host`] holds the child side) and read its own
//! `/proc/self/status`, descriptor table and probes back after the drop.
//! None of them needs privileges; an unprivileged user namespace supplies
//! the nonzero capability sets for the drop to clear.

mod host;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use super::super::RingObjects;
use super::capabilities::seccomp::{self, FilterArch, Syscalls};
use super::channel::parse_argument;
use super::spawn::spawn_from_current_thread;
use super::{
    ChildStage, EndpointError, PluginHostChild, PluginHostCommand, PluginHostLauncher,
    ReadbackMismatch, SecurityReadback, SpawnError,
};

const BOUND: Duration = Duration::from_secs(20);
const REGION_LEN: usize = 4096;

// ---- pure checks -------------------------------------------------------

#[test]
fn endpoint_argument_accepts_only_four_distinct_canonical_descriptors() {
    assert_eq!(parse_argument(OsStr::new("3,4,5,6")), Ok([3, 4, 5, 6]));
    assert_eq!(
        parse_argument(OsStr::new("10,4,5,600")),
        Ok([10, 4, 5, 600])
    );
    for malformed in [
        "",
        "3,4,5",
        "3,4,5,6,7",
        "3,4,5,",
        ",3,4,5,6",
        "3,4,5,6 ",
        " 3,4,5,6",
        "3,4,5,+6",
        "3,4,5,-6",
        "3,4,5,06",
        "3,4,5,0x6",
        "3,4,5,2",
        "0,4,5,6",
        "3,4,4,6",
        "3;4;5;6",
        "3,4,5,99999999999",
    ] {
        assert_eq!(
            parse_argument(OsStr::new(malformed)),
            Err(EndpointError::Malformed),
            "{malformed:?}"
        );
    }
}

fn verified() -> SecurityReadback {
    SecurityReadback {
        last_cap: 40,
        effective: 0,
        permitted: 0,
        inheritable: 0,
        ambient: 0,
        bounding: 0x1ff_ffff_ffff,
        no_new_privs: 1,
        seccomp: 2,
        user_namespace_errno: Some(libc::EPERM),
        uids: [1000, 1000, 1000],
        threads: 1,
    }
}

#[test]
fn readback_accepts_only_the_a5_1_postcondition_and_ignores_bounding() {
    assert_eq!(verified().verify(), Ok(()));
    let empty_bounding = SecurityReadback {
        bounding: 0,
        ..verified()
    };
    assert_eq!(empty_bounding.verify(), Ok(()), "CapBnd is recorded only");
    let cases: [(SecurityReadback, ReadbackMismatch); 12] = [
        (
            SecurityReadback {
                effective: 1 << 19,
                ..verified()
            },
            ReadbackMismatch::Effective(1 << 19),
        ),
        (
            SecurityReadback {
                permitted: 1 << 63,
                ..verified()
            },
            ReadbackMismatch::Permitted(1 << 63),
        ),
        (
            SecurityReadback {
                inheritable: 1,
                ..verified()
            },
            ReadbackMismatch::Inheritable(1),
        ),
        (
            SecurityReadback {
                ambient: 1 << 40,
                ..verified()
            },
            ReadbackMismatch::Ambient(1 << 40),
        ),
        (
            SecurityReadback {
                no_new_privs: 0,
                ..verified()
            },
            ReadbackMismatch::NoNewPrivs(0),
        ),
        (
            SecurityReadback {
                seccomp: 0,
                ..verified()
            },
            ReadbackMismatch::Seccomp(0),
        ),
        (
            SecurityReadback {
                seccomp: 1,
                ..verified()
            },
            ReadbackMismatch::Seccomp(1),
        ),
        (
            SecurityReadback {
                user_namespace_errno: None,
                ..verified()
            },
            ReadbackMismatch::UserNamespace(None),
        ),
        (
            SecurityReadback {
                user_namespace_errno: Some(libc::EINVAL),
                ..verified()
            },
            ReadbackMismatch::UserNamespace(Some(libc::EINVAL)),
        ),
        (
            SecurityReadback {
                uids: [1000, 0, 1000],
                ..verified()
            },
            ReadbackMismatch::RootUid([1000, 0, 1000]),
        ),
        (
            SecurityReadback {
                uids: [1000, 1000, 0],
                ..verified()
            },
            ReadbackMismatch::RootUid([1000, 1000, 0]),
        ),
        (
            SecurityReadback {
                threads: 2,
                ..verified()
            },
            ReadbackMismatch::Threads(2),
        ),
    ];
    for (readback, mismatch) in cases {
        assert_eq!(readback.verify(), Err(mismatch));
    }
}

/// Minimal classic-BPF interpreter for the instructions the filter uses.
fn run_filter(program: &[libc::sock_filter], nr: u32, arch: u32, arg0: u64) -> u32 {
    let mut data = [0_u8; 64];
    data[0..4].copy_from_slice(&nr.to_le_bytes());
    data[4..8].copy_from_slice(&arch.to_le_bytes());
    data[16..24].copy_from_slice(&arg0.to_le_bytes());
    let mut accumulator = 0_u32;
    let mut pc = 0_usize;
    loop {
        let insn = program[pc];
        let class = u32::from(insn.code) & 0x07;
        let op = u32::from(insn.code) & 0xf0;
        match class {
            c if c == libc::BPF_LD => {
                let at = insn.k as usize;
                accumulator = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
                pc += 1;
            }
            c if c == libc::BPF_JMP => {
                let taken = match op {
                    o if o == libc::BPF_JEQ => accumulator == insn.k,
                    o if o == libc::BPF_JGE => accumulator >= insn.k,
                    o if o == libc::BPF_JSET => accumulator & insn.k != 0,
                    other => panic!("unexpected jump {other:#x}"),
                };
                pc += 1 + usize::from(if taken { insn.jt } else { insn.jf });
            }
            c if c == libc::BPF_RET => return insn.k,
            other => panic!("unexpected class {other:#x}"),
        }
    }
}

const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
const KILL: u32 = libc::SECCOMP_RET_KILL_PROCESS;
const EPERM: u32 = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
const ENOSYS: u32 = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;

/// The expected verdict for one native syscall (§7.3 step 3).
fn model(syscalls: Syscalls, floor: Option<u32>, nr: u32, arg0: u64) -> u32 {
    let newuser = arg0 & libc::CLONE_NEWUSER as u64 != 0;
    if floor.is_some_and(|floor| nr >= floor) {
        KILL
    } else if nr == syscalls.clone3 {
        ENOSYS
    } else if nr == syscalls.setns || ((nr == syscalls.unshare || nr == syscalls.clone) && newuser)
    {
        EPERM
    } else {
        ALLOW
    }
}

#[test]
fn seccomp_program_denies_exactly_user_namespace_routes_on_every_syscall_number() {
    let arches = [
        FilterArch {
            audit_arch: 0xC000_003E,
            foreign_nr_floor: Some(0x4000_0000),
        },
        FilterArch {
            audit_arch: 0xC000_00B7,
            foreign_nr_floor: None,
        },
    ];
    // Real and synthetic numbering, so a swapped comparison shows up.
    let tables = [
        seccomp::SYSCALLS,
        Syscalls {
            clone: 220,
            clone3: 435,
            unshare: 97,
            setns: 268,
        },
    ];
    let mut flag_patterns = vec![0_u64, u64::MAX, u64::from(u32::MAX)];
    flag_patterns.extend((0..64).map(|bit| 1_u64 << bit));
    flag_patterns.push(libc::CLONE_NEWUSER as u64 | libc::SIGCHLD as u64);
    flag_patterns.push(!(libc::CLONE_NEWUSER as u64));
    for arch in arches {
        for syscalls in tables {
            let program = seccomp::program(arch, syscalls);
            assert!(program.len() < 32);
            let mut numbers: Vec<u32> = (0..1024).collect();
            numbers.extend([0x3fff_ffff, 0x4000_0000, 0x4000_0027, 0x4000_0110, u32::MAX]);
            for nr in numbers {
                for &arg0 in &flag_patterns {
                    assert_eq!(
                        run_filter(&program, nr, arch.audit_arch, arg0),
                        model(syscalls, arch.foreign_nr_floor, nr, arg0),
                        "nr {nr:#x} arg0 {arg0:#x} arch {:#x}",
                        arch.audit_arch
                    );
                }
                for foreign in [0x4000_0003_u32, 0xC000_00B7, 0xC000_003E, 0] {
                    if foreign != arch.audit_arch {
                        assert_eq!(run_filter(&program, nr, foreign, 0), KILL);
                    }
                }
            }
        }
    }
}

#[test]
fn seccomp_program_has_a_native_architecture_on_supported_targets() {
    assert!(
        seccomp::NATIVE.is_some(),
        "this target would fail every plugin host closed with security_unavailable"
    );
}

// ---- native plugin hosts -------------------------------------------------

struct Rings {
    to_plugin: RingObjects,
    to_host: RingObjects,
}

fn rings() -> Rings {
    let to_plugin = RingObjects::create(REGION_LEN).unwrap();
    let to_host = RingObjects::create(REGION_LEN).unwrap();
    to_plugin.region().write(0, host::H2P_MARK).unwrap();
    to_host.region().write(0, host::P2H_MARK).unwrap();
    Rings { to_plugin, to_host }
}

fn executable() -> PathBuf {
    std::env::current_exe().unwrap()
}

struct Scenario {
    _dir: tempfile::TempDir,
    report: PathBuf,
    region: String,
}

impl Scenario {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("report");
        Self {
            _dir: dir,
            report,
            region: REGION_LEN.to_string(),
        }
    }

    fn env<'a>(&'a self, name: &'a str) -> [(&'a OsStr, &'a OsStr); 3] {
        [
            (OsStr::new(host::SCENARIO), OsStr::new(name)),
            (OsStr::new(host::REPORT), self.report.as_os_str()),
            (OsStr::new(host::REGION), OsStr::new(&self.region)),
        ]
    }

    fn read(&self, suffix: &str) -> HashMap<String, String> {
        let mut path = self.report.clone().into_os_string();
        path.push(suffix);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("no report at {}: {error}", path.to_string_lossy()));
        text.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }
}

fn command<'a>(rings: &'a Rings, executable: &'a std::path::Path) -> PluginHostCommand<'a> {
    PluginHostCommand {
        executable,
        build_identity: host::IDENTITY,
        host_to_plugin: &rings.to_plugin,
        plugin_to_host: &rings.to_host,
    }
}

/// Runs one scenario through the real launcher to completion.
fn run(name: &str) -> (ExitStatus, HashMap<String, String>, Rings, Scenario) {
    let launcher = PluginHostLauncher::start().unwrap();
    let rings = rings();
    let scenario = Scenario::new();
    let executable = executable();
    let child = launcher
        .spawn_with_env(&command(&rings, &executable), &scenario.env(name))
        .unwrap();
    let status = child
        .wait_timeout(BOUND)
        .unwrap()
        .expect("test plugin host finished within the bound");
    let report = scenario.read("");
    (status, report, rings, scenario)
}

fn get<'a>(report: &'a HashMap<String, String>, key: &str) -> &'a str {
    report
        .get(key)
        .unwrap_or_else(|| panic!("report lacks {key}: {report:#?}"))
}

fn hex(report: &HashMap<String, String>, key: &str) -> u64 {
    u64::from_str_radix(get(report, key), 16).unwrap()
}

#[test]
fn plugin_host_reaches_the_a5_1_postcondition_with_only_its_endpoints() {
    let (status, report, rings, scenario) = run("drop");
    assert_eq!(status.code(), Some(0), "{report:#?}");

    // Exact §7.6 command line.
    assert_eq!(get(&report, "argc"), "5");
    let argv: Vec<&str> = get(&report, "argv").split(' ').collect();
    assert_eq!(argv[..3], ["vosu", "plugin-host", host::IDENTITY]);
    assert_eq!(argv[3], std::process::id().to_string());
    assert_eq!(argv[4], "3,4,5,6");

    // Only the null device and the four endpoint objects crossed exec,
    // none of them close-on-exec.
    assert_eq!(get(&report, "pre.fds"), "0,1,2,3,4,5,6");
    for fd in 0..=2 {
        assert_eq!(get(&report, &format!("pre.fd.{fd}")), "/dev/null");
    }
    for fd in [3, 5] {
        assert!(get(&report, &format!("pre.fd.{fd}")).starts_with("/memfd:vosu-plugin-ring"));
    }
    for fd in [4, 6] {
        assert_eq!(
            get(&report, &format!("pre.fd.{fd}")),
            "anon_inode:[eventfd]"
        );
    }
    assert_eq!(get(&report, "pre.cloexec"), "0,0,0,0");
    // The spawn set no_new_privs before exec: the exec granted nothing.
    assert_eq!(get(&report, "pre.NoNewPrivs"), "1");
    assert_eq!(hex(&report, "pre.CapEff"), 0);
    // The spawn installs no filter of its own: before its §7.3 drop the
    // child has exactly the seccomp mode it inherited from this process (0
    // on a plain host, 2 inside a container that filters every process).
    assert_eq!(get(&report, "pre.Seccomp"), own_seccomp_mode());
    assert_eq!(get(&report, "own_group"), "true");

    // Parent death armed and parent verified after exec (A5.8).
    assert_eq!(get(&report, "parent"), "ok");
    assert_eq!(get(&report, "pdeathsig"), libc::SIGKILL.to_string());

    // Adoption marks the endpoints close-on-exec, once only, and they are
    // the objects core created.
    assert_eq!(get(&report, "adopt"), "ok");
    assert_eq!(get(&report, "adopted.cloexec"), "1,1,1,1");
    // A second adoption is refused: the adopted descriptors are now
    // close-on-exec, so they no longer read as inherited.
    assert_eq!(get(&report, "adopt_again"), "Some(NotInherited { fd: 3 })");
    assert_eq!(get(&report, "region.h2p"), "H2P-MARK");
    assert_eq!(get(&report, "region.p2h"), "P2H-MARK");
    let mut mark = [0_u8; 8];
    rings.to_host.region().read(8, &mut mark).unwrap();
    assert_eq!(&mark, host::CHILD_MARK);

    // §7.3 postcondition, read back from the child's own procfs.
    assert_eq!(get(&report, "drop"), "ok");
    for set in ["CapInh", "CapPrm", "CapEff", "CapAmb"] {
        assert_eq!(hex(&report, &format!("post.{set}")), 0, "{set}");
    }
    assert!(report.contains_key("post.CapBnd"), "CapBnd is recorded");
    assert_eq!(get(&report, "post.NoNewPrivs"), "1");
    assert_eq!(get(&report, "post.Seccomp"), "2");
    assert_eq!(get(&report, "post.Threads"), "1");
    assert!(
        get(&report, "post.Uid").split(' ').all(|uid| uid != "0"),
        "no root UID"
    );

    // The filter's denials and what still works.
    assert_eq!(get(&report, "probe.clone3"), libc::ENOSYS.to_string());
    assert_eq!(get(&report, "probe.setns"), libc::EPERM.to_string());
    assert_eq!(get(&report, "probe.unshare"), libc::EPERM.to_string());
    assert_eq!(get(&report, "probe.clone_newuser"), libc::EPERM.to_string());
    assert_eq!(get(&report, "probe.thread"), "true");
    assert_eq!(get(&report, "probe.grandchild"), "Ok(Some(0))");
    #[cfg(target_arch = "x86_64")]
    {
        assert_eq!(get(&report, "probe.x32"), libc::SIGSYS.to_string());
        assert_eq!(get(&report, "probe.i386"), libc::SIGSYS.to_string());
    }
    assert_eq!(get(&report, "load"), "reached");

    // A process the plugin starts inherits the confinement, not the channel.
    let grandchild = scenario.read(".grandchild");
    assert_eq!(get(&grandchild, "fds"), "0,1,2");
    assert_eq!(get(&grandchild, "status.NoNewPrivs"), "1");
    assert_eq!(get(&grandchild, "status.Seccomp"), "2");
    assert_eq!(hex(&grandchild, "status.CapEff"), 0);

    // Core's own copies stay close-on-exec, so unrelated children of core
    // never inherit them.
    for fd in [
        rings.to_plugin.mapping_fd(),
        rings.to_plugin.doorbell_fd(),
        rings.to_host.mapping_fd(),
        rings.to_host.doorbell_fd(),
    ] {
        // SAFETY: reads the flags of a live descriptor.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
    }
}

#[test]
fn drop_clears_nonzero_permitted_effective_inheritable_and_ambient_sets() {
    let (status, report, _rings, _scenario) = run("userns-caps");
    assert_eq!(
        status.code(),
        Some(0),
        "an unprivileged user namespace must supply real capabilities: {report:#?}"
    );
    for set in ["CapInh", "CapPrm", "CapEff", "CapAmb"] {
        assert_ne!(
            hex(&report, &format!("raised.{set}")),
            0,
            "precondition {set}"
        );
        assert_eq!(
            hex(&report, &format!("post.{set}")),
            0,
            "{set} after the drop"
        );
    }
    assert_eq!(get(&report, "drop"), "ok");
    assert_eq!(get(&report, "post.NoNewPrivs"), "1");
    assert_eq!(get(&report, "post.Seccomp"), "2");
    assert_eq!(get(&report, "probe.unshare"), libc::EPERM.to_string());
    assert_eq!(get(&report, "load"), "reached");
}

/// Each refused step fails closed: a precise step, code and errno, and no
/// plugin load.
#[test]
fn refused_drop_steps_fail_closed_before_any_plugin_load() {
    for (scenario, expected) in [
        (
            "fault-no-new-privs",
            "err:no_new_privs:security_unavailable:1",
        ),
        ("fault-seccomp", "err:seccomp:security_unavailable:1"),
        ("fault-ambient", "err:ambient:drop_failed:1"),
        ("fault-capset", "err:capset:drop_failed:1"),
        ("fault-threads", "err:threads:not_single_threaded:0"),
    ] {
        let (status, report, _rings, _scenario) = run(scenario);
        assert_eq!(
            status.code(),
            Some(host::EXIT_REFUSED),
            "{scenario}: {report:#?}"
        );
        if scenario != "fault-threads" {
            assert_eq!(get(&report, "fault_filter"), "ok", "{scenario}");
        }
        assert_eq!(get(&report, "drop"), expected, "{scenario}");
        assert!(
            !report.contains_key("load"),
            "{scenario} reached the plugin load"
        );
    }
}

#[test]
fn parent_mismatch_refuses_the_launch_before_endpoints_are_adopted() {
    let (status, report, _rings, _scenario) = run("parent-mismatch");
    assert_eq!(status.code(), Some(host::EXIT_PARENT));
    assert!(
        get(&report, "parent").starts_with("err:Mismatch"),
        "{report:#?}"
    );
    assert!(!report.contains_key("adopt"));
    assert!(!report.contains_key("load"));
}

/// Waits for the `wait` scenario's readiness ring, then pings it once.
fn ping(rings: &Rings) {
    assert!(
        rings.to_host.doorbell().wait(Some(BOUND)).unwrap(),
        "child ready"
    );
    rings.to_plugin.doorbell().ring().unwrap();
    assert!(
        rings.to_host.doorbell().wait(Some(BOUND)).unwrap(),
        "child answered"
    );
}

#[test]
fn child_spawned_from_a_transient_thread_dies_with_that_thread() {
    let rings = rings();
    let scenario = Scenario::new();
    let executable = executable();
    let env = scenario.env("wait");
    let command = command(&rings, &executable);
    let child = std::thread::scope(|scope| {
        scope
            .spawn(|| spawn_from_current_thread(&command, &env))
            .join()
            .unwrap()
    })
    .unwrap();
    let status = child
        .wait_timeout(BOUND)
        .unwrap()
        .expect("the parent-death signal fired when the spawning thread exited");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
}

#[test]
fn launcher_children_outlive_the_requesting_thread_and_launcher_handle() {
    let launcher = PluginHostLauncher::start().unwrap();
    let rings = rings();
    let scenario = Scenario::new();
    let executable = executable();
    let env = scenario.env("wait");
    let command = command(&rings, &executable);
    let child = std::thread::scope(|scope| {
        scope
            .spawn(|| launcher.spawn_with_env(&command, &env))
            .join()
            .unwrap()
    })
    .unwrap();
    drop(launcher);
    ping(&rings);
    assert!(child.try_wait().unwrap().is_none());

    child.kill().unwrap();
    let status = child
        .wait_timeout(BOUND)
        .unwrap()
        .expect("killed child exits");
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert_eq!(child.try_wait().unwrap(), Some(status), "status is kept");
    child.kill().unwrap();
}

#[test]
fn dropping_a_child_handle_kills_and_reaps_it() {
    let launcher = PluginHostLauncher::start().unwrap();
    let rings = rings();
    let scenario = Scenario::new();
    let executable = executable();
    let child = launcher
        .spawn_with_env(&command(&rings, &executable), &scenario.env("wait"))
        .unwrap();
    ping(&rings);
    let probe = child.exit_fd().try_clone_to_owned().unwrap();
    drop::<PluginHostChild>(child);
    let mut poll = libc::pollfd {
        fd: probe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd` on a live pidfd; zero timeout.
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 0) }, 1, "process exited");
    // SAFETY: `waitid` writes into the zeroed local.
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    // SAFETY: waits on a live pidfd without blocking.
    let result = unsafe {
        libc::waitid(
            libc::P_PIDFD,
            probe.as_raw_fd() as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG,
        )
    };
    assert_eq!(result, -1, "already reaped by the handle");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn failed_exec_is_reported_with_its_stage_and_errno() {
    let launcher = PluginHostLauncher::start().unwrap();
    let rings = rings();
    let missing = std::path::Path::new("/nonexistent/vosu-plugin-host");
    match launcher.spawn(&command(&rings, missing)) {
        Err(SpawnError::Child {
            stage: ChildStage::Exec,
            errno,
        }) => assert_eq!(errno, libc::ENOENT),
        other => panic!("unexpected spawn result {other:?}"),
    }
}

#[test]
fn invalid_commands_are_refused_before_forking() {
    let launcher = PluginHostLauncher::start().unwrap();
    let rings = rings();
    let executable = executable();
    for identity in ["", "with\0nul"] {
        let command = PluginHostCommand {
            build_identity: identity,
            ..command(&rings, &executable)
        };
        assert!(matches!(
            launcher.spawn(&command),
            Err(SpawnError::InvalidCommand(_))
        ));
    }
}

/// This test process's own `Seccomp:` mode from `/proc/self/status`.
fn own_seccomp_mode() -> String {
    std::fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("Seccomp:"))
        .expect("procfs reports Seccomp")
        .trim()
        .to_owned()
}

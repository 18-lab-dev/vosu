//! The plugin host's Linux privilege drop (`docs/v2/plugin-abi.md` §7.3 as
//! amended by A5.1(b), and A5.8's parent-death re-arm).
//!
//! `vosu plugin-host` calls [`arm_parent_death`] right after parsing its
//! arguments and [`drop_privileges`] after mapping its endpoints, both on its
//! one initial thread and before it opens the plugin library. Every step
//! checks its return value; any failure or readback difference is returned
//! as a [`SecurityFailure`] and the caller must exit without loading plugin
//! code. Nothing here retries a weaker sequence, ignores `EPERM`, or stops
//! after clearing only the effective set.
//!
//! `CapBnd` is recorded but not required to be empty: emptying it needs
//! `CAP_SETPCAP`, which setup never grants. `no_new_privs` and the empty
//! inheritable and ambient sets prevent acquiring capabilities in the
//! original user namespace. Only the opt-in filter permits gaining
//! capabilities scoped to a newly created user namespace.

use std::borrow::Cow;
use std::io;

/// `_LINUX_CAPABILITY_VERSION_3`: two 32-bit words per set.
const CAPABILITY_VERSION_3: u32 = 0x2008_0522;
/// `PR_CAP_AMBIENT_CLEAR_ALL`.
const AMBIENT_CLEAR_ALL: libc::c_ulong = 4;

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: libc::c_int,
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
struct CapabilityData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// Why the parent-death step (§7.3 step 1, startup stage `parent`) failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParentDeathError {
    #[error("PR_SET_PDEATHSIG failed with errno {0}")]
    Arm(i32),
    #[error("PR_GET_PDEATHSIG reported signal {0} instead of SIGKILL")]
    Readback(i32),
    #[error("parent process is {actual}, not the launching core {expected}")]
    Mismatch { expected: u32, actual: u32 },
}

/// Arms `SIGKILL` as this process's parent-death signal, then checks that
/// the parent is still `expected_parent`, the core PID named on the command
/// line (§7.3 step 1, A5.8).
///
/// Arming comes first, so a core that dies after the check still kills this
/// process, and a core that died before arming is caught by the check: the
/// orphan has been reparented to another process. The signal fires when the
/// core *thread* that spawned this process exits, which is why core spawns
/// only from its long-lived launcher thread.
pub fn arm_parent_death(expected_parent: u32) -> Result<(), ParentDeathError> {
    // SAFETY: `PR_SET_PDEATHSIG` takes a signal number and no pointers.
    if unsafe {
        libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGKILL as libc::c_ulong,
            0,
            0,
            0,
        )
    } != 0
    {
        return Err(ParentDeathError::Arm(errno()));
    }
    let mut armed: libc::c_int = 0;
    // SAFETY: `PR_GET_PDEATHSIG` writes one `int` through the pointer, which
    // refers to the live local above.
    if unsafe {
        libc::prctl(
            libc::PR_GET_PDEATHSIG,
            &mut armed as *mut libc::c_int,
            0,
            0,
            0,
        )
    } != 0
    {
        return Err(ParentDeathError::Arm(errno()));
    }
    if armed != libc::SIGKILL {
        return Err(ParentDeathError::Readback(armed));
    }
    // SAFETY: `getppid` has no preconditions.
    let actual = unsafe { libc::getppid() };
    if u32::try_from(actual).ok() != Some(expected_parent) {
        return Err(ParentDeathError::Mismatch {
            expected: expected_parent,
            actual: actual.max(0).unsigned_abs(),
        });
    }
    Ok(())
}

/// The step of the §7.3 sequence that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityStep {
    /// The single-thread precondition (`/proc/self/status` `Threads: 1`).
    Threads,
    /// Step 2, `PR_SET_NO_NEW_PRIVS`.
    NoNewPrivs,
    /// Step 3, the user-namespace seccomp filter.
    Seccomp,
    /// Step 4, `PR_CAP_AMBIENT_CLEAR_ALL`.
    Ambient,
    /// Step 5, `capset` with empty sets.
    Capset,
    /// Step 6, the readback.
    Readback,
}

impl SecurityStep {
    /// The step's name in a startup `FAILED` report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Threads => "threads",
            Self::NoNewPrivs => "no_new_privs",
            Self::Seccomp => "seccomp",
            Self::Ambient => "ambient",
            Self::Capset => "capset",
            Self::Readback => "readback",
        }
    }
}

/// The class of a security failure, reported as the startup `code`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityCode {
    /// The environment refuses `no_new_privs` or the filter (no
    /// `CONFIG_SECCOMP_FILTER`, a container policy, an unsupported
    /// architecture) or hides the procfs files the checks read (§7.3).
    Unavailable,
    /// Another thread exists, so the drop could not cover it.
    NotSingleThreaded,
    /// Clearing the ambient or the other capability sets failed.
    DropFailed,
    /// The sequence returned success but the readback differs.
    ReadbackMismatch,
}

impl SecurityCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "security_unavailable",
            Self::NotSingleThreaded => "not_single_threaded",
            Self::DropFailed => "drop_failed",
            Self::ReadbackMismatch => "readback_mismatch",
        }
    }
}

/// A failed or unverifiable privilege drop. The process must exit without
/// opening the plugin library; core marks the plugin `failed` and does not
/// retry (§7.3 "Fail closed").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("plugin-host security step {} failed ({}): {detail}", step.as_str(), code.as_str())]
pub struct SecurityFailure {
    pub step: SecurityStep,
    pub code: SecurityCode,
    /// The failing syscall's `errno`, when one failed.
    pub errno: Option<i32>,
    pub detail: Cow<'static, str>,
}

impl SecurityFailure {
    fn os(step: SecurityStep, code: SecurityCode, detail: &'static str) -> Self {
        Self {
            step,
            code,
            errno: Some(errno()),
            detail: Cow::Borrowed(detail),
        }
    }
}

/// The approved §7.3 seccomp filter variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespacePolicy {
    DenyUserNamespaces,
    AllowUserNamespaces,
}

impl NamespacePolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DenyUserNamespaces => "deny_user_namespaces",
            Self::AllowUserNamespaces => "allow_user_namespaces",
        }
    }
}

/// What the step-6 readback observed. Every field comes from a syscall
/// except the selected policy, the thread count and the last capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityReadback {
    /// `/proc/sys/kernel/cap_last_cap`.
    pub last_cap: u32,
    pub effective: u64,
    pub permitted: u64,
    pub inheritable: u64,
    /// One bit per `PR_CAP_AMBIENT_IS_SET` answer up to `last_cap`.
    pub ambient: u64,
    /// One bit per `PR_CAPBSET_READ` answer. Recorded, never required.
    pub bounding: u64,
    /// `PR_GET_NO_NEW_PRIVS`.
    pub no_new_privs: i32,
    /// `PR_GET_SECCOMP`: 2 is filter mode.
    pub seccomp: i32,
    pub namespace_policy: NamespacePolicy,
    /// The result of probing `unshare(CLONE_NEWUSER)` for the deny filter
    /// or `setns(-1, CLONE_NEWUSER)` for the opt-in filter. `None` means
    /// success (a broken filter).
    pub namespace_probe_errno: Option<i32>,
    /// `getresuid`: real, effective, saved.
    pub uids: [u32; 3],
    pub threads: u32,
}

/// The first field of a [`SecurityReadback`] that breaks the §7.3
/// postcondition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReadbackMismatch {
    #[error("effective set is {0:#x}")]
    Effective(u64),
    #[error("permitted set is {0:#x}")]
    Permitted(u64),
    #[error("inheritable set is {0:#x}")]
    Inheritable(u64),
    #[error("ambient set is {0:#x}")]
    Ambient(u64),
    #[error("no_new_privs is {0}")]
    NoNewPrivs(i32),
    #[error("seccomp mode is {0}, not filter")]
    Seccomp(i32),
    #[error("user-namespace probe returned {0:?} instead of EPERM")]
    UserNamespace(Option<i32>),
    #[error("setns probe returned {0:?} instead of EPERM")]
    Setns(Option<i32>),
    #[error("a UID is root: {0:?}")]
    RootUid([u32; 3]),
    #[error("{0} threads exist")]
    Threads(u32),
}

impl SecurityReadback {
    /// Checks the §7.3 postcondition: empty effective, permitted,
    /// inheritable and ambient sets, `no_new_privs` 1, seccomp filter mode,
    /// the selected filter's probe failing with exactly `EPERM`, no root
    /// UID and one thread. `bounding` is not checked (A5.1(b)).
    pub fn verify(&self) -> Result<(), ReadbackMismatch> {
        if self.effective != 0 {
            return Err(ReadbackMismatch::Effective(self.effective));
        }
        if self.permitted != 0 {
            return Err(ReadbackMismatch::Permitted(self.permitted));
        }
        if self.inheritable != 0 {
            return Err(ReadbackMismatch::Inheritable(self.inheritable));
        }
        if self.ambient != 0 {
            return Err(ReadbackMismatch::Ambient(self.ambient));
        }
        if self.no_new_privs != 1 {
            return Err(ReadbackMismatch::NoNewPrivs(self.no_new_privs));
        }
        if self.seccomp != libc::SECCOMP_MODE_FILTER as i32 {
            return Err(ReadbackMismatch::Seccomp(self.seccomp));
        }
        if self.namespace_probe_errno != Some(libc::EPERM) {
            let errno = self.namespace_probe_errno;
            return Err(match self.namespace_policy {
                NamespacePolicy::DenyUserNamespaces => ReadbackMismatch::UserNamespace(errno),
                NamespacePolicy::AllowUserNamespaces => ReadbackMismatch::Setns(errno),
            });
        }
        if self.uids.contains(&0) {
            return Err(ReadbackMismatch::RootUid(self.uids));
        }
        if self.threads != 1 {
            return Err(ReadbackMismatch::Threads(self.threads));
        }
        Ok(())
    }
}

/// Runs §7.3 steps 2–6 on the calling (only) thread: `no_new_privs`, the
/// user-namespace filter, ambient clear, empty `capset`, then the readback.
/// Returns the verified readback, or the first failure. On failure the
/// process may already be partly confined; the caller reports and exits.
pub fn drop_privileges(policy: NamespacePolicy) -> Result<SecurityReadback, SecurityFailure> {
    let threads = thread_count()?;
    if threads != 1 {
        return Err(SecurityFailure {
            step: SecurityStep::Threads,
            code: SecurityCode::NotSingleThreaded,
            errno: None,
            detail: Cow::Owned(format!("{threads} threads exist before the drop")),
        });
    }
    // SAFETY: `PR_SET_NO_NEW_PRIVS` takes integer arguments only.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(SecurityFailure::os(
            SecurityStep::NoNewPrivs,
            SecurityCode::Unavailable,
            "PR_SET_NO_NEW_PRIVS failed",
        ));
    }
    seccomp::install(policy)?;
    // SAFETY: `PR_CAP_AMBIENT_CLEAR_ALL` takes integer arguments only.
    if unsafe { libc::prctl(libc::PR_CAP_AMBIENT, AMBIENT_CLEAR_ALL, 0, 0, 0) } != 0 {
        return Err(SecurityFailure::os(
            SecurityStep::Ambient,
            SecurityCode::DropFailed,
            "PR_CAP_AMBIENT_CLEAR_ALL failed",
        ));
    }
    let header = CapabilityHeader {
        version: CAPABILITY_VERSION_3,
        pid: 0,
    };
    let empty = [CapabilityData::default(); 2];
    // SAFETY: version-3 `capset` reads one header and two data words, all
    // live locals.
    if unsafe { libc::syscall(libc::SYS_capset, &header, empty.as_ptr()) } != 0 {
        return Err(SecurityFailure::os(
            SecurityStep::Capset,
            SecurityCode::DropFailed,
            "capset with empty sets failed",
        ));
    }
    let readback = read_back(policy)?;
    readback.verify().map_err(|mismatch| SecurityFailure {
        step: SecurityStep::Readback,
        code: SecurityCode::ReadbackMismatch,
        errno: None,
        detail: Cow::Owned(mismatch.to_string()),
    })?;
    Ok(readback)
}

/// Step 6's observations. Does not judge them; see
/// [`SecurityReadback::verify`].
fn read_back(policy: NamespacePolicy) -> Result<SecurityReadback, SecurityFailure> {
    let last_cap = last_cap()?;
    let mut header = CapabilityHeader {
        version: CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData::default(); 2];
    // SAFETY: version-3 `capget` writes two data words into the live local
    // array and may rewrite the header's version.
    if unsafe { libc::syscall(libc::SYS_capget, &mut header, data.as_mut_ptr()) } != 0 {
        return Err(readback_os("capget failed"));
    }
    let set = |pick: fn(&CapabilityData) -> u32| {
        u64::from(pick(&data[0])) | (u64::from(pick(&data[1])) << 32)
    };
    let mut ambient = 0_u64;
    let mut bounding = 0_u64;
    for cap in 0..=last_cap {
        let bit = 1_u64 << cap;
        // SAFETY: `PR_CAP_AMBIENT_IS_SET` takes integer arguments only.
        match unsafe {
            libc::prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_IS_SET as libc::c_ulong,
                libc::c_ulong::from(cap),
                0,
                0,
            )
        } {
            0 => {}
            1 => ambient |= bit,
            _ => return Err(readback_os("PR_CAP_AMBIENT_IS_SET failed")),
        }
        // SAFETY: `PR_CAPBSET_READ` takes integer arguments only.
        match unsafe { libc::prctl(libc::PR_CAPBSET_READ, libc::c_ulong::from(cap), 0, 0, 0) } {
            0 => {}
            1 => bounding |= bit,
            _ => return Err(readback_os("PR_CAPBSET_READ failed")),
        }
    }
    // SAFETY: integer arguments only.
    let no_new_privs = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    if no_new_privs < 0 {
        return Err(readback_os("PR_GET_NO_NEW_PRIVS failed"));
    }
    // SAFETY: integer arguments only.
    let seccomp = unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) };
    if seccomp < 0 {
        return Err(readback_os("PR_GET_SECCOMP failed"));
    }
    let namespace_probe_errno = namespace_probe(policy);
    let (mut real, mut effective, mut saved) = (0, 0, 0);
    // SAFETY: `getresuid` writes one `uid_t` through each live local.
    if unsafe { libc::getresuid(&mut real, &mut effective, &mut saved) } != 0 {
        return Err(readback_os("getresuid failed"));
    }
    Ok(SecurityReadback {
        last_cap,
        effective: set(|data| data.effective),
        permitted: set(|data| data.permitted),
        inheritable: set(|data| data.inheritable),
        ambient,
        bounding,
        no_new_privs,
        seccomp,
        namespace_policy: policy,
        namespace_probe_errno,
        uids: [real, effective, saved],
        threads: thread_count()?,
    })
}

/// The default probe is unchanged. The opt-in probe uses an invalid fd:
/// the kernel would return EBADF without the filter, and the call cannot
/// change namespaces even if the filter is wrong. Never create a user
/// namespace in the bootstrap thread on the opt-in path.
fn namespace_probe(policy: NamespacePolicy) -> Option<i32> {
    let result = match policy {
        NamespacePolicy::DenyUserNamespaces => {
            // SAFETY: `unshare` takes integer flags only. On unexpected
            // success verification fails and the host exits before loading.
            unsafe { libc::unshare(libc::CLONE_NEWUSER) }
        }
        NamespacePolicy::AllowUserNamespaces => {
            // SAFETY: integer arguments only; -1 cannot name a live fd.
            unsafe { libc::setns(-1, libc::CLONE_NEWUSER) }
        }
    };
    (result != 0).then(errno)
}

fn readback_os(detail: &'static str) -> SecurityFailure {
    SecurityFailure::os(SecurityStep::Readback, SecurityCode::DropFailed, detail)
}

/// The highest capability number this kernel supports. Unreadable or out of
/// the 64-bit range the version-3 sets cover fails closed.
fn last_cap() -> Result<u32, SecurityFailure> {
    let text = std::fs::read_to_string("/proc/sys/kernel/cap_last_cap")
        .map_err(|error| procfs_failure(SecurityStep::Readback, "cap_last_cap", &error))?;
    match text.trim().parse::<u32>() {
        Ok(last) if last < 64 => Ok(last),
        _ => Err(SecurityFailure {
            step: SecurityStep::Readback,
            code: SecurityCode::Unavailable,
            errno: None,
            detail: Cow::Owned(format!("unusable cap_last_cap {:?}", text.trim())),
        }),
    }
}

/// This process's thread count from `/proc/self/status`. Unreadable procfs
/// fails closed.
fn thread_count() -> Result<u32, SecurityFailure> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|error| procfs_failure(SecurityStep::Threads, "/proc/self/status", &error))?;
    parse_threads(&status).ok_or(SecurityFailure {
        step: SecurityStep::Threads,
        code: SecurityCode::Unavailable,
        errno: None,
        detail: Cow::Borrowed("/proc/self/status has no Threads line"),
    })
}

fn parse_threads(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|value| value.trim().parse().ok())
}

fn procfs_failure(step: SecurityStep, what: &str, error: &io::Error) -> SecurityFailure {
    SecurityFailure {
        step,
        code: SecurityCode::Unavailable,
        errno: error.raw_os_error(),
        detail: Cow::Owned(format!("cannot read {what}: {error}")),
    }
}

fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

pub(super) mod seccomp {
    //! §7.3 step 3: classic-BPF seccomp filters; only the default variant
    //! denies namespace creation. Both deny `setns` and foreign syscall ABIs.

    use std::borrow::Cow;

    use super::{NamespacePolicy, SecurityCode, SecurityFailure, SecurityStep};

    /// `offsetof(struct seccomp_data, nr)`.
    const NR_OFFSET: u32 = 0;
    /// `offsetof(struct seccomp_data, arch)`.
    const ARCH_OFFSET: u32 = 4;
    /// The low 32 bits of `args[0]` on a little-endian architecture.
    const ARG0_LOW_OFFSET: u32 = 16;

    /// The native syscall ABI the filter admits.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct FilterArch {
        /// `AUDIT_ARCH_*` of the native ABI.
        pub(crate) audit_arch: u32,
        /// Syscall numbers at or above this belong to another table on the
        /// same audit architecture (x86_64's x32 bit).
        pub(crate) foreign_nr_floor: Option<u32>,
    }

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    pub(crate) const NATIVE: Option<FilterArch> = Some(FilterArch {
        audit_arch: 0xC000_003E, // AUDIT_ARCH_X86_64
        foreign_nr_floor: Some(0x4000_0000),
    });
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    pub(crate) const NATIVE: Option<FilterArch> = Some(FilterArch {
        audit_arch: 0xC000_00B7, // AUDIT_ARCH_AARCH64
        foreign_nr_floor: None,
    });
    /// Unsupported architectures cannot install a verified filter and fail
    /// with `security_unavailable`.
    #[cfg(not(any(
        all(target_arch = "x86_64", target_endian = "little"),
        all(target_arch = "aarch64", target_endian = "little")
    )))]
    pub(crate) const NATIVE: Option<FilterArch> = None;

    /// The syscalls the filter singles out.
    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Syscalls {
        pub(crate) clone: u32,
        pub(crate) clone3: u32,
        pub(crate) unshare: u32,
        pub(crate) setns: u32,
    }

    pub(crate) const SYSCALLS: Syscalls = Syscalls {
        clone: libc::SYS_clone as u32,
        clone3: libc::SYS_clone3 as u32,
        unshare: libc::SYS_unshare as u32,
        setns: libc::SYS_setns as u32,
    };

    /// Where a conditional jump goes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Target {
        Next,
        CheckFlags,
        Allow,
        Eperm,
        Enosys,
        Kill,
    }

    /// The filter program for `arch`:
    ///
    /// 1. a foreign audit architecture, or a native number at or above the
    ///    foreign floor, kills the process;
    /// 2. `clone3` fails with `ENOSYS` (its flags are in user memory the
    ///    filter cannot read; libc falls back to `clone`);
    /// 3. `setns` fails with `EPERM`;
    /// 4. `unshare` and `clone` whose flags include `CLONE_NEWUSER` fail
    ///    with `EPERM`;
    /// 5. everything else is allowed.
    ///
    /// The opt-in variant skips 2 and 4; it retains 1, 3 and 5.
    pub(crate) fn program(
        arch: FilterArch,
        syscalls: Syscalls,
        policy: NamespacePolicy,
    ) -> Vec<libc::sock_filter> {
        let mut code: Vec<(u16, u32, Target, Target)> = Vec::with_capacity(16);
        let load = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
        let jeq = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
        let jge = (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16;
        let jset = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
        code.push((load, ARCH_OFFSET, Target::Next, Target::Next));
        code.push((jeq, arch.audit_arch, Target::Next, Target::Kill));
        code.push((load, NR_OFFSET, Target::Next, Target::Next));
        if let Some(floor) = arch.foreign_nr_floor {
            code.push((jge, floor, Target::Kill, Target::Next));
        }
        let deny_creation = policy == NamespacePolicy::DenyUserNamespaces;
        if deny_creation {
            code.push((jeq, syscalls.clone3, Target::Enosys, Target::Next));
            code.push((jeq, syscalls.setns, Target::Eperm, Target::Next));
            code.push((jeq, syscalls.unshare, Target::CheckFlags, Target::Next));
            code.push((jeq, syscalls.clone, Target::CheckFlags, Target::Allow));
        } else {
            code.push((jeq, syscalls.setns, Target::Eperm, Target::Allow));
        }
        let check_flags = code.len();
        if deny_creation {
            code.push((load, ARG0_LOW_OFFSET, Target::Next, Target::Next));
            code.push((
                jset,
                libc::CLONE_NEWUSER as u32,
                Target::Eperm,
                Target::Allow,
            ));
        }
        let ret = (libc::BPF_RET | libc::BPF_K) as u16;
        let allow = code.len();
        let eperm = allow + 1;
        let enosys = allow + 2;
        let kill = enosys + usize::from(deny_creation);
        let resolve = |from: usize, target: Target| -> u8 {
            let to = match target {
                Target::Next => from + 1,
                Target::CheckFlags => check_flags,
                Target::Allow => allow,
                Target::Eperm => eperm,
                Target::Enosys => enosys,
                Target::Kill => kill,
            };
            u8::try_from(to - from - 1).expect("filter jumps are short and forward")
        };
        let mut program: Vec<libc::sock_filter> = code
            .iter()
            .enumerate()
            .map(|(index, &(op, k, yes, no))| libc::sock_filter {
                code: op,
                jt: resolve(index, yes),
                jf: resolve(index, no),
                k,
            })
            .collect();
        let actions = [
            libc::SECCOMP_RET_ALLOW,
            libc::SECCOMP_RET_ERRNO | libc::EPERM as u32,
        ]
        .into_iter()
        .chain(deny_creation.then_some(libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32))
        .chain([libc::SECCOMP_RET_KILL_PROCESS]);
        for k in actions {
            program.push(libc::sock_filter {
                code: ret,
                jt: 0,
                jf: 0,
                k,
            });
        }
        program
    }

    /// Installs [`program`] for this architecture on every thread (TSYNC;
    /// the caller has verified there is only one). Requires
    /// `no_new_privs`.
    pub(super) fn install(policy: NamespacePolicy) -> Result<(), SecurityFailure> {
        let Some(arch) = NATIVE else {
            return Err(SecurityFailure {
                step: SecurityStep::Seccomp,
                code: SecurityCode::Unavailable,
                errno: None,
                detail: Cow::Borrowed("no seccomp filter for this architecture"),
            });
        };
        let mut program = program(arch, SYSCALLS, policy);
        let filter = libc::sock_fprog {
            len: u16::try_from(program.len()).expect("the filter has a few dozen instructions"),
            filter: program.as_mut_ptr(),
        };
        // SAFETY: `seccomp(SET_MODE_FILTER)` reads `filter`, which points at
        // `program`'s live instructions; the kernel copies the program.
        let result = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                libc::SECCOMP_FILTER_FLAG_TSYNC,
                &filter as *const libc::sock_fprog,
            )
        };
        match result {
            0 => Ok(()),
            // With TSYNC, a positive result names a thread that could not
            // be synchronized.
            thread if thread > 0 => Err(SecurityFailure {
                step: SecurityStep::Seccomp,
                code: SecurityCode::NotSingleThreaded,
                errno: None,
                detail: Cow::Owned(format!("thread {thread} could not take the filter")),
            }),
            _ => Err(SecurityFailure::os(
                SecurityStep::Seccomp,
                SecurityCode::Unavailable,
                "seccomp filter installation failed",
            )),
        }
    }
}

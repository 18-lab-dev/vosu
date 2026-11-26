//! The `vosu plugin-host` child entry (`docs/v2/plugin-abi.md` §7.3, §7.4,
//! §7.6 "Launch" and "Startup").
//!
//! `src/main.rs` calls [`run`] when `argv[1]` is exactly `plugin-host`,
//! before `vosux`, before any Tokio runtime or other thread exists, and
//! before any daemon service, listener, updater, discovery, ownership lock
//! or log file. Nothing here reaches those either: the child is this
//! module, [`super::bootstrap`] and the plugin.
//!
//! Before its channel exists the child reports only through its exit
//! status (`control::EXIT_*`):
//!
//! 1. parse exactly the §7.6 arguments, refusing anything else
//!    (`arguments`), including a build identity other than this
//!    executable's own (`generation`);
//! 2. Linux: re-arm the parent-death signal and check the parent is the
//!    core PID on the command line (`parent`);
//! 3. adopt and map exactly the inherited endpoints (`endpoints`);
//! 4. Linux: the §7.3 privilege drop, reported as `FAILED` stage
//!    `security` over the channel. Windows: job assignment already
//!    happened before core resumed this process.
//!
//! [`super::bootstrap`] then reads `CREATE` and loads the plugin.

use std::ffi::{OsStr, OsString};

#[cfg(target_os = "linux")]
use vosu_sys::plugin_process::{NamespacePolicy, USER_NAMESPACES_ARGUMENT};

use super::control::{EXIT_ARGUMENTS, EXIT_GENERATION};

/// The exact `argv[1]` of a plugin host.
pub const COMMAND: &str = "plugin-host";

/// Whether `args` (the full `argv`, program first) asks for the plugin-host
/// entry: `argv[1]` is exactly [`COMMAND`].
pub fn is_plugin_host(args: &[OsString]) -> bool {
    args.get(1).is_some_and(|arg| arg == COMMAND)
}

/// Runs the plugin host to completion and returns its exit status. `args`
/// is the full `argv`; `build_identity` is this executable's compile-time
/// identity, which core passed as the first argument after the command.
pub fn run(args: &[OsString], build_identity: &str) -> i32 {
    match parse(args, build_identity) {
        Ok(arguments) => platform::start(arguments),
        Err(code) => code,
    }
}

/// The validated arguments after `plugin-host`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Arguments<'a> {
    /// The core PID the parent-death check expects (Linux only).
    #[cfg(target_os = "linux")]
    pub parent: u32,
    #[cfg(target_os = "linux")]
    pub namespace_policy: NamespacePolicy,
    pub endpoints: &'a OsStr,
}

/// Linux: `<identity> <core pid> [--allow-user-namespaces] <endpoints>`.
/// Windows: `<identity> <endpoints>`.
#[cfg(target_os = "linux")]
const ARGUMENT_COUNT: usize = 5;
#[cfg(not(target_os = "linux"))]
const ARGUMENT_COUNT: usize = 4;

/// Checks the exact argument shape, then the build identity. Returns the
/// exit status of the stage that refused them.
pub(super) fn parse<'a>(args: &'a [OsString], build_identity: &str) -> Result<Arguments<'a>, i32> {
    #[cfg(target_os = "linux")]
    let namespace_policy = match args.len() {
        ARGUMENT_COUNT => NamespacePolicy::DenyUserNamespaces,
        n if n == ARGUMENT_COUNT + 1 && args[4] == USER_NAMESPACES_ARGUMENT => {
            NamespacePolicy::AllowUserNamespaces
        }
        _ => return Err(EXIT_ARGUMENTS),
    };
    #[cfg(not(target_os = "linux"))]
    if args.len() != ARGUMENT_COUNT {
        return Err(EXIT_ARGUMENTS);
    }
    if !is_plugin_host(args) {
        return Err(EXIT_ARGUMENTS);
    }
    let identity = args[2].to_str().ok_or(EXIT_ARGUMENTS)?;
    if identity.is_empty() {
        return Err(EXIT_ARGUMENTS);
    }
    #[cfg(target_os = "linux")]
    let parent = canonical_pid(&args[3]).ok_or(EXIT_ARGUMENTS)?;
    let endpoints = args[args.len() - 1].as_os_str();
    if endpoints.is_empty() {
        return Err(EXIT_ARGUMENTS);
    }
    if identity != build_identity {
        return Err(EXIT_GENERATION);
    }
    Ok(Arguments {
        #[cfg(target_os = "linux")]
        parent,
        #[cfg(target_os = "linux")]
        namespace_policy,
        endpoints,
    })
}

/// A positive canonical decimal PID: no sign, no leading zero, no
/// whitespace, within `pid_t`.
#[cfg(target_os = "linux")]
fn canonical_pid(arg: &OsStr) -> Option<u32> {
    let text = arg.to_str()?;
    let canonical = !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && !text.starts_with('0');
    if !canonical {
        return None;
    }
    let pid: u32 = text.parse().ok()?;
    (pid <= i32::MAX as u32).then_some(pid)
}

#[cfg(target_os = "linux")]
mod platform {
    use vosu_sys::plugin_process::{arm_parent_death, drop_privileges};

    use super::super::bootstrap;
    use super::super::control::{EXIT_ENDPOINTS, EXIT_PARENT, StartupStage};
    use super::super::linux::adopt_channel;
    use super::{Arguments, NamespacePolicy};

    pub(super) fn start(arguments: Arguments<'_>) -> i32 {
        if arm_parent_death(arguments.parent).is_err() {
            return EXIT_PARENT;
        }
        let Ok(endpoint) = adopt_channel(arguments.endpoints) else {
            return EXIT_ENDPOINTS;
        };
        // Still the one initial thread: nothing above starts one.
        if let Err(failure) = drop_privileges(arguments.namespace_policy) {
            let policy = match arguments.namespace_policy {
                NamespacePolicy::DenyUserNamespaces => "",
                NamespacePolicy::AllowUserNamespaces => " (allow_user_namespaces)",
            };
            return bootstrap::report_before_create(
                &endpoint,
                StartupStage::Security,
                failure.code.as_str(),
                failure.errno,
                &format!("{}{policy}: {failure}", failure.step.as_str()),
            );
        }
        bootstrap::run(endpoint)
    }
}

#[cfg(windows)]
mod platform {
    use vosu_sys::plugin_process::suppress_crash_reporting_hold;

    use super::super::bootstrap;
    use super::super::control::{EXIT_ARGUMENTS, EXIT_ENDPOINTS};
    use super::super::windows_launch::attach_inherited;
    use super::Arguments;

    pub(super) fn start(arguments: Arguments<'_>) -> i32 {
        // Before any plugin code can run: a crash must exit at once, not
        // be held by Windows Error Reporting past the startup deadline
        // (§7.4 "Crash reporting").
        suppress_crash_reporting_hold();
        let Some(endpoints) = arguments.endpoints.to_str() else {
            return EXIT_ARGUMENTS;
        };
        let Ok(endpoint) = attach_inherited(endpoints) else {
            return EXIT_ENDPOINTS;
        };
        bootstrap::run(endpoint)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::super::control::EXIT_ENDPOINTS;
    use super::Arguments;

    /// No process runtime on this target: there are no endpoints to adopt.
    pub(super) fn start(_arguments: Arguments<'_>) -> i32 {
        EXIT_ENDPOINTS
    }
}

#[cfg(test)]
mod tests;

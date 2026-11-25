//! Windows objects behind one ring direction: an unnamed pagefile-backed
//! section view and an unnamed auto-reset event doorbell
//! (`docs/v2/plugin-abi.md` §7.2); and the plugin host's race-free launch
//! into a strict kill-on-close job with exactly those objects inherited
//! (§7.4).

mod channel;
mod job;
mod objects;
mod spawn;

pub use channel::{EndpointError, EndpointValues, InheritedEndpoints, adopt_inherited};
pub use job::{CurrentJob, JobProcessIds, PLUGIN_JOB_LIMIT_FLAGS, PluginJob, current_job};
pub use objects::Doorbell;
pub(super) use objects::Mapping;
pub(super) use objects::{adopt_mapping, create_mapping};
pub use spawn::{
    KILL_EXIT_CODE, PluginHostProcess, SpawnError, SpawnStep, UNWIND_EXIT_CODE, UNWIND_REAP_BOUND,
    UnwoundChild, spawn_plugin_host,
};

/// Keeps Windows Error Reporting from holding this process after it
/// crashes (`docs/v2/plugin-abi.md` §7.4 "Crash reporting"). The plugin
/// host calls it first, before it maps its channel or loads any plugin
/// code, so a crash in a library constructor, `create` or `dispatch`
/// exits at once and core classifies it as a crash, not a startup timeout
/// or a late loss.
///
/// Measured in the Windows VM: a real `std::process::abort` fail-fast was
/// held 1.2–5.6 s before the process exited, and an access violation
/// 1.3–5.8 s. With `SEM_NOGPFAULTERRORBOX` set by the process itself, both
/// exited within about 125 ms of the fault. The job limit
/// `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION` ended the access violation
/// promptly but not the fail-fast, so it is not used.
///
/// `SEM_FAILCRITICALERRORS` also keeps a critical-error dialog from
/// blocking a library load inside the startup deadline. Both flags are
/// inherited by processes the plugin starts. The previous mode's other
/// flags are kept.
pub fn suppress_crash_reporting_hold() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };
    // SAFETY: plain calls that read and set this process's error mode; no
    // pointer or handle is involved.
    unsafe {
        SetErrorMode(GetErrorMode() | SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
}

#[cfg(test)]
mod tests;

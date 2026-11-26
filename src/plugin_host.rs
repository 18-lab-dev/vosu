//! The `vosu plugin-host` command (`docs/v2/plugin-abi.md` §7.6 "Launch"):
//! the child process core starts once per `runtime = "process"` plugin
//! instance. `src/main.rs` dispatches it before anything else; the entry
//! itself lives in `vosu_core::plugin::process::entry`.

use std::ffi::OsString;

/// Runs the plugin host when `args` (the full `argv`) names it, returning
/// its exit status; `None` for every other command line.
pub fn dispatch(args: &[OsString]) -> Option<i32> {
    use vosu_core::plugin::process::entry;
    entry::is_plugin_host(args).then(|| entry::run(args, vosu::BUNDLE.plugin_host_identity))
}

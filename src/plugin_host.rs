//! The `vosu plugin-host` command (`docs/v2/plugin-abi.md` §7.6 "Launch"):
//! the child process core starts once per `runtime = "process"` plugin
//! instance. `src/main.rs` dispatches it before anything else; the entry
//! itself lives in `vosu_core::plugin::process::entry`.

use std::ffi::OsString;

/// This executable's build identity, passed by core as the first argument
/// after `plugin-host` and compared by the child with its own. Every
/// release has its own version, so a child started from a replaced
/// executable (an in-place update between core's start and a launch)
/// refuses with stage `generation` instead of mixing generations.
pub const BUILD_IDENTITY: &str = concat!("vosu/", env!("CARGO_PKG_VERSION"), "/plugin-host-1");

/// Runs the plugin host when `args` (the full `argv`) names it, returning
/// its exit status; `None` for every other command line.
pub fn dispatch(args: &[OsString]) -> Option<i32> {
    use vosu_core::plugin::process::entry;
    entry::is_plugin_host(args).then(|| entry::run(args, BUILD_IDENTITY))
}

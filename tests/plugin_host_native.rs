//! Native `vosu plugin-host` evidence (`docs/v2/plugin-abi.md` §7.2–§7.6,
//! M5): the real `vosu` executable started as a plugin host by core's real
//! launcher, on Linux and on Windows. A thin root: each M5 unit owns child
//! modules under `tests/plugin_host_native/`, and every test name starts
//! with its module path (`docs/testing/verification.md`, "M5 test and
//! fixture conventions").
#![cfg(feature = "test-harness")]
#![cfg(any(target_os = "linux", windows))]
#![expect(
    clippy::disallowed_methods,
    reason = "fixtures write scratch packages and compile sources, and bounded marker waits sleep between checks"
)]

#[path = "plugin_host_native/actions.rs"]
mod actions;
#[path = "plugin_host_native/approval.rs"]
mod approval;
#[path = "plugin_host_native/backpressure.rs"]
mod backpressure;
#[path = "plugin_host_native/broker.rs"]
mod broker;
#[path = "plugin_host_native/crash.rs"]
mod crash;
#[path = "plugin_host_native/lifecycle.rs"]
mod lifecycle;
#[cfg(target_os = "linux")]
#[path = "plugin_host_native/linux_security.rs"]
mod linux_security;
#[path = "plugin_host_native/parent_death.rs"]
mod parent_death;
#[path = "plugin_host_native/parity.rs"]
mod parity;
#[path = "plugin_host_native/relay.rs"]
mod relay;
#[path = "plugin_host_native/resources.rs"]
mod resources;
#[path = "plugin_host_native/restart.rs"]
mod restart;
#[path = "plugin_host_native/state.rs"]
mod state;
#[path = "plugin_host_native/support/mod.rs"]
mod support;
#[path = "plugin_host_native/sync.rs"]
mod sync;
#[cfg(windows)]
#[path = "plugin_host_native/windows_job.rs"]
mod windows_job;

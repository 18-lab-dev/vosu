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

#[path = "plugin_host_native/lifecycle.rs"]
mod lifecycle;

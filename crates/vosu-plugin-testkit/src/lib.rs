//! vosu 2.0 plugin-host test fixtures, test-harness only
//! (`docs/v2/implementation-plan.md` section 2, the `vosu-plugin-testkit`
//! row): a bundled-form serialized echo plugin, the same type's `cdylib`
//! export for out-of-process-style `dlopen` tests, a `concurrent_dispatch`
//! echo variant, and a host-services plugin exercising M1's timers and
//! rate-limited logging end to end. Actions, streams, and the synthetic
//! game are out of scope for this phase.
#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

#[cfg(feature = "test-harness")]
pub mod concurrent;
#[cfg(feature = "test-harness")]
pub mod echo;
#[cfg(feature = "test-harness")]
pub mod host_services;

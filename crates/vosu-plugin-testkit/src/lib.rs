//! vosu 2.0 plugin-host test fixtures, test-harness only
//! (`docs/v2/implementation-plan.md` section 2, the `vosu-plugin-testkit`
//! row): a bundled-form serialized echo plugin, the same type's `cdylib`
//! export for out-of-process-style `dlopen` tests, a `concurrent_dispatch`
//! echo variant, a host-services plugin exercising M1's timers and
//! rate-limited logging end to end, a session/focus plugin identifying
//! every session it is attached to and reporting
//! `SESSION_ATTACHED`/`SESSION_DETACHED`/`FOCUS_CHANGED` back to a test
//! (`session.rs`), a read-plan plugin driving async and
//! `VOSU_READ_PLAN_SYNC` submissions from a test-controlled background
//! thread (`read_plan.rs`), a signature-scan plugin driving
//! `VOSU_OP_SCAN_REQUEST` submissions the same way (`scan.rs`), a
//! `concurrent_dispatch` state-bus publisher driving
//! `VOSU_OP_PUBLISH_FIELD`/`VOSU_OP_PUBLISH_BATCH` submissions from
//! several test-controlled writer threads at once (`publisher.rs`), and
//! a `VOSU_OP_STATE_SUBSCRIBE` subscriber reassembling split
//! `VOSU_OP_STATE_UPDATE` deliveries (`subscriber.rs`), plus instance-owned
//! resource/action scripts and a development API fixture (`fixture.rs`).
#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

#[cfg(feature = "bench-harness")]
pub mod actions;
#[cfg(feature = "test-harness")]
pub mod concurrent;
#[cfg(feature = "test-harness")]
pub mod echo;
#[cfg(feature = "bench-harness")]
pub mod fixture;
#[cfg(feature = "test-harness")]
pub mod host_services;
#[cfg(feature = "bench-harness")]
pub mod publisher;
#[cfg(feature = "test-harness")]
pub mod read_plan;
#[cfg(feature = "bench-harness")]
pub mod resources;
#[cfg(feature = "test-harness")]
pub mod scan;
#[cfg(feature = "test-harness")]
pub mod session;
#[cfg(feature = "test-harness")]
pub mod subscriber;

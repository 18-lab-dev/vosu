//! End-to-end read-plan routing tests, driven through a real
//! [`vosu_plugin_testkit::read_plan::ReadPlanPlugin`] instance (never
//! `read_plan.rs`/this module's own functions called directly), against
//! the test process's own memory exactly as `tests/process_memory.rs`
//! does for `vosu-sys`. Split into topic modules so each stays under the
//! maintained-file line limit (`docs/architecture/source-map.md`).

/// Shared fixtures every submodule here uses: a test `ReadPlanPlugin`
/// instance, event/status helpers, and the gated/slow/dying
/// `ProcessMemory` fakes.
mod helpers;

/// Core routing behavior: async/dependent-stage reads, permission
/// checks, module-table caching, open-error classification, async
/// per-read NotReady/timeout delivery (P1-3), and the synchronous
/// `ref_op`-width admission check (F6).
mod basics;

/// `VOSU_READ_PLAN_SYNC` (D5, §6.3): dispatch-thread refusal, the
/// whole-call `VOSU_ERR_TIMED_OUT`/`VOSU_ERR_TARGET_GONE` errors (F2),
/// and `VOSU_ERR_NOT_READY` for a torn-down instance.
mod sync;

/// The shared background pool: the 64-outstanding-async-plan cap,
/// `Weak<PluginHost>` self-join safety (P1-2), fairness across
/// instances (P2-7), and cancel-before-detach ordering (F6).
mod pool;

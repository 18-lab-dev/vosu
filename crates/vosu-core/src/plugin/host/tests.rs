/// Shared fixtures: a test `PluginHost`/logger/settings wiring, session
/// testkit manifests, and `ProcessRecord` builders every other submodule
/// here uses. Split out of `tests.rs` so each topic module stays under
/// the maintained-file line limit (`docs/architecture/source-map.md`).
mod helpers;

/// `host::read_plans::tests` and `host::scans::tests` build their own
/// `PluginHost` fixtures through these, same as every submodule here.
pub(super) use helpers::{test_host, test_process};

/// Focus-rule tests: attach/identify/focus ordering, loss-before-gain on
/// a foreground switch, `foreground_changed()`'s own re-run, process-exit
/// recomputation, `focus_priority`, and concurrent-recomputation safety.
mod focus;

/// Session lifecycle tests: decline re-offering, a failed `create`
/// unregistering its matcher, and the identification-timeout sweep.
mod sessions;

/// The two P2 shutdown/timer races: a `TIMER_SET` made during `create`
/// queued and replayed once `create` returns, and `destroy_all` not
/// leaking an instance still racing `create_instance`.
mod races;

/// The process execution seam with a recording launcher: per-instance
/// endpoints, readiness failure, loss retirement, bundled isolation.
mod process_backend;

mod resource_composition;
/// P0 review: `create_instance` building `init.settings` from the
/// loaded plugin's own schema, and `PluginHost::schema_for` (plugin-id
/// match, `Creating`-and-`Live` resolution).
mod settings_schema;
mod state;
mod state_subscribe;
/// M5 C0: process supervision — backoff restart as a new generation,
/// stop and shutdown during the delay, the per-instance teardown seam and
/// the manual restart of a failed initial load.
mod supervision;

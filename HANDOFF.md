# M7 C0 handoff (branch `m7/c0`, base `69c499ed`)

## Done

1. `e9374131` — the legacy filter **value type** moved to
   `vosu_surface::filter::{Filter, MAX_FILTER_NODES, MAX_FILTER_DEPTH,
   within_limits}`. `vosu-adapter-support` keeps the JS-compatible parser
   and projection semantics and imports the type (new edge
   `vosu-adapter-support -> vosu-surface`, recorded in
   `scripts/crate-boundaries.txt`; the M4a rule in
   `scripts/check-crate-boundaries.py` now allows exactly that target).
   `FeedControl::replace_filters` decodes the canonical serialized plan
   (bounded by the same limits) instead of re-running the legacy parser.
2. `fe604bb7` — `crates/vosu-core/src/server/` now owns `api_keys`, `api_v1`,
   `auth`, `broker`, `catalog_api`, `files`, `ids`, `origins`, `pages`,
   `routes`, `runtime` (listeners), `subscription_wire`, `websocket`.
   No core edge to any bundled crate:
   - `CalculatePpProvider::calculate_pp(Option<&str>)`; the provider parses
     (`CalculateResult::Invalid` keeps the 400 envelope). PpQuery tests moved
     to `crates/vosu-adapter-upstream/src/calculate/tests.rs`.
   - `RouteDelegate::adapter_capture() -> Option<&dyn HarnessRoute>`.
   - API-v1 frames use `crate::state::ecma_number::write_js_number_to`.
   - Root `src/server.rs` keeps only `bus_provider`, `adapter_capture`
     (test-harness), `overlay_folders`, `service`. Tests/benches/examples
     import `vosu_core::server::…` for the moved modules.

## Remaining C0 work, in order

1. **`bus_provider` + `adapter_capture` → core.** Generic over
   `Projection<Filter, Bytes>`; blocked only by their tests, which build real
   adapters (`src/server/bus_provider/tests.rs`, `adapter_capture/tests.rs`,
   `adapter_capture/endpoint/tests.rs`). Rehome those tests to a root
   integration test (`tests/`) using only the public API, then move.
2. **Bundle descriptor (core side).** Add `vosu_core::runtime::bundle`:
   `Bundle { version: &'static str, plugin_host_identity: &'static str,
   plugins: &'static [BundledPlugin<'static>], hosts: &'static dyn
   BundledHosts }`, passed to `run_cli(&BUNDLE)` by `src/main.rs`.
   `BundledHosts` (implemented in root `src/plugins.rs`) covers exactly what
   today's runtime names directly:
   - adapters: `start(host, config, dashboard) -> (Arc<dyn AdapterSet>,
     WebSocketHubs)` where `AdapterSet: StateJsonProvider +
     WebSocketStateProvider (+ HarnessRoute source)` with `router`,
     `calculation`, `report_files`, `shutdown` (`src/runtime/adapters.rs`);
   - dashboard: `dashboard_routes(version) -> Vec<RouteDescriptor>`
     (`src/runtime/dashboard.rs`; the id/manifest come from `plugins`);
   - tool host: `mount_tool_host(requester, views, roles) -> Box<dyn Send +
     Sync>` (`lifecycle.rs:272-306`);
   - overlay host: one `OverlayComposition` trait object owning
     `ReloadableCounterService`, `CommandProcessor`, `WebSocketRelaySink`,
     `OverlayPresenter`, `CatalogService`, `OverlayStatic` and
     `OverlayRootComposition` with `current_root`, `reload_root`,
     `apply_root`, `mount(commands_hub)`, `serve_static`, `has_settings`,
     `installed_names`, `values_path`, `command_session(...)` and an owned,
     joined `shutdown` (sites: `lifecycle.rs:203-326,422-482`,
     `config.rs:428,635`, `catalog.rs:82,253`, `catalog_removal.rs:116`,
     `service.rs`, `server/service/{relay,routing,dashboard,
     websocket_transport}.rs`, `overlay_folders.rs`);
   - osu!-specific inputs: `poll_duration` (adapters), vosu's own startup
     flags (`StartupArgs` in `run_cli`), and the CN_PROC probe manifest
     (`cli.rs:210`, derive it from `plugins` entries that carry a matcher).
   Version: thread `semver::Version` into `ToolRegistry::new`
   (`tools/manifest.rs` 19/52, `inspect`), catalog, dev plugins and the
   overlay validation instead of `crate::application_version()`.
3. **Move `src/tools`, `src/runtime`, the rest of `src/server/service`** into
   core once 2 removes their bundled names; their tests that need `BUNDLED`
   (`runtime/tests/lifecycle_owner*`, `runtime/tests/projection.rs`,
   `server/service/tests/command_filters.rs`) rehome to root integration
   tests or `vosu-test-harness`. Then delete the root library API
   (`src/lib.rs` keeps only what `main.rs`, `plugins.rs`, `vosux` need) and
   migrate `vosu::{runtime,tools,application_version}` callers in `tests/`,
   `benches/`, `examples/`, `crates/vosu-test-harness`.
4. Remove the obsolete fallbacks once nothing constructs them:
   `RouteProviders::{state,calculation}` (production always answers through
   `registered_http`; only tests use the fallbacks).

## Invariants to keep

- Shutdown order in `ProductionRuntime::shutdown_with_plugin_destroy`
  (tool host before registry; discovery/foreground before `destroy_all`;
  listeners stop accepting → hubs → join → adapters → destroy → overlay).
- `src/main.rs` dispatch order: `plugin-host` → `vosux` → runtime; Windows
  console: no `#![windows_subsystem]` by design (ADR 0024: manifest
  `consoleAllocationPolicy=detached` in `build_support/windows_manifest.rs`,
  `detach_private_console` only after command classification in `run_cli`).
- No provider/IO/callback under host locks (see `OverlayRootComposition::retire`).

# M7 C0 handoff (branch `m7/c0`)

Base `69c499ed`; `origin/v2/main` (with `de655cd8`, overlays.query) is merged
(signed merge commit).

## Landed (each compiles, tests pass)

1. `e9374131`: the filter value type is in `vosu_surface::filter`.
2. `fe604bb7`: the generic server mechanics are in `vosu_core::server`.
3. `26582373`: step 1. `bus_provider` and `adapter_capture` (the latter
   under `test-harness`) are in `vosu_core::server`. The adapter-backed
   provider tests moved to root `tests/bus_provider.rs`. The capture tests
   run in core, and the envelope golden file was regenerated for its new
   generator path.
4. `4a1a8c54`: step 4. The `RouteProviders::{state,calculation}`
   fallbacks are gone, along with `CalculatePpProvider`, `CalculateResult`
   and `DeclaredCalculateProvider`. An unregistered state or calculation
   path now returns the ordinary 404 (new core test). Core's
   `test-harness` feature now enables `vosu-net/test-harness`.
5. Merge of `origin/v2/main`.

## WIP commit (does NOT compile): steps 2 and 3 in progress

`git mv` moved `src/runtime{,.rs}`, `src/tools`, `src/server/service{,.rs}`
and `src/server/overlay_folders.rs` into `crates/vosu-core/src/`, and
`vosu_core::` paths became `crate::` (ast_edit). `src/server.rs` is deleted.

Done inside core:
- `runtime/bundle.rs` defines `Bundle`, `BundledHosts`, `AdapterRequesters`,
  `Projections`, `AdapterServices`, `HostRoutes`, `OverlayInputs`,
  `OverlayCatalog` and `OverlayRootSetting`. `runtime.rs` re-exports them;
  the `bundle` module is private. The rest of `runtime.rs` was rewritten:
  `daemon_logger_options(bundle, ..)` is public and `current_version` is
  gone.
- `server/service/overlay.rs` defines the `OverlayComposition`,
  `CommandSession`, `CommandReply` and `RelayControl` traits, with
  `RelayControl` impls for `ConnectionControl` and `Arc<dyn FeedControl>`.
- `server/service/endpoints.rs` (renamed from `relay.rs`) defines
  `COMMANDS_NAMESPACE`, `WebSocketEndpoints` (no processor or relays) and
  `ProductionServices { overlay, config, report_input, report_probe,
  updater, websocket }`. The dead `beatmap` field, `BeatmapFiles` and
  `CurrentBeatmapProvider` are deleted.
- `surface.rs` no longer has `adapter_routes`, `AdapterRoutes`,
  `RegisteredProvider` or `surface_game_response`. `surface_bytes`,
  `surface_not_ready` and `JSON` are now `pub`.
- `routing.rs` is partly converted: the struct, `new`, removed-route
  attribution and `installed_names` are done.
- `runtime/{adapters,dashboard,overlay_root,service,catalog,catalog_removal,
  cli,dev_plugins,config}.rs` and `lifecycle.rs` (struct, shutdown,
  compose, run_cli, update hooks) now go through `Bundle`.
  `RuntimeSettingsStorage::new` takes the `OverlayRootSetting`. The cli
  probe hints come from every bundled manifest with a matcher, and the
  version comes from `bundle.version`.

Root-glue sources copied in and NOT yet edited:
- `src/bundle/overlay/relay.rs` (WebSocketRelaySink and worker)
- `src/bundle/overlay/root.rs` (OverlayRootComposition)
- `src/bundle/adapters.rs` (old Adapters; reduce it to projections)
- `src/bundle/adapters/routes.rs` (keep only `adapter_routes`,
  `RegisteredProvider` and `surface_game_response`)

## Remaining, in order

1. Finish `routing.rs`:
   - `counter_open_root` and `counter_open` use
     `services.overlay.current_root()`;
   - pass `valid_folder` into `overlay_folders::openable_folder` (it still
     calls `vosu_overlay_host::validate_component`);
   - `counter_static` calls `services.overlay.serve_static(surface)`;
   - `websocket()` takes no processor or relays and passes
     `services.overlay` to `bridge_socket`.
2. `websocket_transport.rs`: `bridge_socket` calls
   `overlay.command_session(namespace, client_id, overlay_query,
   Arc::new(connection.clone()))`; `None` closes the connection. It then
   matches `session.process(text)` on `CommandReply`, where `ApplyFilters`
   carries already-parsed filters.
3. `dashboard.rs`: delete `installed_overlay_names`. `socket_overlay` takes
   `&dyn OverlayComposition` and calls `query_overlay(raw)` instead of
   `CommandClient` plus `split_counter`.
4. `test_support.rs`: `start_test_production_composition(bundle, options)`
   uses `bundle.hosts.overlay(..)` with `OverlayCatalog::Test`, and keeps
   one `overlay: Arc<dyn OverlayComposition>` instead of processor, relays,
   counters and overlay_root. `TestGame` replaces `bundle.plugins[0]`.
5. Tools: add a `version: semver::Version` argument to
   `ToolRegistry::new` (the 7th argument; lifecycle already passes it),
   then thread it through `inspect`, `ToolPackage::parse(text, &version)`
   and `validate_context`. Replace `crate::application_version()`.
6. `runtime/discovery.rs`: rewrite its 5 doc links from `vosu_core::` to
   `crate::`. Its tests use `env!("CARGO_PKG_VERSION")`, which is now
   0.1.0; use a test version of `"2.0.0-alpha.1"` (fixture manifests say
   `>=2.0.0-alpha.1`). Do the same in the `api_v1_tests`, `tests/lifecycle`
   and `cli_broker` tests and in the tools tests.
7. Core `lib.rs`: add `pub mod runtime; pub mod tools;`. Core
   `server.rs`: add `pub mod overlay_folders; pub mod service;`. Add the
   core dependencies the moved code needs (`semver`, `md-5`, `url`, `toml`,
   `dotenvy`, `notify`, `rust-embed`?) to `crates/vosu-core/Cargo.toml`;
   let `cargo check` name them.
8. Root glue (`src/bundle.rs`, private): `struct Hosts; impl BundledHosts`.
   - `startup_flags` uses `vosu_osu_reader::discovery::StartupArgs`.
   - `poll_duration` is `vosu_osu::capture::poll_duration`.
   - `projections`: the upstream Regular and Precise providers, gosumemory,
     the streamCompanion chart provider, and the `AdapterServices` impl
     (`upstream::files::Provider::client_version` and the chart
     `shutdown`).
   - `adapter_routes` builds `Provider::new(upstream)`.
   - `dashboard_routes` uses `vosu_dashboard::routes::Routes`.
   - `mount_tool_host` uses `ToolHost::mount` plus `mount_design_system`.
   - `overlay` builds one struct holding counters, processor, relays,
     `OverlayRootComposition` (with `bind_overlay_query`) and
     `OverlayStatic`. It does `set_installed_packages`,
     `processor.bind_profiles(..)`, `relays.bind_processor(..)` and
     `overlay_root.apply(current_root)`. Its `shutdown` runs processor,
     then relays, then `retire`, then counters, and is idempotent (take
     the parts behind a Mutex). The command session parses `ApplyFilters`
     with `vosu_adapter_support::filtered::parse_filters`.
   - Move `RuntimeCounterFacade` and `InstalledOverlayValidation` (old
     `runtime/service.rs`; see git history) here.
9. `src/plugins.rs`: add `pub static BUNDLE: Bundle` (version =
   `CARGO_PKG_VERSION`, `plugin_host_identity`, plugins `BUNDLED`, the 7
   required ids, dashboard and tool host ids,
   `overlay_root: { "vosu.overlayhost", vosu_overlay_host::root::STATIC_FOLDER_PATH }`,
   `hosts: &bundle::Hosts`). `src/lib.rs` becomes `mod bundle; pub mod
   plugins; pub use plugins::BUNDLE;` and nothing else. Make
   `vosux`/`plugin_host` modules of `src/main.rs`, which calls
   `vosu_core::runtime::run_cli(&vosu::BUNDLE)` and
   `vosu_core::runtime::is_startup_owner_conflict`. Replace `DaemonAssets`
   in `tests/process_memory.rs` and `examples/live_reader_bench` with
   `include_bytes!` of the asset (then drop `src/assets.rs`).
10. Tests moving out of core because they need real bundled crates (root
    lib unit tests under `src/bundle/tests/` or root integration tests;
    add an `Assertions-Removed:` trailer wherever text changed):
    - `runtime/tests/projection.rs`, `lifecycle_owner{,/dashboard_off}.rs`
      and the static-root reload test in `runtime/tests/lifecycle.rs`;
    - `server/service/tests/command_filters.rs` and the four relay tests
      in `providers_and_transport.rs`;
    - the dashboard instance test that was deleted from
      `runtime/dashboard.rs` (re-add it as a root test:
      `BUNDLE.instance(BUNDLE.dashboard)` is the dashboard manifest's
      position);
    - `runtime/tests/version.rs`: in core, use a fake bundle version; in
      root, assert `BUNDLE.version == CARGO_PKG_VERSION`.

    The remaining core service tests use a small `OverlayComposition` test
    double.
11. Update root callers to `vosu_core::{runtime,tools,server::service,
    server::overlay_folders}` and `vosu::BUNDLE`:
    - `tests/` (`server_compat/{mounted,declared_roots}`,
      `upstream_wire_extensions` and `catalog_runtime` use
      `BUNDLE.hosts.adapter_routes` / `CatalogComposition::start(.., &BUNDLE)`);
    - `benches/`, `examples/` and `crates/vosu-test-harness`;
    - `vosu::application_version()` becomes `vosu::BUNDLE.version()`;
    - `vosu::plugin_host::BUILD_IDENTITY` becomes
      `vosu::BUNDLE.plugin_host_identity`;
    - `test_updater_handoff` gained a `version` argument.
12. Boundary files and docs:
    - `scripts/module-boundaries/{vosu,vosu-core}.txt` (core gains
      `runtime` and `tools` edges);
    - `scripts/file-length-baseline.txt` paths;
    - `AGENTS.md` source ownership (`src/runtime`, `src/tools` → core);
    - `docs/architecture/{source-map,overview}.md`: bus_provider,
      adapter_capture and runtime paths, and overview.md:264 (the
      calculation endpoint is now the registered route);
    - `docs/decisions/0002`, `docs/testing/benchmarks.md:684`,
      `docs/v2/implementation-plan.md:1013`;
    - record the accepted `vosu-adapter-support -> vosu-surface` edge
      exception in source-map.md (crate-boundaries.txt already has it).
13. Verify:
    - `cargo nextest run --locked -p vosu-core --all-features`;
    - `cargo test --locked --features test-harness --test daemon_smoke
      --test plugin_host --test state_bus --test api_v1_compat`;
    - `server_compat`, `config_compat`, `catalog_v2_compat`;
    - the dashboard and tool_host smokes (ports 25150 and 25151; check
      that they are free first);
    - fmt, workspace clippy, xwin clippy and every `scripts/check-*.py`.

## Decisions

- The overlays.query process-wide weak registry stays. The plugin
  instance is created through the C ABI, which cannot carry in-process
  objects, so the descriptor cannot hand it the counter service. The
  registry is that rendezvous. The binding belongs in the root glue's
  overlay composition.
- Overlay parts are created right after the catalog and stored in
  `ProductionRuntime.overlay` at once. Every later compose failure leaves
  them to the normal `shutdown`, which joins them after plugin destroy, in
  the old processor → relays → retire → counters order.

## Invariants to keep

- `ProductionRuntime::shutdown_with_plugin_destroy` order: tool host before
  the registry; discovery and foreground before `destroy_all`; listeners
  stop accepting, then hubs, then join, then adapters, then destroy, then
  the overlay.
- `src/main.rs` dispatch order: `plugin-host`, then `vosux`, then the
  runtime. No `#![windows_subsystem]` (ADR 0024). `detach_private_console`
  runs only after command classification in `run_cli`.
- No provider work, IO or callback under host locks.

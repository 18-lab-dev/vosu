# O0 handoff (m7/o0)

## Commits
- `8a9d6240` WIP: crate extraction and A7.1-OR write primitives (previous agent).
- `a1cb112d` Root test targets migrated to `vosu-overlay-host`; crate/module boundaries, brand allowlist and doc links updated.
- `78402c87` Overlay static serving through the core-minted read-only lease; `BUNDLED`/`REQUIRED` entry; `static_folder_path` seeding.

## State at 78402c87 (verified)
- `cargo nextest run --locked -p vosu-overlay-host --all-features`: 81/81 pass.
- `cargo test --locked --test server_compat --test config_compat`: 10 + 34 pass.
- `cargo nextest run --all-features --test daemon_smoke --test plugin_host --test catalog_runtime --test catalog_v2_compat`: 56/56 pass (COUNTER_PATH end to end).
- `cargo nextest run -p vosu --lib --all-features owner_tests`: 9/9 pass.
- fmt and clippy `-D warnings` are clean. At a1cb112d every `scripts/check-*.py` passed. At 78402c87, module-boundaries, crate-boundaries and file-length were re-run and pass.

## Design
- Core: `PluginHost::overlay_root_lease(instance, root)` lives in `crates/vosu-core/src/plugin/host/resources/overlay_root.rs`. It returns a `RootLease` plus an `OverlayRootRetirement`. A top-level directory link is opened once as a confined mount; nested links are never followed.
- Root: `src/runtime/overlay_root.rs` holds two types:
  - `RuntimeSettingsStorage` serves B2 for `vosu.osu` and the seed for `vosu.overlayhost`, which rejects `store`.
  - `OverlayRootComposition::apply(root)` seeds the setting, applies it to the counter service, delivers `SETTINGS_CHANGED`, mints the next lease and retires the previous one.
- Host: `OverlayStatic` (`crates/vosu-overlay-host/src/serve*.rs`). Core keeps its `CounterStatic` fallback classification and hands that request to `OverlayStatic::serve` (`ProductionRouteDelegate::with_overlay_static`). A catch-all cannot be a registry route, because every registry form collides with the exact routes.

## Remaining (in order)
1. Counter API routes as `vosu-surface` registrations:
   - Move the `counter_*` handlers from `src/server/service/routing.rs` into a host `RouteProvider` set registered through `host.surface_requester(OVERLAY_HOST_INDEX).register_routes`.
   - Drop the `/api/counters` prefix and the `/websocket/commands` reservation from `reserve_core_routes`, but keep core reservations for `open-root`/`open/*`.
   - Extend `surface_request` to carry the body and the `ManagementProvenance` (`PluginHost::surface_management_provenance` with `Origins::csrf`).
   - Listener roles follow the A7 table.
   - `/websocket/commands`: the host registers the exact feed. Connection framing stays in core's hub (inject a connection factory), and processing stays in the host's `CommandProcessor` through core's `bridge_socket`.
2. Step 3 counterexample tests still missing:
   - core lease: symlink/reparse escape, plus a write (`vosu_net::publish::publish_file`) racing a read of an already opened file. Use the `reader(&host, "vosu.overlayhost", ...)` fixture in `crates/vosu-core/src/plugin/host/tests/state/surface/capture.rs`.
   - a root change during an in-flight download, which must publish into the starting root;
   - the same folder in different profiles;
   - a metadata-only overlay;
   - dashboard preview not counted as a browser (`OverlayConnections`);
   - no private-source credentials on art or package requests.

   Double encoding and the reserved `tools` prefix are already covered in `serve/tests.rs`.
3. Docs: the source map entry for `crates/vosu-overlay-host` and the lease, and the AGENTS.md `src/counters/` ownership line (now the crate). Re-run every `scripts/check-*.py`.
4. Run the dashboard smoke (`cargo build --locked --features test-harness` + `tests/dashboard_smoke.py`). It has not been run yet.

## Integrator-owned edits made
- Workspace `members`, `Cargo.lock`, the test-harness dev-dependency, `scripts/crate-boundaries.txt` and the `scripts/check-module-boundaries.py` `CRATE_ROOTS` entry.
- `src/plugins.rs` `BUNDLED` (index 4) and `dev_plugins::REQUIRED`, plus the composition wiring in `src/runtime/{lifecycle,test_support}.rs`.
- `vosu-core` changes: the route registry allow-list and the overlay root lease.
- `src/tools/{http,mod,tests}.rs`: mechanical renames only.

## Notes for the integrator
- `crates/vosu-overlay-host/src/serve/http.rs` copies core's range planning and content-type table, because the host cannot depend on the root crate. A candidate for a shared `vosu-surface` helper.
- Redirects re-encode the admitted query from decoded pairs (`SurfaceRequest` carries no raw query). This is equivalent for form-style queries.
- An HTML page over 64 MiB that needs `COUNTER_PATH` is refused with a 500 instead of being streamed without the identity script.

## After D0/T0 merge, delete
- D0 removes `RuntimePages::render`: then drop `OverlayPresenter::{installed_list, available_list}` and `cards::{INSTALLED_EMPTY_STATE, render_overlays_unreadable}`.
- T0 removes `/tools/_host/v1/overlays`: then drop `ReloadableCounterService::local_overlays` and `LocalOverlay` (model).

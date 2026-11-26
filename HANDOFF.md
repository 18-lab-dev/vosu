# M7 L2 handoff (m7/l2)

Done (signed): c55ee003 T1 merge; 24725233 merge of f97c3861; 9efb35b1 (d);
fbae450a (b); b00b06ba (c) + reservation narrowing. (e) needed no change: the
`/tools/_host/v1/overlays` doc comment no longer exists after O0's move.

## Remaining

### (a) compose the design-system set
1. `src/runtime/lifecycle.rs` ~line 280: after `ToolHost::mount(..)`, call
   `tool_host.mount_design_system(if self.dashboard { &[Overlay, Dashboard] } else { &[Overlay] })`
   and fail composition on error (same cleanup as the mount error).
2. `src/server/service/routing.rs::page()`: for `Endpoint::Asset`/`Favicon`
   dispatch to the surface on the request's `SurfaceAdmission.listener`
   (generalize `SurfaceRouter::dashboard` in `surface.rs` to take the
   listener role, empty selection), `not_found()` when unregistered; stamp
   `x-vosu-identity: vosu` on a 200 `/favicon.ico` (second-launch probe,
   `vosu_core::desktop::existing_dashboard` requires 200 + header).
3. Delete `ServerAssets` (`src/assets.rs`), `PageService::{asset, favicon,
   lookup_get}`, `embedded_named`, `safe_asset_name`, `PageLookup` if dead;
   `/assets/tool-bridge.js` and `/assets/design.html` then 404.
4. Tests to migrate: `src/server/pages.rs` asset test,
   `tests/server_compat.rs:~630` (PageService assets),
   `src/server/service/tests/listener_and_files.rs` (identity probe needs a
   surface with a mounted tool host, or move the probe check to daemon_smoke).
   Add daemon_smoke asserts: tool-bridge.js/design.html 404 on both
   listeners, favicon carries the identity header.
5. Source-map "Web tool host" row: drop "not yet composed" sentence.
6. Optional (A7.3 spirit): `OverlayRoutes::register` skips dashboard-role
   rows when the dashboard is off.

### (f) runbook ~line 149, (g) catalog-v2 ~line 1686, (h) dashboard smoke
tool assertions (backend `/tools/backend/api/v1/read` "Working" card) and a
full smoke pass, (i) `dashboard-tools.js` Start/Stop/autostart/logs controls.

### Acceptance not yet run on this HEAD
daemon_smoke/plugin_host/api_v1_compat (test-harness), config_compat,
catalog_v2_compat, runtime_wakeups, performance_invariants, prop_, core
nextest, both smokes, xwin clippy, check-*.py (asset hashes, deleted
assertions --base v2/main: no local v2/main ref; fetch origin first).
Run so far: workspace clippy -D warnings clean, fmt clean, lib nextest for
vosu + the three hosts 427/427, server_compat 37/37, check-brand,
crate/module boundaries, file-length, doc-links pass.

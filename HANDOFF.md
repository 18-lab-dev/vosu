# O0 handoff (m7/o0)

## Commits
- `8a9d6240`, `a1cb112d`, `78402c87`: crate extraction, root test migration, static serving through the core-minted read-only lease, `static_folder_path` seeding.
- `25d1a512`: signed merge of `m7/t0` (`82663ea8`). `vosu.overlayhost` is `BUNDLED` index 4, `vosu.toolhost` index 5, the test-harness echo fixture index 6. The `/tools/_host/v1/overlays` listing (`LocalOverlay`, `local_overlays`, the scan's metadata/index flags) is deleted. The host uses `vosu_surface::files::content_type`.
- `9720d7f0`: the counter API and `/websocket/commands` are `vosu-surface` registrations (`OverlayRoutes`). Core keeps classification and admission and passes the listener role, body and a `ManagementProvenance` witness (`SurfaceRouter::overlay_host`). Core reserves only `POST /api/counters/open-root` and `/api/counters/open/*` (dashboard role). Search is dashboard-only (A7).
- `9dd9510f`: step-2 counterexamples; a retired overlay root generation now ends reads of already open files.
- Step 3 docs commit: source map overlay-host row, AGENTS.md ownership line.

## Remaining
- Range planning is still duplicated between `src/server/files.rs` and `crates/vosu-overlay-host/src/serve/http.rs` (optional move to `vosu-surface`).
- After D0 merges: drop `OverlayPresenter::{installed_list, available_list}` and `cards::{INSTALLED_EMPTY_STATE, render_overlays_unreadable}` with `RuntimePages::render`.
- Native Windows evidence for the lease (reparse points) has not been gathered; the link-escape test is Unix-only.

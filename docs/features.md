---
type: Feature Map
title: vosu feature map
description: User-facing map of vosu's surfaces with the names the dashboard uses, the code that owns each one, and the check that exercises it.
resource: README.md
tags: [features, dashboard, verification, vocabulary]
---

# Feature map

Use these names when you describe a change or a bug. They are the labels a user sees. The [README](../README.md) holds the full behavior contract, and the [source map](architecture/source-map.md) lists code ownership.

## Dashboard

The dashboard runs on its own listener (`DASHBOARD_PORT`, [ADR 0016](decisions/0016-separate-dashboard-origin.md)). The header has a **Diagnostics report** button and an update banner (**Update vosu** / **Later**). The status strip shows save state and connection state.

| Page | Path | What the user does there | Owner | Exercised by |
|---|---|---|---|---|
| **Overlays** | `/` | Installed overlay cards: preview, URL bar (click copies the OBS URL, Shift-click opens it), pin toggle, **⋯** menu (**Enlarge preview**, **Move pin earlier/later**, **Open in browser**, **Copy resolution**, **Open folder**, **Nickname and URL…**, **Delete**), per-overlay **Settings for …** dialog, search | `src/runtime/render.rs`, `assets/server/homepage*.{js,css}`, `src/counters/` | `tests/dashboard_smoke.py`, `server_compat` |
| **Available** | `/available` | Community catalog (upstream tosu.app) and catalog-source overlays: install, update, required-tool offers, third-party disclosure | `src/server/catalog.rs`, `assets/server/dashboard-catalog.js`, `src/counters/` | `tests/dashboard_smoke.py`, `tests/catalog_art_smoke.py` |
| **Tools** | `/tools` | **Plugins** (native code vosu loads: bundled, `--plugin` and catalog-installed) with source trust, runtime, approval review and revoke, process **Restart**/**Stop**, failure reasons, per-plugin **Settings** (passwords write-only) and the *revocation pending restart* state that only a restart of vosu clears; **Web tools** (pages vosu serves): **Open** (mints a launch session), enable/disable, trust, settings, **Update available**; **Available** catalog tools and plugins installed through a reviewed plan (D8 disclosure, approvals, one confirmation); **Add support for …** game offers; uninstall and **Keep** | `src/tools/`, `src/runtime/plugin_management.rs`, `src/server/catalog_api/`, `assets/server/dashboard-tools.js`, `assets/server/dashboard-plugins.js`, `assets/server/dashboard-catalog-browse.js`, `assets/server/dashboard-catalog-flow.js` | `tests/tool_catalog_smoke.py`, `tests/tool_host_smoke.py`, `tests/catalog_art_smoke.py`, `server_compat` |
| **Settings** | `/settings` | **General** (Overlays directory, Open dashboard at startup, Start at login on Linux), **Server** (Listener/Dashboard address and port, Allowed IP addresses; changes are applied and then confirmed), **Game data** (poll intervals, pp calculation, key overlay, mania scroll speed and per-key hit errors), **Advanced** (debug logging, multiplayer commands), **Catalog sources**, **API keys**, overlay settings **profiles** | `src/runtime/render.rs`, `assets/server/homepage-settings.js`, `crates/vosu-core/src/config/` | `config_compat`, `server_compat`, `tests/dashboard_smoke.py` |

## Outside the dashboard

| Feature | Surface | Owner | Exercised by |
|---|---|---|---|
| Tray | **Open dashboard**, **Quit** | `crates/vosu-core/src/desktop/tray.rs` | manual (needs a desktop session) |
| Second launch | Opens the running instance's dashboard and exits | `crates/vosu-core/src/app.rs`, `crates/vosu-core/src/desktop/` | `daemon_smoke` |
| Setup | `vosu --setup` (Linux process-event capabilities) | `src/platform/linux/` | `process_memory` (native) |
| Plugin action CLI | `vosux` / `vosu x` `<plugin-id> <action> [json] [--detach] [--session <id>]`; the 1.x tool form and its options were removed in M7 | `src/vosux.rs` | `api_v1_compat` (`cli::vosux_production_action_routes`), `vosux` unit tests |
| Game state APIs | `/json`, `/json/sc`, `/json/v2`, `/json/v2/precise`, WebSockets, selected-field v2 subscriptions | `src/api/`, `src/server/`, `src/state/` | `state_api_compat`, `server_compat`, `performance_invariants` |
| Current game files | `/files/beatmap/…`, `/files/skin/…` | `src/server/files.rs` | `server_compat` |
| Overlay settings profiles | `/api/counters/profiles`, brokered commands | `src/counters/`, `src/server/` | `server_compat`, `config_compat` |
| Native overlay brokers and API keys | broker token, scoped keys | `src/server/auth.rs` | `server_compat` |
| Lazer offsets | automatic cache → embedded asset → local generator | `crates/vosu-osu-reader/src/offset_generator.rs`, `crates/vosu-osu-reader/src/memory/lazer_loader.rs` | unit tests; nightly lazer offset canary |
| Self-update | **Update vosu**, rollback, crash recovery | `crates/vosu-core/src/updater/` | `updater_compat`, `updater_native`, `vosu-release-smoke`, updater property tests |
| Diagnostics report | **Diagnostics report** | `crates/vosu-core/src/report/` | unit tests (escaping) |

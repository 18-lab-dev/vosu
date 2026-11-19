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
| **Tools** | `/tools` | Installed and **Available** tools: install/update/uninstall, **Update to…** (renames), **Open** (mints a write session), Start/Stop/Restart, **Start with vosu**, trust approval | `src/tools/`, `assets/server/dashboard-tools.js` | `tests/tool_catalog_smoke.py`, `tests/tool_host_smoke.py`, `tests/catalog_art_smoke.py` |
| **Settings** | `/settings` | **General** (Overlays directory, Open dashboard at startup, Start at login on Linux), **Server** (Listener/Dashboard address and port, Allowed IP addresses; changes are applied and then confirmed), **Game data** (poll intervals, pp calculation, key overlay, mania scroll speed and per-key hit errors), **Advanced** (debug logging, multiplayer commands), **Catalog sources**, **API keys**, overlay settings **profiles** | `src/runtime/render.rs`, `assets/server/homepage-settings.js`, `src/config/` | `config_compat`, `server_compat`, `tests/dashboard_smoke.py` |

## Outside the dashboard

| Feature | Surface | Owner | Exercised by |
|---|---|---|---|
| Tray | **Open dashboard**, **Quit** | `src/desktop/tray.rs` | manual (needs a desktop session) |
| Second launch | Opens the running instance's dashboard and exits | `src/app.rs`, `src/desktop/` | `daemon_smoke` |
| Setup | `vosu --setup` (Linux process-event capabilities) | `src/platform/linux/` | `process_memory` (native) |
| Tool CLI | `vosux` / `vosu x` (`--list`, `--open`, `--install`, `--update`, `--uninstall`, `--autostart`) | `src/vosux.rs`, `src/tools/` | `tests/tool_host_smoke.py` |
| Game state APIs | `/json`, `/json/sc`, `/json/v2`, `/json/v2/precise`, WebSockets, selected-field v2 subscriptions | `src/api/`, `src/server/`, `src/state/` | `state_api_compat`, `server_compat`, `performance_invariants` |
| Current game files | `/files/beatmap/…`, `/files/skin/…` | `src/server/files.rs` | `server_compat` |
| Overlay settings profiles | `/api/counters/profiles`, brokered commands | `src/counters/`, `src/server/` | `server_compat`, `config_compat` |
| Native overlay brokers and API keys | broker token, scoped keys | `src/server/auth.rs` | `server_compat` |
| Lazer offsets | automatic cache → embedded asset → local generator | `src/runtime/offset_generator.rs`, `src/memory/lazer_loader.rs` | unit tests; nightly lazer offset canary |
| Self-update | **Update vosu**, rollback, crash recovery | `src/updater/` | `updater_compat`, `updater_native`, `vosu-release-smoke`, updater property tests |
| Diagnostics report | **Diagnostics report** | `src/report/` | unit tests (escaping) |

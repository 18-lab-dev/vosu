<div align="center">

# vosu

**Native osu! telemetry, counters, and compatible APIs in Rust.**

<p>
  <a href="rust-toolchain.toml"><img alt="Rust 1.96.1" src="https://img.shields.io/badge/Rust-1.96.1-000000?style=flat-square"></a>
  <a href="#platform-support"><img alt="Linux: first-class" src="https://img.shields.io/badge/Linux-first--class-FCC624?style=flat-square"></a>
  <a href="#platform-support"><img alt="Windows: supported" src="https://img.shields.io/badge/Windows-supported-0078D4?style=flat-square"></a>
  <a href="LICENSE"><img alt="License: LGPL-3.0-only" src="https://img.shields.io/badge/License-LGPL--3.0--only-6e5494?style=flat-square"></a>
</p>

</div>

> A vibe-coded, ground-up Rust rewrite of upstream [tosu](https://github.com/tosuapp/tosu), with first-class Linux support, measured hot paths, and a repository designed to work well with coding agents.

The daemon was written from scratch in Rust. The upstream project defines the compatibility target and supplies pinned dashboard assets and calculator provenance; it is not a runtime dependency or a translated codebase.

`vosu` reads live state from osu!stable and osu!lazer, then serves the dashboard, counters, reports, and compatible HTTP/WebSocket APIs. It is one native daemon: no Node.js, Electron, .NET, or C# runtime is required.

This build does not ship or launch the former Electron/asdf-overlay in-game overlay. The compatible browser page, settings data, and command WebSocket remain available.

## Why this build

| | |
| --- | --- |
| **Native runtime** | A single Rust daemon with embedded dashboard assets and offsets. |
| **Verified compatibility** | gosuMemory v1, streamCompanion, and vosu v2 HTTP/WebSocket behavior are checked against the pinned upstream tosu reference with live parity captures. |
| **Linux support** | Native osu!lazer and osu!stable through Wine are supported alongside Windows stable and lazer. |
| **Measured performance** | Criterion covers memory scanning, snapshot reads, calculations, serialization, caching, and publication. Results and limits are recorded in [benchmarks](docs/benchmarks.md). |
| **Agent-ready repository** | A pinned toolchain, explicit source ownership, runnable checks, architecture notes, and decision records keep automated changes reviewable. Start with [AGENTS.md](AGENTS.md). |

## Install

Download `vosu-linux-v0.1.0.zip` or `vosu-windows-v0.1.0.zip` from the matching [GitHub release](https://github.com/vosuapp/vosu/releases), then:

1. Extract the complete archive into a writable directory. Its root contains `vosu` or `vosu.exe`, plus `LICENSE`.
2. On Linux, restore the executable bit if your archive tool removed it: `chmod +x vosu`.
3. Run `./vosu` on Linux or `vosu.exe` on Windows.
4. Open <http://127.0.0.1:24050/>. By default, vosu opens the dashboard after startup.

Do not run the Windows executable directly from an archive or a OneDrive-controlled directory. The updater cannot provide safe replacement and rollback in those locations.

### Automatic updates

`ENABLE_AUTOUPDATE=true` follows stable `vosuapp/vosu` releases. The updater verifies the selected platform archive against its published SHA-256 digest, stages exactly one root executable, and uses a crash-recoverable helper and rollback flow.

`GET /api/runUpdates` starts the same update path. Set `ENABLE_AUTOUPDATE=false` or launch with `--update=false` to disable the startup check. `--update=true` enables it for one launch.

## Build from source

### Requirements

- Rust **1.96.1**, also pinned in `rust-toolchain.toml`
- a native toolchain for `x86_64-unknown-linux-gnu` or `x86_64-pc-windows-msvc`
- Linux: a GNU C toolchain and the Vulkan/OpenGL loader development files needed by `wgpu`
- Windows: MSVC build tools and the Windows SDK

The dashboard and offsets are vendored build inputs. No frontend or managed-runtime toolchain is needed.

```sh
cargo build --locked --no-default-features
cargo run --locked --no-default-features
```

Production builds:

```sh
# Linux
cargo build --release --locked --no-default-features --target x86_64-unknown-linux-gnu

# Windows, from Developer PowerShell
cargo build --release --locked --no-default-features --target x86_64-pc-windows-msvc
```

The `test-harness` feature is development-only. `build.rs` rejects it in release profiles; it is not a production switch.

## Platform support

Run vosu as the same desktop user as osu!. Tournament and debug-client processes are filtered by command line.

| Platform | osu!stable | osu!lazer |
| --- | --- | --- |
| **Linux** | Wine process discovery and native memory reads. Set `VOSU_OSU_PATH=/absolute/path/to/osu!` if Wine does not expose the install directory. | The official executable or AppImage layout must expose `osu!.deps.json`. Sandbox policy must allow process discovery and memory reads. |
| **Windows** | Supported 32-bit processes are read through native Windows process-memory APIs. | A supported 64-bit process is required. Offsets are resolved from embedded data, the versioned cache, then the first-party `https://vosu.app/offsets` service and configured fallback services. |

On Linux, vosu can also read the osu! path from `${XDG_DATA_HOME:-$HOME/.local/share}/osuconfig/osupath`.

`process_vm_readv` must be permitted. If ptrace policy blocks same-user reads, use the distribution-appropriate ptrace configuration or grant only the required capability to the binary:

```sh
sudo setcap cap_sys_ptrace=eip ./vosu
```

Do not run the dashboard service as root to bypass ptrace policy. Pattern or offset resolution may remain pending and retry while the process is alive; that alone does not confirm support for a game build.

## Files and migration

### Linux

The daemon follows XDG directories and appends `vosu`:

| Data | Path |
| --- | --- |
| Configuration | `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/vosu.env` |
| Counters | `${XDG_DATA_HOME:-$HOME/.local/share}/vosu/static/` |
| Counter values | `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/settings/<counter>.values.json` |
| Cache | `${XDG_CACHE_HOME:-$HOME/.cache}/vosu/` |

### Windows

The directory containing `vosu.exe` is the program, configuration, and data root:

| Data | Path |
| --- | --- |
| Configuration | `<program>/vosu.env` |
| Counters | `<program>/static/` |
| Counter values | `<program>/settings/<counter>.values.json` |
| Cache | `<program>/.cache/` |
| Logs | Runtime log directory below the program location |

An absolute `STATIC_FOLDER_PATH` is used as written. A custom relative path is resolved from the launch directory. The defaults `./static` and `.\static` map to the platform data root.

On first start after the rename, vosu performs a one-time, idempotent state migration. On Linux it moves files from the former `tosu` XDG config, data, and cache roots into the corresponding `vosu` roots without overwriting state already present there, then removes empty former roots. On every platform it merges the former `tosu.env` into `vosu.env`: values already in `vosu.env` win, deprecated bindings are dropped, and the former file is removed only after the replacement succeeds. After migration, vosu reads and writes only the new locations. Counter-local `settings.values.json` files still move to the central settings directory and remain in the counter directory as `settings.old-values.json`.

## Configuration

Edit `vosu.env`. The daemon watches the file and applies supported changes without restarting. Writes use a temporary file in the same directory followed by atomic replacement.

| Binding | Default | Meaning |
| --- | ---: | --- |
| `ENABLE_AUTOUPDATE` | `true` | Check for and install stable vosu releases. |
| `OPEN_DASHBOARD_ON_STARTUP` | `true` | Open the effective dashboard listener after startup. |
| `DEBUG_LOG` | `false` | Include debug-level file and console logging. |
| `CALCULATE_PP` | `true` | Enable live performance calculations. |
| `ENABLE_KEY_OVERLAY` | `true` | Publish key-overlay data in compatible schemas. |
| `POLL_RATE` | `150` | Regular state interval in milliseconds; minimum `100`. |
| `PRECISE_DATA_POLL_RATE` | `10` | Precise state interval in milliseconds; minimum `1`. |
| `SHOW_MP_COMMANDS` | `false` | Include multiplayer commands where supported. |
| `READ_MANIA_SCROLL_SPEED` | `true` | Read mania scroll speed. |
| `SERVER_IP` | `127.0.0.1` | Listener hostname or address. |
| `SERVER_PORT` | `24050` | Listener port. |
| `STATIC_FOLDER_PATH` | `./static` | Counter root; the default maps to the data directory. |
| `ENABLE_INGAME_OVERLAY` | `false` | Retained for data compatibility; see below. |
| `INGAME_OVERLAY_KEYBIND` | `Control + Shift + Space` | Retained overlay setting. |
| `INGAME_OVERLAY_MAX_FPS` | `60` | Retained overlay setting. |
| `ALLOWED_IPS` | `127.0.0.1,localhost,absolute` | Peer IP patterns and allowed Origin/Referer host tokens. |

Authorization requires an allowed network peer and, when supplied, an allowed `Origin` or `Referer`. A hostname token does not allow an arbitrary remote peer. Keep the default loopback listener unless remote access is intentionally configured and protected.

### Deferred in-game overlay

`ENABLE_INGAME_OVERLAY` is parsed, stored, and written back for migration compatibility. If set to `true`, the daemon logs:

```text
In-game overlay is unavailable in the Rust daemon-only build; ENABLE_INGAME_OVERLAY is ignored.
```

The `/api/ingame` browser page, command WebSocket, and `__ingame__.values.json` remain available to compatible browser clients. This build never downloads or launches a native or Electron in-game overlay.

## Dashboard and counters

The dashboard at `/` discovers counters recursively below the static root. A counter normally contains `index.html`. `metadata.txt` can add a name, author links, description, version, and `WxH` size. An immediate `ignore.txt` excludes its directory from discovery. Mutation APIs accept counter names containing one normal path component.

Counter `settings.json` files support text, color, number, checkbox, button, options, commands, textarea, password, and header entries. Values are stored at `<config>/settings/<counter>.values.json`. The `/settings` page handles daemon and counter settings. `__ingame__.values.json` remains available for browser-client compatibility.

The counter catalog is served from `https://vosu.app/api.json`. Production counter downloads require HTTPS. Redirects are bounded; HTTPS-to-HTTP downgrades and loopback, private, or reserved destinations are rejected. ZIP paths, symlinks, entry counts, and expanded size are checked before same-filesystem replacement.

## HTTP API

Methods and paths are exact. Unsupported combinations return `404 Not Found`. State endpoints return `500 {"error":"osu is not ready/running"}` until an instance is focused.

### State and calculation

| Route | Response |
| --- | --- |
| `GET /json` | gosuMemory-compatible v1 snapshot |
| `GET /json/sc` | streamCompanion-compatible snapshot |
| `GET /json/v2` | vosu v2 snapshot |
| `GET /json/v2/precise` | vosu precise snapshot |
| `GET /api/calculate/pp` | Legacy `rosu-pp` calculation; uses the current beatmap unless `path` names a canonical regular `.osu` file |

All calculation parameters are optional: `path`, `lazer`, `mode` (`0` osu, `1` taiko, `2` catch, `3` mania), `ar`, `cs`, `hp`, `od`, `clockRate`, `passedObjects`, `combo`, `nMisses`, `n100`, `n300`, `n50`, `nGeki`, `nKatu`, `mods` (legacy bits or a JSON mod object/array), `acc`, `sliderEndHits`, `smallTickHits`, `largeTickHits`, and `hitresultPriority`.

### Current game files

- `GET /backgroundImage`
- `GET /files/beatmap/background`
- `GET /files/beatmap/audio`
- `GET /files/beatmap/file`
- `GET /Songs/{path}`
- `GET /files/beatmap/{path}`
- `GET /files/skin/{path}`; unavailable for lazer skins

File routes enforce root containment and support validated single HTTP byte ranges for media.

### Pages, counters, reports, and updates

- `GET /`, `/settings`, `/local-overlays`, `/available`, `/favicon.ico`
- `GET /assets/{path}` and the fallback counter-static route
- `GET /api/ingame...`
- `POST /api/settingsSave`
- `GET /api/runUpdates`
- `GET /api/generateReport`
- `GET /api/counters/search/{query}`
- `GET /api/counters/download/{url}?name=<counter>&update=<value>`
- `GET /api/counters/open/{name}`
- `GET /api/counters/delete/{name}`
- `GET|POST /api/counters/settings/{name}`

POST bodies are capped at 1 MiB. Generated reports escape machine, configuration, counter, and log data before inserting it into HTML.

## WebSocket API

| Upgrade path | Stream |
| --- | --- |
| `/ws` | v1 regular state |
| `/tokens` | streamCompanion regular state |
| `/websocket/v2` | vosu v2 regular state |
| `/websocket/v2/precise` | vosu precise state |
| `/websocket/commands` | Counter, settings, filter commands, and the retained `__ingame__` browser flow |

Unknown near-miss paths do not upgrade. Frames and assembled messages are limited to 1 MiB. Slow clients receive the latest state instead of building an unbounded backlog. The [upstream tosu wiki](https://github.com/tosuapp/tosu/wiki) documents the compatibility schemas and filter examples.

## Maintainer documentation

| Document | Scope |
| --- | --- |
| [Documentation map](docs/README.md) | Where project information belongs |
| [Architecture](docs/architecture.md) | Runtime boundaries, flows, deployment, and risks |
| [Decision records](docs/decisions/README.md) | Durable design choices and their consequences |
| [Benchmarks](docs/benchmarks.md) | Criterion coverage, method, results, and limitations |
| [Coding-agent instructions](AGENTS.md) | Source ownership, invariants, commands, and verification |

## License

Licensed under [GNU LGPL v3.0 only](LICENSE). Bundled asset and dependency notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Additional calculator notices are retained in [`crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt`](crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt).

<div align="center">

<img src="assets/branding/vosu-banner.svg" alt="Vosu — Native osu! telemetry, counters, and compatible APIs in Rust." width="100%">

<p>
  <a href="rust-toolchain.toml"><img alt="Rust 1.96.1" src="https://img.shields.io/badge/Rust-1.96.1-000000?style=flat-square"></a>
  <a href="#platform-support"><img alt="Linux: first-class" src="https://img.shields.io/badge/Linux-first--class-FCC624?style=flat-square"></a>
  <a href="#platform-support"><img alt="Windows: supported" src="https://img.shields.io/badge/Windows-supported-0078D4?style=flat-square"></a>
  <a href="LICENSE"><img alt="License: LGPL-3.0-only" src="https://img.shields.io/badge/License-LGPL--3.0--only-6e5494?style=flat-square"></a>
</p>

</div>

> A vibe-coded, ground-up Rust rewrite of upstream [tosu](https://github.com/tosuapp/tosu), with first-class Linux support, measured hot paths, and a repository designed to work well with coding agents.

The daemon was written from scratch in Rust, and its embedded dashboard is a first-party vanilla HTML, CSS, and JavaScript interface. The upstream project defines the compatibility target and calculator provenance; it is not a runtime dependency or a translated codebase.

`vosu` reads live state from osu!stable and osu!lazer, then serves the dashboard, counters, reports, and compatible HTTP/WebSocket APIs. It is one native daemon: no Node.js, Electron, .NET, or C# runtime is required.

This build does not provide the former Electron/asdf-overlay in-game overlay or an embedded replacement. External overlay software can consume the documented HTTP and WebSocket APIs; [ADR 0008](openwiki/decisions/0008-external-in-game-overlays.md) records this product boundary.

## Why this build

| | |
| --- | --- |
| **Native runtime** | A single Rust daemon with embedded dashboard assets and offsets. |
| **Verified compatibility** | gosuMemory v1, streamCompanion, and vosu v2 HTTP/WebSocket behavior are checked against the pinned upstream tosu reference with live parity captures. |
| **Linux support** | Native osu!lazer and osu!stable through Wine are supported alongside Windows stable and lazer. |
| **Measured performance** | Criterion covers memory scanning, snapshot reads, calculations, serialization, caching, and publication. Results and limits are recorded in the [benchmark guide](openwiki/testing/benchmarks.md). |
| **Agent-ready repository** | A pinned toolchain, explicit source ownership, runnable checks, architecture notes, and decision records keep automated changes reviewable. Start with [AGENTS.md](AGENTS.md). |

## Install

### Portable archive

Download `vosu-linux-v0.1.0.zip` or `vosu-windows-v0.1.0.zip` from the matching [GitHub release](https://github.com/shiruviaa/vosu/releases), then:

1. Extract the complete archive into a writable directory. Its root contains `vosu` or `vosu.exe`, plus `LICENSE`.
2. On Linux, restore the executable bit if your archive tool removed it: `chmod +x vosu`.
3. Run `./vosu` on Linux or `vosu.exe` on Windows.
4. Open <http://127.0.0.1:24050/>. By default, vosu opens the dashboard after startup.

Do not run the Windows executable directly from an archive or a OneDrive-controlled directory. The updater cannot provide safe replacement and rollback in those locations.

### Manual per-user installation

Install into a user-writable location so vosu can update itself without running as root or Administrator. After extracting the release archive, run:

**Linux**

```sh
install -Dm755 vosu "$HOME/.local/bin/vosu"
install -Dm644 LICENSE "$HOME/.local/share/licenses/vosu/LICENSE"
"$HOME/.local/bin/vosu"
```

Add `$HOME/.local/bin` to `PATH` if you want to launch vosu as `vosu`.

**Windows (PowerShell)**

```powershell
$installDir = Join-Path $env:LOCALAPPDATA "Programs\vosu"
New-Item -ItemType Directory -Force $installDir | Out-Null
Copy-Item .\vosu.exe, .\LICENSE $installDir
& "$installDir\vosu.exe"
```

For a later manual upgrade, stop vosu, replace the executable and `LICENSE` with the files from the new complete archive, then restart it.

### Automatic updates

`ENABLE_AUTOUPDATE=true` follows stable `shiruviaa/vosu` releases. The updater verifies the selected platform archive against its published SHA-256 digest, stages exactly one root executable, and uses a crash-recoverable helper and rollback flow.

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

On Linux, `process_vm_readv` must be permitted. During ordinary startup vosu checks the running process's effective capability set and warns if `CAP_SYS_PTRACE` is not effective, or if that check cannot be completed. The warning does not stop startup, but memory reads may fail. Install the capabilities required by the exact vosu build with:

```sh
sudo vosu --setcap
```

`--setcap` must be the only argument. It uses the running executable's absolute path, installs `/etc/systemd/system/vosu-capabilities.service`, starts that one-shot service immediately, and exits without starting the daemon. A normal build receives only `CAP_SYS_PTRACE`; a build compiled with `linux-process-events` also receives `CAP_SYS_RESOURCE`, `CAP_PERFMON`, and `CAP_BPF` for eBPF loading.

The service is deliberately not enabled at boot: automatically granting capabilities to a replaceable user-owned binary would create a privilege-escalation path. Executable replacement, including an automatic update, removes file capabilities. After replacing vosu at the same path, rerun `sudo systemctl start vosu-capabilities.service`; rerun `sudo vosu --setcap` if the executable path changed.

Do not run the ordinary vosu daemon as root to bypass ptrace or eBPF policy. Pattern or offset resolution may remain pending and retry while the process is alive; that alone does not confirm support for a game build.

## Files and migration

### Linux

The daemon follows XDG directories and appends `vosu`:

| Data | Path |
| --- | --- |
| Configuration | `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/vosu.toml` |
| Counters | `${XDG_DATA_HOME:-$HOME/.local/share}/vosu/static/` |
| Counter schema | `${XDG_DATA_HOME:-$HOME/.local/share}/vosu/static/<counter>/settings.schema.toml` |
| Counter values | `${XDG_CONFIG_HOME:-$HOME/.config}/vosu/settings/<counter>.settings.values.toml` |
| Cache | `${XDG_CACHE_HOME:-$HOME/.cache}/vosu/` |

### Windows

The directory containing `vosu.exe` is the program, configuration, and data root:

| Data | Path |
| --- | --- |
| Configuration | `<program>/vosu.toml` |
| Counters | `<program>/static/` |
| Counter schema | `<program>/static/<counter>/settings.schema.toml` |
| Counter values | `<program>/settings/<counter>.settings.values.toml` |
| Cache | `<program>/.cache/` |
| Logs | Runtime log directory below the program location |

An absolute `STATIC_FOLDER_PATH` is used as written. A custom relative path is resolved from the launch directory. The defaults `./static` and `.\static` map to the platform data root.

On first start after the rename, vosu performs a one-time, idempotent state migration. On Linux it moves files from the former `tosu` XDG config, data, and cache roots into the corresponding `vosu` roots without overwriting state already present there, then removes empty former roots. On every platform, configuration migration layers defaults, readable `tosu.env`, readable `vosu.env`, and canonical `vosu.toml` with precedence `vosu.toml > vosu.env > tosu.env > defaults`. The legacy dotenv files are migration inputs only. It writes canonical TOML to `vosu.toml` through an atomic same-directory replacement; if canonical TOML is malformed at startup, all source files remain untouched. After migration, vosu reads and writes only canonical `vosu.toml`; legacy counter settings inputs remain untouched.
For each counter, the package root's `settings.schema.toml` is the authoritative schema and developer schema saves write that file. Steady-state config storage contains only mutable values at `<config>/settings/<counter>.settings.values.toml`. When the root schema is missing, vosu creates it from root `settings.json` when present, otherwise by inferring setting types from scalar root `settings.toml`: strings become `text`, integers/floats become `number`, booleans become `checkbox`, and each key supplies `uniqueID`/`title` with the scalar as its default. Arrays, nested tables, and datetimes are rejected as ambiguous without creating a schema or modifying package inputs. Only after package sources does loading fall back to old config `<counter>.settings.toml` or `<counter>.settings.json`; every legacy counter schema and values input remains byte-for-byte untouched after canonical schema and values are created. Root JSON and TOML inputs remain untouched, and when both exist JSON defines the schema while TOML may seed values.

## Configuration

Edit `vosu.toml` in the platform configuration root. It is a flat, typed TOML document with uppercase binding names. The daemon watches its parent directory and applies supported changes without restarting. Writes use a temporary file in the same directory, flush and sync it, then atomically replace the target.

```toml
ENABLE_AUTOUPDATE = true
OPEN_DASHBOARD_ON_STARTUP = true
POLL_RATE = 150.0
PRECISE_DATA_POLL_RATE = 10.0
SERVER_IP = "127.0.0.1"
SERVER_PORT = 24050.0
STATIC_FOLDER_PATH = "./static"
ALLOWED_IPS = "127.0.0.1,localhost,absolute"
```

At startup, malformed canonical TOML leaves every source file untouched. During watch reload, malformed TOML leaves the last-good runtime configuration and dependent resources active.

| Binding | Default | Meaning |
| --- | ---: | --- |
| `ENABLE_AUTOUPDATE` | `true` | Check for and install stable vosu releases. |
| `OPEN_DASHBOARD_ON_STARTUP` | `true` | Open the effective dashboard listener after startup. |
| `DEBUG_LOG` | `false` | Include debug-level file and console logging. |
| `CALCULATE_PP` | `true` | Enable live performance calculations. |
| `ENABLE_KEY_OVERLAY` | `true` | Publish key-overlay data in compatible schemas. |
| `POLL_RATE` | `150` | Regular state interval in milliseconds; minimum `100`. |
| `PRECISE_DATA_POLL_RATE` | `10` | Precise state interval in milliseconds; minimum `1`. |
| `V2_SUBSCRIPTION_DEBOUNCE` | `0` | Minimum interval in milliseconds between selected-field WebSocket frames; minimum `0`, fractional values supported. |
| `SHOW_MP_COMMANDS` | `false` | Include multiplayer commands where supported. |
| `READ_MANIA_SCROLL_SPEED` | `true` | Read mania scroll speed. |
| `SERVER_IP` | `127.0.0.1` | Listener hostname or address. |
| `SERVER_PORT` | `24050` | Listener port. |
| `STATIC_FOLDER_PATH` | `./static` | Counter root; the default maps to the data directory. |
| `ALLOWED_IPS` | `127.0.0.1,localhost,absolute` | Peer IP patterns and allowed Origin/Referer host tokens. |

Authorization requires an allowed network peer and, when supplied, an allowed `Origin` or `Referer`. A hostname token does not allow an arbitrary remote peer. Keep the default loopback listener unless remote access is intentionally configured and protected.


## Dashboard and counters

The dashboard at `/` discovers counters recursively below the static root. A counter normally contains `index.html`. `metadata.txt` can add a name, author links, description, version, and `WxH` size. An immediate `ignore.txt` excludes its directory from discovery, and directories named `node_modules` are never traversed. Mutation APIs accept counter names containing one normal path component.

A top-level directory symlink directly below the static root is an explicit local mount declaration and may target a directory outside that root; no `index.html` marker is required for authorization. Discovery follows that top-level alias but skips nested directory symlinks, which neither create nor traverse additional mount roots. Every requested file below the alias is still confined to the current alias target and streamed through a retained handle. Counter settings remain available through aliases, while final symlinked settings files are rejected and settings writes use atomic same-directory replacement. **Open Folder** follows a directory alias. **Delete** unlinks the alias itself without traversing or deleting its target.

Installed counter cards can be pinned; pinned cards stay above unpinned cards in their saved order and can be reordered by dragging. Shift-click a counter URL to open it in a new window instead of copying it. Press `/` outside an editable control to focus the dashboard search.

Counter settings support text, color, number, checkbox, button, options, commands, textarea, password, header entries. Each counter package owns its authoritative schema at `<static-root>/<counter>/settings.schema.toml`; only mutable values are stored centrally at `<config>/settings/<counter>.settings.values.toml`. `/settings` page handles daemon counter settings.

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

The v2 snapshot and matching WebSocket streams expose `beatmap.stats.timingWindows`. Values are maximum absolute judgement errors in milliseconds for osu!, taiko, and mania, derived from the playable map after difficulty mods and scaled to real time after speed mods; osu!lazer Difficulty Adjust values above OD 10 are supported. For catch, only `"300"` is present and represents the spatial half-width from the catcher's centre in playfield units; unsupported judgements are omitted.

Hit-error fields use the same object shape across v1, streamCompanion, v2, and v2 precise: `{"all":[],"press":[],"release":[]}`. `all` preserves timing-event order, `press` contains note presses, and `release` contains osu!lazer mania hold-note releases. Stable reports every timing error in `all` and `press` and leaves `release` empty.

`/json/v2/precise` publishes low-latency input data at `PRECISE_DATA_POLL_RATE`. Its additive `keypresses` object contains the active ruleset in `mode` and every source-reported logical input in ordered `keys` entries (`name`, `isPressed`, `count`, `pressedAt`, and `releasedAt`) for stable and lazer; the legacy fixed `keys` object remains available. Edge timestamps are nullable gameplay milliseconds. They update only when the sampled state exposes that edge, so a count jump can report missed complete taps without fabricating timestamps.
Maintainers can run the zero-dependency harness with `cd tools/input-validation && uv run python run.py ...`. `capture` accounts for precise transitions and `scan-maps` selects long-note-heavy mania maps. `autoplay <map.osu>` and `live-autoplay <map.osu>` generate deterministic plans for osu!, taiko, catch, and mania, then inject them through Windows `SendInput` or Linux `/dev/uinput`; live mania plans additionally support bounded seeded timing jitter. Standard-mode and catch pointer injection requires `--playfield x,y,width,height`; Linux also requires the full compositor layout as `--desktop x,y,width,height`, not only the target monitor. Default controls are `Z` for osu!, `F,J,D,K` for taiko plan controls `DON_LEFT,DON_RIGHT,KAT_LEFT,KAT_RIGHT`, pointer-only for catch, and `D,F,J,K` for 4-key mania.

Maintainer workflow, evidence interpretation, troubleshooting, and maintenance triggers are documented in the [precise input validation guide](openwiki/testing/input-validation.md).

`navigate` follows keyboard paths from an unobstructed idle logo: lazer uses `P,P,P` for song select and `P,P,M,M` for multiplayer, while stable uses `P,P,P` and `P,P,M`. Navigation key-down onsets default to 100 ms apart. Optional song-select search text uses 1 ms key-down onset spacing. Use each command's `--help` for backend, timing, geometry, and key-map options.

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
- `POST /api/settingsSave`
- `GET /api/runUpdates`
- `GET /api/generateReport`
- `GET /api/counters/search/{query}`
- `GET /api/counters/download/{url}?name=<counter>&update=<value>`
- `GET /api/counters/open/{name}`
- `GET /api/counters/open-root` opens the configured counter/static root.
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
| `/websocket/v2/subscribe` | Selected fields from the vosu v2 document |
| `/websocket/commands` | Counter settings and filter commands |

Unknown near-miss paths do not upgrade. Frames and assembled messages are limited to 1 MiB. Slow clients receive the latest state instead of building an unbounded backlog. The [upstream tosu wiki](https://github.com/tosuapp/tosu/wiki) documents the compatibility schemas and filter examples.

The selected-field endpoint expects one text message within five seconds: a JSON array of 1–256 non-overlapping RFC 6901 JSON Pointers into `/json/v2`. It sends compact initial `[0, revision, values]` and update `[1, revision, values, removedIds]` arrays; selector IDs are their zero-based request positions. The selection is immutable, so changing fields requires reconnecting. This endpoint has no command channel.

## Maintainer documentation

| Document | Scope |
| --- | --- |
| [Maintainer documentation](openwiki/index.md) | Navigation, information boundaries, and review conventions |
| [Architecture](openwiki/architecture/index.md) | Runtime boundaries, flows, deployment, and risks |
| [Decision records](openwiki/decisions/index.md) | Durable design choices and their consequences |
| [Benchmarks](openwiki/testing/benchmarks.md) | Criterion coverage, method, results, and limitations |
| [Testing and verification](openwiki/testing/verification.md) | Change-specific evidence and release checks |

## License

Licensed under [GNU LGPL v3.0 only](LICENSE). Bundled asset and dependency notices are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Additional calculator notices are retained in [`crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt`](crates/vosu-lazer-pp/THIRD_PARTY_LICENSES.txt).

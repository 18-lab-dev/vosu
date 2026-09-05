# Changelog

All notable user-visible changes to vosu are documented here. Entries below the Unreleased section are retained release history and use the product names and artifact names that were current at the time.

## Unreleased

### Runtime and maintenance

- Remove the report-only graphics stack; use Linux sysfs and Windows WMI hardware metadata, with explicit unavailable/unknown states.
- Bound report log retention with visible truncation notices; isolate counter discovery, report capture, configuration persistence/reload, and ordered console output from async workers.
- Avoid primary API graph/hit-error encoding copies, share live-calculator map preparation and unchanged stable mania replay batches, and bound WebSocket pong backpressure.
- Use component-aware case-insensitive Windows startup-path checks rather than string-prefix matching.
- Separate MSVC SDK, Cargo downloads, and build caches; restrict cross SDK downloads to x86_64; classify changes conservatively and always publish `CI required`, including docs-only changes.
- Move packaged-executable smoke checks into `vosu-release-smoke`, remove duplicate daemon asset embedding, and maintain existing Markdown without scheduled inference. Linux process-event backends and defaults remain unchanged.

### Gameplay

- Reset published hit-error arrays when osu!lazer or osu!stable rebuilds the live hit-event list, so editor test-mode section restarts drop pre-restart errors and skipped-object zeros instead of waiting for those notes to elapse again.

### Licensing

- Project-authored code and documentation are now dual-licensed under `MIT OR Apache-2.0`; canonical texts are provided in `LICENSE-MIT` and `LICENSE-APACHE`, while release archives retain the root `LICENSE` member.
- Retained third-party and provenance-pinned material remains under its own terms; see `THIRD_PARTY_NOTICES.md` and the calculator attribution file.

### Calculation

- Replaced the LGPL-derived native live calculator with a clean-room Rust implementation based on pinned MIT `rosu-pp` 4.0.1 and MIT `ppy/osu` revision `97e94babde9d5ecbe574ecdd33509db27452cd0f`; retained the former calculator tree only as non-production historical parity evidence.

### API

- Restored the tosu-compatible hit-error arrays at `hitErrorArray` and `hitErrors`; ordered press and release classifications now use additive sibling `pressHitErrors` and `releaseHitErrors` fields.
- Replaced `beatmap.stats.timingWindows` with Storycraft's merged tosu `beatmap.stats.hitWindow` contract and its mode-specific `miss`, `meh`, `ok`, `good`, `great`, and `perfect` keys.
- Replaced the separate `keypresses` object with a tosu-compatible `keys` object. Regular and precise v2 responses retain `k1`, `k2`, `m1`, and `m2` and add the ordered mode-agnostic controls under `keys.all`, matching the planned upstream port.

## 0.2.0 — 2026-07-28

### Product identity and clean cutover

- The clean-cutover vosu package and release line starts at **0.1.0**.
- Renamed the current product, Rust package, executable, API terminology, logs, and updater metadata to `vosu`. Workspace crates are now `vosu-pp` and `vosu-test-harness`.
- Canonical server configuration is the flat, typed `vosu.toml` document with uppercase binding names. Former environment-variable interfaces are migration inputs only; current operation does not accept them as configuration aliases.
- Release archives are now `vosu-linux-<tag>.zip` and `vosu-windows-<tag>.zip`, each containing the matching `vosu` or `vosu.exe` root executable. Automatic updates follow `shiruviaa/vosu`.
- First-party catalog, offset, and identity URLs now use <https://vosu.app>.
- Added an idempotent, one-time migration for existing state. Linux moves non-conflicting files from former `tosu` XDG config, data, and cache roots into the corresponding `vosu` roots. Server configuration layers defaults, readable `tosu.env`, readable `vosu.env`, and canonical `vosu.toml` with precedence `vosu.toml > vosu.env > tosu.env > defaults`; it atomically replaces the canonical target before removing readable legacy files. Malformed canonical TOML leaves every source untouched at startup and preserves the last-good configuration during live reload.
- Made each overlay/counter package's root `settings.schema.toml` authoritative while retaining compact mutable values at `<config>/settings/<counter>.settings.values.toml`. A missing schema is created from root `settings.json`, or otherwise inferred from scalar root `settings.toml` (`string`→`text`, integer/float→`number`, boolean→`checkbox`); ambiguous arrays, tables, and datetimes are rejected without creating a schema or mutating package inputs. Old config TOML/JSON schemas remain lower-priority fallbacks and every legacy counter schema/value input remains untouched after durable root schema and values creation. Root JSON and TOML inputs remain untouched, and when both exist JSON defines the schema while TOML may seed values.
- Kept the upstream tosu name only where it identifies the compatibility source, provenance, parity fixtures, migration inputs, or the historical release record below. No permanent product aliases were added.

### In-game overlay removal

- Removed the retained in-game overlay configuration bindings, `/api/ingame`, dedicated browser assets, `__ingame__` storage, and the command WebSocket catalog flow. vosu no longer provides an embedded in-game overlay; external overlay software can continue to consume the remaining documented APIs.

### Dashboard

- Added persistent pinning and drag-and-drop ordering for installed counters, Shift-click URL opening, `/` search focus, and the reference magenta–iris–cyan dark dashboard palette.
- Replaced the inherited tosu dashboard bundle, fonts, icon font, and image assets with a dependency-free first-party Vosu interface; applied the approved brand mark to the dashboard, reports, file indexes, browser favicon, and multi-resolution Windows executable icon.
- Standardized installed-overlay previews on a responsive 5:3 viewport with a dark transparent canvas, aligned card rows and metadata footers, and preserved authored overlay backgrounds.

### API

- Added `beatmap.stats.timingWindows` to stable and lazer v2 responses across all rulesets, including map-aware speed mods, osu!lazer Difficulty Adjust values above OD 10, and osu!mania Classic/ScoreV2 windows. osu! and taiko temporal windows scale to real time by `clockRate`; osu!mania applies gameplay rate before floor-plus-half quantization and exposes the resulting raw chart-time windows for direct comparison with raw hit errors, so the displayed `perfect` window changes from 19.5 ms at 1.0x to 25.5 ms at 1.3x. osu!catch exposes its spatial `300` catch boundary in playfield units; unsupported judgements are omitted.
- Split every public hit-error field into ordered `all`, `press`, and `release` arrays, with osu!lazer mania hold-note releases classified separately.
- Corrected osu!lazer mania hold-release classification to read each hold's measured `TailNote` reference instead of its adjacent body reference, then match exact tail objects across interleaved chords and snapshot boundaries; osu! and taiko timing errors remain press-only.
- Added osu!stable hold-release classification on native osu!mania maps by correlating map judgement phases with exact replay key-state transitions, preserving ordered press/release arrays across misses and snapshot boundaries.
- Added ordered mode-agnostic key states to the regular v2 document and selected-field subscriptions. Each key reports its logical name, pressed state, count, and nullable edge timestamps; consumers read the active ruleset from the existing beatmap or play fields.

### Reliability

- Stopped reporting false `game.paused` during lazer/stable map start and retry clock holds by gating the play-time stall heuristic until time has advanced in the current play segment, and by reading osu!lazer's authoritative `DrawableRuleset.IsPaused` / gameplay-clock pause bindable when offsets provide it.
- Made `vosu -g`/`--generate-offsets` measure managed field offsets from the running client's on-disk assemblies (no live Satori DAC attach), fixing generation failures on official AppImages that ship Satori CLR with a stock-versioned DAC.
- Read lazer gameplay lead-in time from `FramedBeatmapClock.decoupledTrack` so `beatmap.time.live` advances during decoupled intro instead of waiting on the shared track/menu clock.
- Read osu!lazer play time from `Player.GameplayClockContainer.GameplayClock` during active gameplay so lead-in/decoupled intro time tracks the ruleset clock instead of the menu `beatmapClock` residual near 0–5 ms.
- Completed server-initiated WebSocket close handshakes with a bounded wait for peer acknowledgement, preventing Windows connection resets from dropping v2 subscription terminal close codes.
- Made the Windows updater helper tolerate the brief missing-record window that can occur while the parent atomically replaces the durable handoff record, without retrying malformed or unauthenticated records.
- Fixed lazer paused/repeated-time samples so they retain gameplay mode and mod identity without leaking gameplay or precise state after returning to the menu, preventing `.play.mods.rate`, realtime BPM, and osu!mania timing windows from flapping to NoMod between polls.
- Added standalone `-h`/`--help`, `-s`/`--setup`, and `-g`/`--generate-offsets` CLI actions. Offset generation measures managed field offsets from the running client's assemblies and atomically writes generator-produced offsets for its discovered version.
- Added a Linux startup warning when the running daemon lacks effective `CAP_SYS_PTRACE`, and made `process_vm_readv` permission denials emit one terminal error per osu! process generation instead of retrying silently.
- Replaced the root-only `--setcap` systemd installer with `--setup`, which accepts sudo or equivalent setcap authority, probes compiled eBPF and CN_PROC support, reports polling fallback, and narrows file capabilities to `CAP_SYS_PTRACE` plus only the loader capabilities required by a working eBPF backend. Official Linux archives and default local Cargo builds/installations now include `linux-process-events`; with `VOSU_PROCESS_EVENTS` unset, runtime automatically tries eBPF, then CN_PROC, then polling. If the maximum eBPF capability set cannot be applied, setup retries with ptrace-only privileges and still probes CN_PROC instead of aborting before fallback detection.
- Restored legacy counter compatibility by injecting the logical `window.COUNTER_PATH` into served counter HTML, allowing overlay settings WebSocket requests to identify the correct counter.
- Kept legacy overlay WebSocket deadlines anchored to their configured cadence across staggered client write-completion and catch-up wakes, preventing slow overlays from postponing 100 ms updates for fast overlays.
- Restored Linux gameplay snapshot freshness by reading valid process-memory batches directly instead of reparsing thousands of `/proc/<pid>/maps` entries for every primitive read; faults still retry through fresh mapping-aware validation.
- Coalesced ordered, non-overlapping same-page Windows process reads only when their span fits a bounded 1 KiB stack scratch, dispatched singleton `read_many` requests directly, and rejected/zeroed false full-length backend reads while retaining short-read retries and all-destination zeroing on errors.
- Reduced Linux process-event overhead with idle-poll bypass, bounded sorted queue coalescing, direct inspect/exit batching, delta-only eBPF tracked-map synchronization, early in-kernel candidate filtering, and reusable bounded CN_PROC receive storage; Linux 6.16+ also uses the `sched_process_exit` group-dead argument while older kernels retain the compatible fallback.
- Logged the effective process-event backend (`eBPF`, `CN_PROC`, or polling) and active bound server IP/port at startup, plus endpoint changes after successful listener rebinds.

### Documentation

- Updated installation, configuration, API, architecture, benchmark workflow, ownership, and release documentation for the vosu cutover.
- Recorded the durable clean-cutover and state-migration decision in [ADR 0007](openwiki/decisions/0007-vosu-clean-cutover.md).

## Historical releases

## 4.25.0 — Rust daemon cutover

This release replaces the shipped Node.js/Electron daemon package with one native Rust daemon while preserving the public **tosu** identity and version. Release archives remain `tosu-windows-v4.25.0.zip` and `tosu-linux-v4.25.0.zip`; each contains only the root `tosu.exe`/`tosu` executable and the authoritative `LICENSE`.

### Compatibility surface

- Retained the `tosu.env` bindings and defaults, the `tsosu.env` first-load migration, Windows program-directory storage, Linux XDG storage, counter discovery, central counter values, dashboard pages, and report surface.
- Retained the stable and lazer reader architecture, all four game modes, gosuMemory (`/json`, `/ws`), streamCompanion (`/json/sc`, `/tokens`), tosu v2 and precise state routes, the command WebSocket, counter APIs, and game/static file routes.
- Retained the public `tosu` executable, dashboard strings, data/config directory names, settings filenames, updater release channel, and existing counter data rather than introducing a `vosu` namespace.
- Kept legacy `/api/calculate/pp` on the pinned rosu-pp 4.0.1 behavior and moved live state calculation to the native Rust lazer calculator port.
- Embedded dashboard and offset inputs so release builds and normal runtime do not require Node.js, npm, Electron, CMake, .NET, or C#.
- Changed the Windows resource description to identify the executable as the **tosu Rust daemon**, rather than carrying the former TypeScript description.

This changelog does not claim native live parity for every osu! build. Supported processes still require matching memory patterns/offsets and native permission; release qualification must use the separately recorded stable/lazer live evidence.

### Intentional security differences

These changes deliberately reject unsafe or ambiguous inputs instead of reproducing exploitable behavior:

- HTTP and WebSocket authorization now requires `peer_ok && header_ok`. The normalized peer must be loopback or match an exact IP/four-component IPv4 wildcard entry; when a request supplies Origin/Referer, the selected header must also match an allowed hostname/IP token or `SERVER_IP`. An approved browser header alone no longer authorizes a hostile remote peer.
- User-supplied counter downloads require HTTPS in production, follow at most five validated redirects, reject HTTPS-to-HTTP downgrade, pin each connection to the addresses approved for that hop, and reject loopback, private, link-local, unique-local, CGNAT, multicast, reserved, benchmarking, and documentation-only destinations.
- Counter ZIP extraction rejects traversal, absolute paths, symlinks, excessive entries, and excessive expanded size. File routes reject traversal and symlink escape, and malformed/multiple/out-of-range media ranges return 416.
- POST bodies and WebSocket frames/messages are bounded. Unknown WebSocket paths return HTTP 404 rather than upgrading.
- Report and generated directory-index content is HTML-escaped.

### Intentional reliability differences

- Configuration writes use a same-directory temporary file, flush, and atomic replacement. Linux uses rename; Windows uses `ReplaceFileW`/`MoveFileExW` with bounded sharing-violation retries. Reload suppresses self-generated events and publishes one complete new snapshot.
- Listener reload binds a desired address before draining the old listener. A DNS/bind failure leaves the effective listener serving and retries with bounded backoff.
- Downloads validate status, timeouts, size, digest, and archive shape, clean partial files, and install from same-filesystem staging.
- Updates are single-flight and maintain an fsynced phase/owner/digest record. A copied helper performs crash-atomic replacement, waits for a private readiness signal, rolls back on candidate failure, and recovers or quarantines interrupted transactions according to verified target/backup digests.
- Windows archive/OneDrive guards run before replacement. The update API responds before the old daemon drains HTTP/WebSocket work and hands off.
- Logging uses a bounded asynchronous writer and flushes on fatal/graceful shutdown so memory polling does not wait on ordinary disk writes.

### Daemon-only overlay status

- The Electron/asdf-overlay in-game overlay and its updater are intentionally not shipped or launched in this release.
- `ENABLE_INGAME_OVERLAY`, its keybind/FPS settings, `/api/ingame`, `/websocket/commands`, and `__ingame__.values.json` remain recognized for configuration/data and browser-client compatibility.
- Setting `ENABLE_INGAME_OVERLAY=true` is preserved on disk but emits `In-game overlay is unavailable in the Rust daemon-only build; ENABLE_INGAME_OVERLAY is ignored.` The daemon never silently rewrites it to false.

### Build and delivery

- Pinned the workspace to Rust 1.96.1 and edition 2024, with locked dependencies and native Windows/Linux CI.
- Native CI runs formatting, Clippy, workspace all-target/all-feature tests, process-memory self-read coverage, updater recovery coverage, and locked no-default-feature release builds.
- Release-profile builds reject the development-only `test-harness` feature.
- Tagged Windows builds retain SignPath signing and verify the signed executable's version/icon/resource fields before publishing.
- Packaging validates the exact two-entry archive root and publishes only the Windows and Linux daemon ZIP assets.

### Licensing and retained development references

- This historical release was licensed `LGPL-3.0-only`; its existing root `LICENSE` was unchanged and authoritative at that release.
- Added third-party notices for embedded Vue, Roboto, IcoMoon-generated icons, retained images/static assets, Rust crates, the pinned rosu/osu! calculator inputs, and the pinned lazer-calculator reference.
- Retained oracle generators, source-revision manifests, reference sources/licenses, and committed golden fixtures for reproducible parity work. Generated Python bytecode and oracle/package build caches are not release inputs and are removed.

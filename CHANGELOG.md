# Changelog

All notable user-visible changes to vosu are documented here. Entries below the Unreleased section are retained release history and use the product names and artifact names that were current at the time.

## Unreleased

### Product identity and clean cutover

- The clean-cutover vosu package and release line starts at **0.1.0**.
- Renamed the current product, Rust package, executable, API terminology, logs, and updater metadata to **vosu**. Workspace crates are now `vosu-lazer-pp` and `vosu-test-harness`.
- Renamed configuration and operational interfaces to `vosu.env`, `VOSU_*`, vosu XDG roots, and `.vosu-update.json`. Current operation does not accept former branded aliases.
- Release archives are now `vosu-linux-<tag>.zip` and `vosu-windows-<tag>.zip`, each containing the matching `vosu` or `vosu.exe` root executable. Automatic updates follow `shiruviaa/vosu`.
- First-party catalog, offset, and identity URLs now use <https://vosu.app>.
- Added an idempotent, one-time migration for existing state. Linux moves non-conflicting files from former `tosu` XDG config, data, and cache roots into the corresponding `vosu` roots. On every platform, former `tosu.env` values fill gaps in `vosu.env`; current values win, deprecated bindings are dropped, and the former file is removed only after successful replacement. Normal operation then reads and writes only vosu locations.
- Kept the upstream tosu name only where it identifies the compatibility source, provenance, parity fixtures, migration inputs, or the historical release record below. No permanent product aliases were added.

### In-game overlay removal

- Removed the retained in-game overlay configuration bindings, `/api/ingame`, dedicated browser assets, `__ingame__` storage, and the command WebSocket catalog flow. vosu no longer provides an embedded in-game overlay; external overlay software can continue to consume the remaining documented APIs.

### Dashboard

- Added persistent pinning and drag-and-drop ordering for installed counters, Shift-click URL opening, `/` search focus, and the reference magenta–iris–cyan dark dashboard palette.

### API

- Added `beatmap.stats.timingWindows` to v2 responses for stable and lazer across all rulesets, including map-aware speed mods, osu!lazer Difficulty Adjust values above OD 10, and osu!mania Classic/ScoreV2 windows. osu!catch exposes its spatial `300` catch boundary in playfield units; unsupported judgements are omitted.
- Split every public hit-error field into ordered `all`, `press`, and `release` arrays, with osu!lazer mania hold-note releases classified separately.

### Reliability

- Completed server-initiated WebSocket close handshakes with a bounded wait for peer acknowledgement, preventing Windows connection resets from dropping v2 subscription terminal close codes.
- Made the Windows updater helper tolerate the brief missing-record window that can occur while the parent atomically replaces the durable handoff record, without retrying malformed or unauthenticated records.
- Fixed dashboard navigation links so they no longer trigger counter-folder API calls, and made both folder controls open the configured counter root without fake executable names.
- Added a Linux startup warning when the running daemon lacks effective `CAP_SYS_PTRACE`, and made `process_vm_readv` permission denials emit one terminal error per osu! process generation instead of retrying silently.
- Restored legacy counter compatibility by injecting the logical `window.COUNTER_PATH` into served counter HTML, allowing overlay settings WebSocket requests to identify the correct counter.
- Kept legacy overlay WebSocket deadlines anchored to their configured cadence across staggered client write-completion and catch-up wakes, preventing slow overlays from postponing 100 ms updates for fast overlays.
- Restored Linux gameplay snapshot freshness by reading valid process-memory batches directly instead of reparsing thousands of `/proc/<pid>/maps` entries for every primitive read; faults still retry through fresh mapping-aware validation.
- Reduced Linux process-event overhead with idle-poll bypass, bounded sorted queue coalescing, direct inspect/exit batching, delta-only eBPF tracked-map synchronization, early in-kernel candidate filtering, and reusable bounded CN_PROC receive storage; Linux 6.16+ also uses the `sched_process_exit` group-dead argument while older kernels retain the compatible fallback.

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

- The project remains `LGPL-3.0-only`; the existing root `LICENSE` is unchanged and authoritative.
- Added third-party notices for embedded Vue, Roboto, IcoMoon-generated icons, retained images/static assets, Rust crates, the pinned rosu/osu! calculator inputs, and the pinned lazer-calculator reference.
- Retained oracle generators, source-revision manifests, reference sources/licenses, and committed golden fixtures for reproducible parity work. Generated Python bytecode and oracle/package build caches are not release inputs and are removed.

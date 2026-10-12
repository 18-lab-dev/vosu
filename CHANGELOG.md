# Changelog

All notable user-visible changes to vosu are documented here. Older sections are retained release history and use the product names and artifact names that were current at the time.

## Unreleased

### Packaging

- The AUR package is now `vosu-git`, built from `main` with `packaging/aur/PKGBUILD`. Releases no longer include the `vosu-bin-aur-v<version>.tar.gz` bundle.
- Release files, commits and tags are signed with an OpenPGP release key (`6F0F 0D30 C09D 3A03 6246  A810 2CB6 524B B54B 2CDA`), starting with the published 1.0.1 files; each `<file>.sig` sits next to its file on the release page. The public key is `packaging/release-signing-key.asc`.

### Licensing

- tosu's copyright holder, Mikhail Babynichev, gave written permission on 2026-10-12 for vosu's tosu-derived Rust (memory readers, API, state and related code) to be distributed under MIT OR Apache-2.0 with tosu credited. `THIRD_PARTY_NOTICES.md` and the provenance audit record the permission and its scope; it does not cover the LGPL calculator wrapper reference.

## 1.0.1 — 2026-10-12

### Dashboard

- The **Available** page notice now names tosu.app and osuck.link, states that vosu is not affiliated with the tosu project and does not operate either service, and points catalog or download outages to the tosu project.
- A failed overlay install names the download host when the host caused it, including a host that answers with a text page instead of a ZIP archive (osuck.link does this when rate-limiting), instead of reporting an opaque ZIP error.

### Licensing

- The vosu logo and icons are now licensed separately from the code, under CC BY 4.0 crediting sxlvia, with an extra permission to show them without credit when using or talking about vosu. The terms are in `LICENSE` and ship in every release archive and package.

## 1.0.0 — 2026-10-10

### Dashboard

- Move navigation and search into the top bar; remove the retired local-overlay guide and schema builder.
- Show larger installed cards with enlargeable live previews, and list catalog overlays with lazy-loaded preview images and their metadata.
- Adapt the dashboard to the screen: catalog rows sit side by side when there is room, previews follow the window height so short laptop screens show more cards, catalog text scales with its card, and the whole layout grows slightly on large monitors. Phones get a compact header with full-width page tabs and settings checkboxes beside their labels; short landscape screens no longer pin the header.
- Autosave runtime settings with interval/port validation; preserve the unsaved/error status until every rejected field is corrected.
- Show a community-overlays notice above the **Available** listing: listed overlays are third-party work from the tosu catalog, run with the dashboard's access, and should be installed only when trusted. It links the upstream catalog and the privacy policy.
- Describe **Automatic updates** accurately: turning it off stops startup installation, but release builds still check for a new version at every start.

### Packaging and desktop

- Publish Debian/Ubuntu (`.deb`), Fedora (`.rpm`), and Arch (`.pkg.tar.zst`) packages next to the portable ZIPs, plus a rendered AUR `vosu-bin` PKGBUILD bundle. Packages install `/usr/bin/vosu`, an application launcher, and icons, and run `vosu --setup` on install and upgrade without failing the transaction when capabilities cannot be set.
- Build Linux releases against glibc 2.35, so the binary runs on Debian 12+, Ubuntu 22.04+, Fedora, and Arch; release CI asserts the baseline and install-tests every package in clean containers.
- Show a tray icon on Linux (StatusNotifierItem) and Windows with **Open dashboard** and **Quit**; headless sessions without a tray host run unchanged.
- Launching vosu while another instance already serves the configured address opens that instance's dashboard and exits instead of failing to bind.
- Add the Linux-only **Start at login** setting (`START_AT_LOGIN`), which manages a marked systemd user unit for the graphical session.
- Treat an executable whose directory the user cannot write (for example a package-managed `/usr/bin/vosu`) as externally managed: skip self-update checks, reject `POST /api/runUpdates` with an explanation, and disable the dashboard's automatic-update setting.
- Build tagged releases from a clean target directory without restoring any Cargo cache. Windows releases stay unsigned until SignPath signing is enabled; signed releases must carry a timestamped signature issued to SignPath Foundation.

### Security

- Prevent counter-delete CSRF and protect the shared management mutation boundary with exact same-origin browser provenance. Counter delete/download/open, updater, and report actions now require POST; no mutating GET aliases remain. The dashboard uses the new methods.
- Preserve headerless/file-origin read-only overlay access and stock settings commands. Hosted counters can still save settings without new tokens or code; non-hosted sockets may read settings but cannot save them.
- Management writes require exact HTTP scheme/host/port browser provenance; `Origin` is authoritative, with `Referer` fallback only when absent. Missing, null, malformed, or duplicate provenance cannot authorize writes. Non-hosted settings sockets retain reads but close with 1008 before saving or relaying mutations.
- Made an unprivileged `vosu --setup` request OS-owned authentication through a trusted polkit `pkexec` boundary before any capability change. Root execution skips authentication; cancellation, denial, unavailable/noninteractive elevation, and missing `pkexec` fail without invoking `setcap`. Vosu never reads credentials and has no sudo fallback.
- `vosu --setup` warns before the authentication prompt when the executable or its directory is writable without root, as with a portable ZIP: the polkit prompt authorizes whatever file currently sits at that path. Setup still continues so portable installs can receive capabilities.
- Bound native telemetry, calculator inputs, counter installation and transport work without losing stable hit-error catch-up, ordered filter precedence or settings refreshes during receive-side connection contention. Preserve per-recipient control-queue backpressure.

### Runtime and maintenance

- Selected-field v2 subscriptions now sample game state on their own cadence. While any selection is established, both readers run every `V2_SUBSCRIPTION_DEBOUNCE` milliseconds, so subscribed fields can change before HTTP and compatibility WebSocket payloads. Those payloads keep their `POLL_RATE`/`PRECISE_DATA_POLL_RATE` cadence; beatmap changes and retries still reach them immediately. The osu!stable stall-pause heuristic no longer depends on how often state is sampled.
- `V2_SUBSCRIPTION_DEBOUNCE` now defaults to `1` with a minimum of `1`. Saved values below the minimum clamp to `1`, and settings saves below `1` are rejected.
- Disable Nagle's algorithm (`TCP_NODELAY`) on every accepted connection. Small WebSocket state frames written while an earlier frame was still unacknowledged waited for the client's delayed ACK (about 40 ms on Linux loopback) and then arrived in bursts; at the 1 ms selected-field cadence this showed up as live-time delivery jitter.
- Fix automatic offset generation for lazer framework upgrades, including `2026.1005.0` on .NET 10: run both embedded helpers with the exact client's bundled CLR/framework in disposable staging instead of measuring under the helper's .NET 8 build runtime. No user-installed SDK, runtime download, or game-file modification is needed.
- Include client-published NuGet framework replacements and their transitive dependencies in the staged helper runtime. Native Windows startup with the client's .NET 8 runtime and `System.Text.Json`/`System.IO.Pipelines` 10 now resolves the game and serves its live API without changing the game or verified helper payload.
- Route maintained Windows autoresearch benchmarks through the shared `windows-vm` command: transfer filtered source/vendor archives with SFTP, extract through native PowerShell, and clean temporary archives on success or failure. Sources, Cargo vendor caches, builds, and run output stay on `V:\agent-workspaces` instead of an account-specific C: profile.

- Fix mania gradual-performance completion so `last()` and oversized skips return the final attributes before exhaustion; correct the osu! gradual example to finish without consuming the calculator before checking exhaustion.
- Make the daemon reload smoke verify rejected malformed TOML through unchanged served settings, untouched source bytes, and an unbound requested port, independently of whether osu! is running. Production last-good configuration and state-readiness behavior is unchanged.
- Split maintained code into domain-focused modules capped at 1,000 lines, preserving API and calculator behavior; remove tracked offset-tool build output while retaining source, locks, and provenance.
- Use the upstream tosu catalog service at `https://tosu.app/api.json`; vosu does not operate a website. Use `vosu/<version> (+https://github.com/shiruviaa/vosu)` as the first-party request identity.
- Derive Windows textual and numeric executable-resource versions from the root Cargo package version, keeping both resource version fields consistent with the package.

- Remove the report-only graphics stack; use Linux sysfs and Windows WMI hardware metadata, with explicit unavailable/unknown states.
- Bound report log retention with visible truncation notices; isolate counter discovery, report capture, configuration persistence/reload, and ordered console output from async workers.
- Avoid primary API graph/hit-error encoding copies, share live-calculator map preparation and unchanged stable mania replay batches, and bound WebSocket pong backpressure.
- Use component-aware case-insensitive Windows startup-path checks rather than string-prefix matching.
- Separate MSVC SDK, Cargo downloads, and build caches; restrict cross SDK downloads to x86_64; classify changes conservatively and always publish `CI required`, including docs-only changes.
- Move packaged-executable smoke checks into `vosu-release-smoke`, remove duplicate daemon asset embedding, and maintain existing Markdown without scheduled inference. Linux process-event backends and defaults remain unchanged.
- Pin release toolchain actions to reviewed commits and limit repository-write permissions to the release-publishing job. Documentation-only changes retain required CI checks without triggering native compilation.
- Add `CONTRIBUTING.md` with per-platform setup, the local check set, and a repository map; move the historical offsets-asset reproduction recipe from the README into `docs/operations/offsets-asset.md`. Remove the vendored third-party agent skills, personal agent settings, the superseded Windows verification batch script, and the retired docs instruction stub; the Linux process-event benchmark now lives at `tools/autoresearch/process-events-bench.sh`.
- Replace remote/manual offset acquisition with automatic exact-instance resolution: valid versioned `<version>.json` cache entries are preferred, then matching embedded offsets, then local generation from the exact PID and version. The extracted helper cache is keyed by the embedded bundle checksum; no bundle identity is added to offset-file metadata. Release binaries embed the platform-matching self-contained generator; end users do not install .NET, and debug builds without a prepared payload report an explicit missing-bundle error.
- Maintainer source builds prepare `target/offset-generator/<rid>/offset-generator-v1-<rid>.zip` plus its `.zip.sha256` sidecar with the .NET 8 SDK. Release archives remain exactly the executable and root `LICENSE`; updater behavior is unchanged.

- Consolidate the five retained autoresearch lanes: reduce Linux read/scanner allocation work, combine lazer key-read stages, avoid per-byte stable mod-checksum allocations, reserve v1 serialization capacity and sort StreamCompanion strains in place, share selected-field comparison results, and reuse immutable calculator source maps.
- Retain the Linux sparse-large iovec capacity change with a measured limitation: 65,536-request/33-active spread/back probes improved 10.64–13.54%, while all-empty and 32-active controls regressed 2.64–3.83%. These are synthetic native self-process measurements, not a general daemon speedup; see the [benchmark guide](docs/testing/benchmarks.md#five-lane-autoresearch-consolidation-2026-09-08).
- Merge all five reopened lane histories and benchmark the combined source against both the original optimization base and previous integration. Production calculator preparation and shared subscriptions improved on the measured fixtures; native lazer precise capture was 16.04% faster than previous while requested bytes rose 570 → 912 per cycle.
- Preserve the final measured limitations: sparse 33-front/64-spread controls were 16.48%/13.69% slower than the original base, distinct-plan fast-writer subscriptions were 4.03% slower than previous, and mania parsing was 4.89% slower than previous. These are workload-specific results, not a general daemon speedup; see the [final combined measurements](docs/testing/benchmarks.md#final-reopened-lane-consolidation-2026-09-08).
- Qualify the retained parsing regression as archived-binary/protocol-specific: follow-up diagnosis found unchanged allocations and negligible instruction-work changes, while a disposable diagnostic reversed the timing sign with setup both present and absent. The exact cause remains unresolved; no parser speedup or fix is claimed. Preserve the original unfavorable numbers and see the [parsing interpretation and local evidence](docs/testing/benchmarks.md#combined-measurements).
- Keep live-parity attestation compatible with protected Git for Windows/MSVC build layouts; require initialized client settings before promoting the menu barrier. Compare shared fields strictly while recognizing only vosu's documented additive hit-error arrays and `keys.all` paths. Distinguish stale Windows exit snapshots from new clients without relaxing PID/creation-time ownership checks.
- Attest the exact published compressed Linux parity reference by its pinned executable SHA-256 rather than requiring an uncompressed version string; retain ELF, configured-digest, ownership, and strict comparison checks.
- Preserve the Linux live-parity socket-ownership cache across CLI module composition; keep its 5 ms lifetime and exact process/connection ownership checks unchanged.
- Require the exact Linux production build command, including `linux-process-events`, when verifying live-parity build attestation; keep the Windows command and all capture/comparison gates unchanged.
- Preserve the attested executable search order on POSIX when constructing the reference-build PATH, so system Rustup proxies cannot shadow a protected Rust toolchain; continue rejecting untrusted paths and cross-tool shadowing.
- Build the pinned upstream Node reference on Linux without requiring `cmd.exe`; retain protected `cmd.exe` and MSVC discovery requirements on Windows.
- Mark Linux osu!stable through Wine as experimental and not release-verified; retain its existing discovery/read implementation without claiming a live-parity pass.
- Record clean strict Windows stable, Windows lazer, and native Linux lazer release-gate captures with exact source/artifact identities; preserve failed sampling-boundary captures without claiming those boundaries are fixed.

### Documentation

- Move maintained Markdown from `openwiki/` to `docs/`, update live navigation and relative references, and document the reviewed-maintenance workflow without generator dependencies.
- Add a [privacy policy](PRIVACY.md) covering every network request (GitHub update checks and downloads, the tosu catalog, overlay download hosts, and catalog preview images), the local API, and locally stored data.
- Add a [code signing policy](CODE_SIGNING_POLICY.md) for SignPath Foundation signing of the Windows executable, link it from the README and release notes, and add uninstall instructions.
- Document the community-overlay licensing position in the third-party notices, and record the decision to keep counters on the dashboard origin ([ADR 0013](docs/decisions/0013-counters-share-dashboard-origin.md)).
- Add a [security and installation disclosure](docs/security.md) (memory access, Linux capabilities and process-event backends, installed files, system changes, network exposure) and a [security policy](SECURITY.md) for private vulnerability reports.
- Record the final released-tosu comparison on native Windows and the Linux VM, keeping whole-daemon resources separate from synthetic reader latency and preserving the unfavorable Linux VM precise-reader result. See the [measured protocol and limitations](docs/testing/benchmarks.md#released-tosu-comparison-2026-10-09); those measurements describe the recorded 0.3.0 generation, not the final 1.0.0 dashboard build.


### Gameplay

- Reset published hit-error arrays when osu!lazer or osu!stable rebuilds the live hit-event list, so editor test-mode section restarts drop pre-restart errors and skipped-object zeros instead of waiting for those notes to elapse again.
- Track upstream tosu changes through `2359305a63f2aaebb201bc8d2eee58177f52590e`: expose lazer multiplayer rooms and optional avatars, preserve falsey stable settings, recognize multiplayer ScoreV2, and calculate unstable rate incrementally using the active clock rate.
- Retain lazer gameplay score, hit counts, accuracy, rank, and modifiers while paused; entering song selection still clears the previous play epoch.

### Licensing

- Correct first-party attribution to `Copyright (c) 2026 sxlvia` in the root/calculator license copies and Windows resources, preserving genuine upstream copyright and canonical MIT/Apache terms.
- Include the six retained calculator MIT texts (rosu-pp, rosu-map, osu!, osu-framework, osu-tools and dotnet/runtime) in the root release `LICENSE` without changing the updater's two-entry ZIP contract; reject stale license members and bad ZIP CRCs.
- Qualify unsupported source-independence claims, inventory locked dependency license declarations, and record concrete source matches and unresolved production-file origins in [the provenance audit](docs/provenance-audit.md). Map the current calculator to its pinned permissive sources and keep the former wrapper only as a historical parity reference and black-box oracle.

### Calculation

- Replaced the former LGPL-declared native live calculator with a Rust facade and pinned MIT `rosu-pp` 4.0.1 / `ppy/osu` sources; retained the former calculator tree only as non-production historical parity evidence. The replacement commit's clean-room characterization is not a certification of source independence; see the provenance audit.
- Update max-achievable/full-combo score and accuracy projections, nested tick-score metadata, long-slider/spinner strain graph bounds, Hidden slider opacity, and source-calculator performance clamps. Regenerate current API/calculator fixtures while retaining historical source and NUnit provenance.

- Report converted osu!→mania display circle size using the source-OD fallback with ties-to-even rounding and 4–7 bounds, then apply key-count overrides and Dual Stages doubling. Native mania display and playable-map difficulty/PP remain unchanged.
- Refresh the embedded lazer offsets for osu! `2026.921.0`, including room-user fields, with version-matched runtime-derived `GameBaseVtable` evidence.

### API

- Add mode-aware `beatmap.stats.hitWindow` judgement windows, including upstream's zero-valued Catch windows; normalize StreamCompanion token-array commands without broadening other WebSocket command paths.

- Restored the tosu-compatible hit-error arrays at `hitErrorArray` and `hitErrors`; ordered press and release classifications now use additive sibling `pressHitErrors` and `releaseHitErrors` fields.
- Matched gosuMemory v1 replay visibility and stable tournament spectator global-rank fields to current tosu behavior instead of emitting fixed defaults.
- Replaced `beatmap.stats.timingWindows` with Storycraft's merged tosu `beatmap.stats.hitWindow` contract and its mode-specific `miss`, `meh`, `ok`, `good`, `great`, and `perfect` keys.
- Replaced the separate `keypresses` object with a tosu-compatible `keys` object. Regular and precise v2 responses retain `k1`, `k2`, `m1`, and `m2` and add the ordered mode-agnostic controls under `keys.all`, matching the planned upstream port.

## 0.2.0 — 2026-07-28

### Product identity and clean cutover

- The clean-cutover vosu package and release line starts at **0.1.0**.
- Renamed the current product, Rust package, executable, API terminology, logs, and updater metadata to `vosu`. Workspace crates are now `vosu-pp` and `vosu-test-harness`.
- Canonical server configuration is the flat, typed `vosu.toml` document with uppercase binding names. Former environment-variable interfaces are migration inputs only; current operation does not accept them as configuration aliases.
- Release archives are now `vosu-linux-<tag>.zip` and `vosu-windows-<tag>.zip`, each containing the matching `vosu` or `vosu.exe` root executable. Automatic updates follow `shiruviaa/vosu`.
- At the time of the 0.2.0 release, first-party catalog, offset, and identity URLs used <https://vosu.app>.
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
- Recorded the durable clean-cutover and state-migration decision in [ADR 0007](docs/decisions/0007-vosu-clean-cutover.md).

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

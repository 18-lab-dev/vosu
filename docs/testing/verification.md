---
type: Verification Guide
title: vosu testing and verification
description: Test selection and evidence workflow for Rust unit/integration contracts, native process and updater checks, live parity, precise input validation, release smoke tests, and Criterion benchmarks.
resource: AGENTS.md
tags: [testing, ci, parity, benchmarks, release]
---

# Testing and verification

## Fast selection by change

- API projection/routing: affected tests plus `cargo test --locked --test state_api_compat` and/or `server_compat`; add `daemon_smoke` for startup/serving changes.
- Configuration/counters: affected unit tests plus `cargo test --locked --test config_compat`; cover flat typed TOML parsing, precedence `vosu.toml > vosu.env > tosu.env > defaults`, canonical replacement before legacy removal, malformed startup/reload preservation of last-good state, watcher handling, and atomic writes.
- Process/memory: affected tests plus `cargo test --locked --test process_memory -- --nocapture` on every affected native OS.
- Updater/release: `updater_compat`, native `updater_native`, and `VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture` for package/startup changes.
- Calculation/state: `vosu-pp` tests, `performance_invariants`, and `state_api_compat`/`server_compat` when observable schemas or timing can change; for v2 stats, cover parsed/missing maps, modes, mods, clock rate, and catch timing-window semantics. The oracle-backed `live_contracts` and `oracle_parity` tests require the crate’s `oracle-debug` feature. Live parity is optional ([ADR 0011](../decisions/0011-live-parity-optional.md)).
- Compatibility payloads: assert the original `hitErrors`/`hitErrorArray` arrays plus additive sibling `pressHitErrors` and `releaseHitErrors` arrays across v1, stream companion, v2, precise, play, and tournament fixtures; include the lazer mania hold release-correction case.
- Hot paths: the affected Criterion group in `benches/hot_paths.rs`, compared on the same machine; record observations in the [benchmark guide](benchmarks.md), not as universal thresholds.

## CI baseline

The required checks in `AGENTS.md` and `.github/workflows/ci.yml` are:

```sh
python scripts/check-doc-links.py
python scripts/check-provenance.py --package-fixtures
python -m unittest discover -s scripts -p test_package_release.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --lib --bins --tests --examples --all-features --locked
cargo xwin clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo test --locked --test process_memory -- --nocapture
cargo test --locked --test updater_native -- --nocapture
```

CI always publishes `CI required`, including documentation-only PRs. The cheap classification job checks local Markdown links without network or inference. Code changes run Linux Clippy/tests and a Windows MSVC cross-check; shared/platform/build/workflow changes also run native Windows verification. Pure schema/calculator changes retain cross-checks without automatically running native Windows. Manual dispatch runs both lanes. Branch protection should require `CI required` rather than conditionally skipped platform jobs. Cross-compilation is not native execution evidence.

MSVC SDK caching is independent of Cargo.lock and restricted to x86_64. Prepared SDKs are saved before later lint failures. Rust artifact caches use separate target/lane keys and weekly generations to limit archive churn; downloads are cached separately. The Ubuntu validation lane builds with `CARGO_PROFILE_DEV_DEBUG=line-tables-only`: full debuginfo made the all-target test build large enough to exhaust the hosted runner's disk. Tagged releases package, extract, and exercise the exact Linux/Windows archives, using a small smoke-test package that does not depend on the daemon crate.

## Evidence workflows

`tools/live-parity/` is an optional interactive diagnostic, not a normal unit test or a required check ([ADR 0011](../decisions/0011-live-parity-optional.md)). Use it to investigate a suspected divergence from upstream: validate configuration, capture a controlled client lifecycle, compare, and generate diffs. It requires pinned artifacts, distinct loopback ports, and supported platform/client combinations. The dated captures below are evidence for their recorded source generations; statements in them that live parity gates a change predate that decision.

Native-reference mode also accepts the exact compressed upstream Linux 4.26.2
ELF (SHA-256 `fa22300bcdc88c3b18a11fd5ff70d7057530b4c4b9413a525eb92044951b034f`).
Its version string is not stored as plain ASCII. The digest is pinned in the
harness from the [published release archive](https://github.com/tosuapp/tosu/releases/download/v4.26.2/tosu-linux-v4.26.2.zip),
whose GitHub-published SHA-256 is
`6ee56962171dda360e62abed6fa52859cc64500cf3bdbb451a71b125cc452867`.
A configured digest alone cannot admit another compressed executable, and
ELF magic is still mandatory. This reference attests the published native
release, not a build of source revision `2359305`; report that distinction
with Linux capture evidence. Client identity, ownership, transport, lifecycle,
cadence, coverage, and comparison requirements are unchanged.


HTTP acquisition keeps one serial reusable connection for each daemon/schema
stream (eight streams total), drains every response, and rechecks the attested
process, listening socket, and exact established connection for every request.
Reusing a connection does not reuse its ownership observation: each sample must
have a fresh check beginning after the preceding sample and completed before
its own response timestamp. Failed requests remain failed samples and are not
replayed. Peer-announced closure permits a new connection on the next sample.
Each stream has a hard limit of 256 connection attempts; exceeding it fails the
capture rather than allowing unbounded churn. This bounds the collector to
2,048 HTTP connection attempts without changing the sample cadence, lifecycle
coverage, comparison tolerances, or OS TCP settings.

Capture metadata records per-stream `http_transport.connection_attempts` and
`request_attempts`. The durable HTTP endurance regression exercises 3,000 rounds
across all eight streams (24,000 responses), including length-framed, chunked,
and exact not-ready responses, with real native socket ownership observations.
It requires eight accepted connections and a fresh valid binding for every
sample, and rejects missing or reused ownership checks. Run it with
`python -m unittest discover -s tools/live-parity -p 'test_live_parity_http.py' -v`;
the full `test_*.py` suite also covers it.

`tools/input-validation/` is a separate `uv` Python harness for precise keypress validation. Prefer dry-run map selection, navigation, autoplay, geometry, and live-autoplay before real input injection. Capture before the first press and after the final release. Treat source-reported `countDelta` as authoritative; sampled edge events are cadence-dependent and cannot alone prove losslessness.

## Configuration evidence

For configuration changes, inspect the effective platform config root's `vosu.toml` as a flat TOML document. Uppercase binding names must use TOML booleans, numbers, or quoted strings; for example, `SERVER_PORT = 24050.0` is a typed TOML number, not a dotenv assignment. Verify startup precedence `vosu.toml > vosu.env > tosu.env > defaults`, that canonical replacement completes before either readable legacy input is removed, and that malformed canonical input does not trigger a partial migration.

During watch reload, malformed UTF-8 or TOML must leave the last-good configuration and dependent resources active. A valid change is published only after `register_before_publish` work completes; failed listener replacement retains the previous listener. Programmatic writes should be debounced and use a same-directory temporary file plus atomic replacement, with a failed replacement leaving the current target intact.

Criterion benchmarks cover scanner/snapshot reads, beatmap parsing/calculation/cache behavior, serializers, publication fan-out, and selected-field subscriptions. They use synthetic fixtures and do not measure real process syscalls, browsers, network backpressure, or full snapshot construction.

## Test layout

`tests/` contains compatibility (`state_api_compat`, `server_compat`, `config_compat`, `updater_compat`), native (`process_memory`, `updater_native`), daemon smoke (`daemon_smoke`), and invariant (`performance_invariants`) contracts. `crates/vosu-release-smoke/` tests an external packaged executable. Focused unit/service tests live beside their owning modules. `crates/vosu-test-harness/` supports integration tests only.

Keep unit tests for distinct consumer-visible behavior: boundaries, precedence, errors, state transitions, security, numerical results, and concurrency or resource bounds. Do not add tests that only repeat constants/help text, construct and read back fields, prove standard-library clone/ownership behavior, or echo arguments through mocks. When committed source-oracle fixtures already cover a wire contract, add a focused unit test only for an uncovered case; do not duplicate the same default or not-ready payload. Preserve provenance-pinned upstream tests through their owning workflow rather than culling them as first-party tests.

When a change crosses [runtime boundaries](../architecture/overview.md), run the narrowest affected contract first and then the platform/live gate appropriate to the observable behavior. Do not substitute a compile-only check for behavior verification.

## Release preparation checks — 2026-10-08

The initial checks below exercised a temporary, uncommitted 1.0.0 working tree, including the existing client-runtime offset-generation and hit-error epoch fixes. The version bump was subsequently deferred for dashboard rework; the source version at that time remained **0.3.0**. These checks are not evidence of publication, signing, or a new live osu! parity capture.

- `python scripts/check-doc-links.py` passed for 31 maintained Markdown files after the `docs/` cutover. A disposable CLI probe rejected a missing link inside the maintained documentation tree and passed once its target existed.
- `python scripts/check-provenance.py --refresh-license-hash --refresh-dependency-inventory --package-fixtures` passed. Cargo regenerated the root package lock entry as 1.0.0; all 362 third-party dependency identities and license-inventory package records remained unchanged. The four release-license tests passed, and `v1.0.0` tag validation matched the manifest.
- `go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 .github/workflows/ci.yml .github/workflows/release.yml .github/workflows/version-build.yml .github/workflows/benchmarks-windows.yml` passed. Release toolchain actions are commit-pinned; only publication has repository-write permission. Documentation/agent-only classification probes skipped Rust jobs, while code, manifest, and workflow probes retained their applicable gates.
- Rust 1.96.1 passed `cargo fmt --all -- --check`, Linux workspace/all-target/all-feature Clippy with warnings denied, both the full workspace test command including doctests and the explicit CI test-target command, and Windows MSVC cross-Clippy with `C_INCLUDE_PATH` unset.
- Both platform generator bundles were rebuilt through `scripts/prepare-offset-generator.py`. The Linux production command with `linux-process-events` built successfully. `scripts/package-release.py` packaged and extracted the actual `vosu-linux-v1.0.0.zip`, containing exactly `vosu` and the refreshed root `LICENSE`.
- The extracted executable's SHA-256 was `954c350183a75c08ad9e76fca7b3f269a1357785c19e93210c33ba4c7b7ba166`. Its packaged-production smoke passed. A separate isolated launch logged version `1.0.0`, served `/` with 200, rejected headerless report POST with 403, accepted same-origin report POST with 200, and exited successfully after native SIGINT.
- `python -m unittest discover -s tools/live-parity -p 'test_*.py'` passed all 156 tooling tests. CLI help advertised vosu 1.0.0; this does not certify a live client capture.
- All four `.claude/skills` symlinks resolved to retained `.agents/skills` directories. `CLAUDE.md` delegates to `AGENTS.md`; the local read-only `tokei` permission and third-party skill sources were preserved.

Native Windows execution remains unverified for this generation: `windows-vm status` reported Windows disabled/inactive and SSH unavailable; Windows MCP health reported missing QMP and no framebuffer. No VM was started. Historical Windows/native parity results below retain their original source and artifact identities and do not establish a pass for this working tree.

### Final deferred-version generation

- Cargo regenerated the root package back to `0.3.0`. The resulting `Cargo.lock` is semantically identical to the pre-preparation commit, including every third-party package version and checksum. The live-parity target and README archive names also remain `0.3.0`.
- The `docs/` link check passed for all 31 maintained Markdown files. Refreshed licensing/inventory/package fixtures passed; all four release-license tests passed; tag validation accepted `v0.3.0`. Only the changelog's historical migration statement retains the old documentation-directory name.
- Rust 1.96.1 formatting, Linux workspace/all-target/all-feature Clippy, full workspace tests including doctests, Windows MSVC cross-Clippy with `C_INCLUDE_PATH` unset, both generator bundle preparations, and the Linux production build completed successfully.
- The actual `vosu-linux-v0.3.0.zip` contained exactly `vosu` and `LICENSE`. Its extracted executable SHA-256 was `54b3f0bac42393e9201b233d4e47f661985376f589db07d1f8e78c4826ab1a8a`; packaged-production smoke passed. An isolated launch logged `0.3.0`, served `/` with 200, rejected a report POST without `Origin` with 403, accepted its exact same-origin report POST with 200, and exited with 0 after native SIGINT.
- Local package/runtime evidence is retained in `/home/user/.cache/vosu-final-20261008-4r3azwf2/`. Native Windows execution remains unavailable and unverified; the VM was not started. No dashboard changes, version bump, publication, or push were performed.

### Final native benchmark generation — 2026-10-09

The verified production implementation is committed as
`230bde863b707c1ecac9b20bf9992290c20909e1`. The native builds used
`f075408209ee98cdd780f3a4d386d2a9850f40b0` plus the client-runtime
framework-dependency closure fix. Its changed Rust files have SHA-256:

- `src/runtime/offset_generator/client_runtime.rs`:
  `d164458a459ce43ad99c2cdf5491cf293af4254e2ef52aae12e47ba819f6d4b6`.
- `src/runtime/offset_generator/client_runtime/tests.rs`:
  `c185a8453a94972a4f12dffa536a769a904eacbc55e1efa5bda6b77459966d84`.

The native Windows staging archive is
`09978c173a44ab4c93f2767a4cffbfb391cd2155530fa9d7655dd9d9c0652551`;
the staged Rust-file hashes matched the host after the final regression-test
update. These source identities distinguish this generation from the earlier
temporary-version and deferred-version builds above.

- Release tooling passed maintained-document link validation (31 files),
  provenance/package fixtures, all four release-license tests, all 156
  live-parity tooling tests, and `validate-tag --tag v0.3.0`.
  Tool-suite success is not a live-parity capture.
- Linux passed `cargo fmt --all -- --check`, the four
  `runtime::offset_generator::client_runtime` regression tests, workspace
  Clippy across all targets/features with warnings denied, and
  `cargo test --workspace --all-features --locked`, including doctests.
  Windows MSVC cross-Clippy passed with `C_INCLUDE_PATH` unset.
- Native Windows passed workspace/all-target/all-feature Clippy, the explicit
  workspace lib/bin/test/example command, and workspace doctests, all with
  `--locked --target x86_64-pc-windows-msvc`. The no-default-feature production
  release build and native reader-benchmark compilation also passed.
  `native-verification-closure.json` and `native-windows-final-logs/` retain
  the commands, exit codes, and logs; compilation is not benchmark evidence.
- The final Linux packaged executable has SHA-256
  `a8b20a661d1fb0281d8ddd0fd397c2367d3845f1d08b9747392ffb56b81ce07a`.
  `VOSU_RELEASE_SMOKE_BINARY=<packaged-executable> cargo test --locked
  -p vosu-release-smoke --test release_smoke -- --ignored --nocapture`
  passed its actual packaged-production policy and shutdown scenario.
- The [released-tosu comparison](benchmarks.md#released-tosu-comparison-2026-10-09)
  records native Windows and Linux VM resource/API runs and reader fixtures
  separately. It excludes host-Linux attempts, preserves the unfavorable
  guest reader timing, and does not certify live schema parity.

Evidence is retained under `~/.cache/vosu-final-20261008-4r3azwf2/`.
At this benchmark generation, the version remained **0.3.0**; no dashboard
changes, publication, or push were performed.

## Local 1.0.0 release validation — 2026-10-09

The integrated code generation is
`80ad900112e6498b0365ec4992f087a735f8222c`; the subsequent verification-record
commit changes only this document.

- Linux passed `cargo fmt --all -- --check`, workspace/all-target/all-feature
  Clippy with warnings denied, and `cargo test --workspace --all-features
  --locked`: 1,153 tests passed and three were ignored in the workspace run.
- Windows MSVC cross-Clippy passed with `C_INCLUDE_PATH` unset, all workspace
  targets/features enabled, `--locked`, and warnings denied.
- All 156 live-parity tooling tests passed. The initial 300-second invocation
  timed out; the complete verbose run finished in 374 seconds. This is tooling
  evidence, not a new live-game parity capture.
- The Linux production build used `--release --locked --no-default-features
  --features linux-process-events --target x86_64-unknown-linux-gnu`.
  Packaging and extraction passed for `vosu-linux-v1.0.0.zip`; the external
  `vosu-release-smoke` test passed against its extracted executable.
- A private headless browser exercised that packaged executable: installed
  overlay search, an actual local-overlay iframe and enlarged preview, live
  catalog search and a loaded remote-image preview, and settings persistence
  across reload. The exact pre-fix JavaScript reproduced an incorrect success
  status while another field remained rejected. The packaged fix retained the
  error status after a valid save and cleared it only after correcting the
  rejected field.
- Provenance/package fixtures, all four release-packaging tests, and
  `validate-tag --tag v1.0.0` passed. Maintained-document links were checked
  separately after this record was added.

Evidence artifacts are under `~/.cache/vosu-1.0.0-7drbi0ua/`. The Linux archive
SHA-256 is `d7f31183fb4baec122f959ad3fb2aa095202c08553767d828fb9395dcf26a41b`;
the extracted executable SHA-256 is
`809b713d9cbd060d009eba2cd290bbc9451786dd6acbddabc2010788c38c3815`.
The temporary dashboard daemon and browser tab were stopped after verification.
No native Windows 1.0.0 execution, signed-resource validation, publication, push,
or licensing clearance is claimed.


## Upstream 2359305 update evidence

Local verification on 2026-09-29, against the working tree targeting upstream
`2359305a63f2aaebb201bc8d2eee58177f52590e`:

- `cargo test --locked -p vosu-pp --all-features` passed, including all 14
  current-oracle replay tests and 48 live-calculator contract tests.
- `cargo test --workspace --lib --bins --tests --examples --all-features --locked`
  passed, including `state_api_compat`, `server_compat`, `performance_invariants`,
  `process_memory`, `updater_native`, and `daemon_smoke`. The packaged-binary
  release gate and privileged process-event test remained ignored; this is not
  release-package or privileged-backend evidence.
- Linux workspace Clippy and Windows MSVC cross-target workspace Clippy passed
  with all targets/features and `-D warnings`. The cross-target command used
  `env -u C_INCLUDE_PATH cargo xwin clippy --workspace --all-targets --all-features
  --locked --target x86_64-pc-windows-msvc -- -D warnings`.
- Calculator `--verify-only` accepted 172 manifest items, 481 exact pinned NUnit
  parameter cases and their Rust replay coverage, the retained historical C#
  evidence, and the current generated fixture. This did not rerun NUnit.
- All 138 live-parity tooling tests passed with `PATH=/usr/bin:/bin`. With the
  ordinary rustup-first PATH, the protected-build-tool test correctly rejected
  the user-writable `~/.cargo/bin/rustc`; only the Python test process's PATH was
  changed. The daemon builds above used the pinned Rust toolchain.
- A separate isolated production-daemon smoke served the dashboard, returned
  the exact no-client readiness error, calculated PP for a local map, upgraded
  `/tokens` to WebSocket and sent a token-array command, then exited cleanly on
  SIGINT. This no-client smoke does not prove filtered live-state delivery.
  A throwaway native progressive-calculator program exercised gameplay
  current/max/FC PP and result/FC PP for osu! and mania and matched the target
  calculator's corresponding sample outputs; both throwaway programs were
  removed.
- Embedded asset checksums and maintained Markdown links passed.

Native Windows verification subsequently passed after explicit VM-testing
authorization, using a frozen working-tree archive and the non-elevated
desktop user: workspace Clippy with warnings denied, the full workspace test
command above, a release build, and the external `vosu-release-smoke` test
against that release executable. Source, build, and temporary files were kept
on `V:`. Evidence is retained under the host directory
`~/.cache/vosu-vm-2359305-2ad4xvrr/`, including `source-manifest.json` and
`native-logs/`. Host Btrfs error counters remained at the recorded baseline.

The live-parity source attestor now accepts Git for Windows' regular-file
representation of a tracked symlink only when `core.symlinks=false`, and still
requires byte-for-byte equality with the pinned target blob. Changed targets,
`core.symlinks=true`, and non-Windows emulation are rejected. The regression
test covers these boundaries; the real Windows reference checkout and build
passed with the corrected attestor.

The Windows verifier also checks the builder's exact command arrays and protected
MSVC PATH additions, including CMake/Ninja entries appended after the controlled
tool paths. It retains tool hashes, ownership checks, and injection rejection.
The comparator recognizes only the documented additive hit-error arrays and
`keys.all` paths; unknown keys and shared-field differences still fail. The menu
barrier now requires a nonempty client version on both sides in addition to
matching client/map context, avoiding upstream's initial HTTP-200 menu sentinel.
The readiness smoke rejected the captured initial payloads and accepted the
later initialized, shared-map pair without altering the capture.
The Windows client-population check also distinguishes a terminated, approved
PID/creation-time pair still present in the kernel snapshot from a new client.
It still rejects PID reuse, unrelated clients, and exit before both result
barriers; the final stable capture exercised the corrected shutdown path.

The first Windows lazer capture exposed cleared gameplay fields during pause.
After the reducer fix, native Windows workspace Clippy and tests passed again
(`native-corrected-logs/`). The fresh attested
`windows-lazer-corrected-capture.json` traversed menu → play → pause → result →
close; both paused APIs retained `Guest`, 55 misses, 0% accuracy, rank `D`, and
matching PP values. Comparing its 134,035 observations with the exact-extension
policy still **failed**: 102 startup-menu mismatches, 642 unmatched reference
observations, zero vosu contract failures, and zero unobservable path families.
Precise WebSocket cadence was approximately 85.5 Hz upstream versus 61.5 Hz in
vosu, with no stalls in this capture. The one-to-one coverage gate was not
relaxed. This capture predates the initialized-menu barrier fix and remains
failed evidence, not a parity pass.

The first initialized-menu Windows stable run (`windows-stable-ready-capture.json`)
reached menu, play, and pause in both daemons but expired before result and process
exit; it is incomplete, not passing evidence. The fixed numeric PE version is
`1.3.3.7` despite the executable's displayed version string being `1.3.3.8`.

The final Windows stable capture (`windows-stable-exit-fixed-capture.json`)
completed every lifecycle barrier and recorded exact no-client responses from
all four schemas after exit. Its 208,667 observations still **failed** comparison:
1,020 mismatches, 772 unmatched reference observations, zero vosu contract
failures, and zero unobservable path families. The mismatches comprise 683 map
maximum-combo differences (865 upstream versus 550 in vosu) across v1, v2, and
StreamCompanion, and 337 session-play-time differences (vosu reported zero).
Precise WebSocket cadence was approximately 87.6 Hz upstream versus 60.0 Hz in
vosu, with no stalls. The original 3.66 GB pretty-printed capture exhausted the
Windows JSON loader's memory; native comparison completed using a 1.73 GB
ASCII-escaped compact copy with the same metadata, observations, values, and
timestamps. Both copies and the failed report are retained; no comparison gate
was relaxed.

Windows was then shut down through the canonical command before the explicitly
authorized Linux native/headless checks. Task-owned Windows scheduled launchers
were removed. Host Btrfs counters remained at 92,602 historical corruption
errors and zero read, write, flush, or generation errors.

The Linux guest's frozen 868-file source archive
(`linux-source-manifest.json`, archive SHA-256
`c1788be205acbcf1695a3c070b4cc6df2364f0d088fc5f9f68ee818d12316743`)
passed Rust 1.96.1 formatting, all-target/all-feature workspace Clippy with
warnings denied, the full workspace test command, the production Linux release
build with `linux-process-events`, and the external release-executable smoke.
These commands ran as uid 1000, not root; logs are retained in
`linux-native-logs/` and `release-logs/` under the same host evidence directory.
The guest ran 138 Python parity-tool tests with one error: the controlled
reference-build environment test could not find `node`. Node remains a
reference-tool prerequisite, not a daemon runtime dependency; the host's full
138-test run passed.

Linux headless smoke then reached initialized menu telemetry for lazer
`2026.726.0`, served `/json`, `/json/sc`, `/json/v2`, and `/json/v2/precise`, and
exited on SIGINT with code 0 (`linux-headless-ready/`). The installed AppImage
first handed off to an existing pending Velopack update and exited, so the smoke
used a task-owned extracted copy with the documented gamescope headless settings.
Its SHA-256 matches the repository's retained `2026.726.0` AppImage provenance.
During the 2026-10-03 Linux verification, the isolated daemon initially could not
resolve that older client's offsets: `vosu.app` DNS resolution failed and
`osuck.net` returned HTTP 500. The successful
run used the exact version-matched, hash-verified offsets from commit
`9c2187e683775485b8551fd6e7cb09b6e6786332` in its private versioned cache; current
embedded `2026.921.0` offsets were not altered. The daemon inherited the client
as a descendant to satisfy Yama `ptrace_scope=1`, without root, capabilities,
`PR_SET_PTRACER`, or permission-policy changes. This proves the cached-offset
headless menu path, not remote offset availability or interactive parity.
The successful release executable, hashes, payloads, and logs were copied to the
host before removing the task-owned Linux staging tree. No task processes
remained; Linux was left running and Windows inactive.

### Session-clock and cadence remediation

The subsequent corrected 869-file native Windows source archive
(`remediation-7c0j_0ys/clock/source-archive.json`, SHA-256
`f0836fcbfc9758a9b051d5786cb9167cb80480c98ac49e95585f30d52ae630ed`)
passed workspace Clippy with warnings denied, the full workspace Rust test
command, production release build, and external executable smoke.
The retained executable SHA-256 is
`fd75e9b32b55ff3edaf21e0e0fc0ca0e05fa74337ce28dae66ffb68730a8b7a6`.
Logs are under `remediation-7c0j_0ys/clock/windows-native-logs/`.

Fresh stable capture `clock/windows-stable-short-capture.json` completed menu,
play, pause, result, and process exit with 134,071 observations. Both daemons
published map maximum combo 865 and advancing session clocks. Precise WebSocket
cadence measured 87.0 Hz upstream versus 90.0 Hz in vosu, with zero detected
stalls. Native replay with minimum-skew, maximum-coverage ordered matching
(`clock/stable-minimum-skew-report.json`) reports zero payload mismatches,
zero vosu-contract failures, and zero unobservable path families, but remains
**failed** because one HTTP v2 reference observation has no unique match.
There are 163 active reference HTTP v2 observations versus 162 vosu observations;
the vosu probe stream has a 264.5 ms gap around that unmatched observation.
This is not a parity pass, and no coverage gate or tolerance was relaxed.

The matching repair passes all 142 host parity-tool tests and a throwaway
500-case exhaustive small-stream comparison. It preserves maximum coverage
while avoiding the oldest-frame bias that produced 143 false session-clock
differences in the same immutable capture.

Linux VM interactive parity has not run: the documented installation provides
`/opt/osu/osu-headless`, but no authorized graphical observation/input route.
The Windows and Linux VMs share GPU/disk resources and must not run concurrently.

Interactive client parity remains unverified until an attested
menu → play → pause → result → close capture passes comparison.
Cross-compilation, synthetic memory fixtures, native smoke, and oracle replay
do not replace that gate.

### Resumed strict capture diagnostics

The fresh October 1 captures are retained under
`~/.cache/vosu-vm-2359305-2ad4xvrr/resume-20261001-ar_rp5xw/`.
Both completed all lifecycle barriers but **failed** strict comparison:

- `windows-stable-resume2-report.json`: 149,870 observations, three payload
  mismatches, 32 reference-coverage failures, and four vosu-contract failures.
  Two paused smooth-health mismatches reproduce the one-ULP difference between
  upstream `(health / 200) * 100` and vosu's former `health / 2`; regular and
  spectator v2 projections now preserve upstream operation order. The remaining
  session-clock discrepancy and synchronized WebSocket stalls are not resolved.
- `windows-lazer-resume2-report.json`: 103,789 observations, 12 payload
  mismatches, 1,140 reference-coverage failures, and three vosu-contract failures.
  The recorded play barrier was promoted during loading: both daemons still
  reported empty player names and a roughly 44.7-second preview clock.

Play readiness now requires an initialized player name from both daemons before
the existing first-object, aligned-clock, and two-second stability checks can
promote the barrier. A clean-process probe of the retained lazer observations
waited until both reported `Guest` and gameplay clocks of 5,128/5,139 ms.
The original capture, attested barrier times, comparison tolerances, coverage
requirements, and failed reports remain unchanged; this diagnostic is not a
replacement live-parity pass. All 143 host parity-tool tests pass, including
loading-state rejection and an isolated early-client-exit test.

The health-corrected native Windows archive
`health-rounding/source-archive.json` (SHA-256
`7d76c16f7298e88f2b6ceac9d024a35f2d16e2a14510d2aec7c1d63d108d7683`)
passed workspace Clippy, the complete workspace Rust test command, release build,
and external executable smoke. The release executable SHA-256 is
`dffa92073d1261e78c3b53566129b985a6855102eeaec72a3b070caab4ac70f2`;
receipts are in `health-rounding/windows-native-logs/`. Host formatting and
Clippy also passed. The full host Rust suite passed with `--test-threads=1`,
avoiding the earlier concurrent inotify exhaustion without changing host limits.
In the new native stable run, paused HTTP v2 output from both daemons contains
exactly `"healthBar":{"normal":0,"smooth":1.4869042945404435e-75}`,
103 misses, and map maximum combo 865. The completed
`health-rounding/windows-stable-health3-capture.json` contains 123,651
observations and all five lifecycle barriers. Strict comparison still **failed**:
four maximum-achievable-PP mismatches and six unmatched HTTP reference
observations, with zero vosu-contract failures and no active-window WebSocket
cadence stalls. Health and session-clock mismatches did not recur in this run.

The diagnostic `health-rounding/stable-gc-trace.json` establishes collector
pauses outside that active window: a generation-2 collection lasting 524.1113 ms
coincides with all eight WebSocket receive gaps of 532–646 ms. Earlier
collections grow from 27 ms to 424 ms as retained observations accumulate.
These are measured collector stalls, not evidence that either daemon stopped
producing frames; the raw capture and failed report remain unchanged.

An exact-map replay explains the paused PP difference without changing either
calculator or the comparison policy. The retained mania map checksum is
`bb4f3cd6159b359df64323b679821df5`; its long note ends at 31,754 ms.
The reference observes 102 misses at 31,870 ms, while vosu observes 103 at
31,875 ms. With stable Classic scoring and No Fail, the same Rust calculation
produces respectively `1.92009571` and `1.68008375` maximum-achievable PP.
Both implementations skip recalculation when no further object advances.
At the paused 32,043 ms clock, the replay therefore retains each prior value;
advancing to 32,450 ms with 103 misses makes both produce `1.68008375`.
The throwaway probe is retained as `health-rounding/pp-timing-probe.rs`, outside
the repository. This explains the failed observations; it does not turn that
capture into a parity pass.

The collector now disables cyclic garbage collection only during acquisition
(restoring the caller's setting afterward) and starts all eight HTTP probes in
synchronized rounds. Each response and connection-ownership check is retained;
no comparison tolerance, reference-coverage requirement, or failed artifact was
changed. The prior active HTTP streams contained 181/179 precise, 182/180
StreamCompanion, and 181/180 v2 source/vosu observations.

The corrected 895-file snapshot is retained as
`collector-portable-o2pkqn_i/source-archive.json`, SHA-256
`cfcb8a665a779963d4b390b15ff6afc740ba6b40e7cdc15457e8a7a4e10b647f`.
All 144 host parity-tool tests pass. Native Windows ran the same suite
successfully (144 tests, eight platform-dependent skips), workspace Clippy with
warnings denied, the complete workspace Rust test command, a production release
build, and external executable smoke. Its executable SHA-256 is
`eb7918bc30333b535b279610c609ec206ebe6def44ba6f129d07eab4536b65dc`;
receipts are under `collector-portable-o2pkqn_i/windows-native-logs/`.
Windows fixtures now use native executable suffixes, byte-exact Git inputs, and
isolated returned build roots. Connection-owner verification asserts the actual
owner rather than assuming the OS must wait for userspace `accept()`.
These checks do not by themselves establish live parity.

### Bounded collector generation

The final supervised October 1 generation is retained under
`collector-tcp-aualunu4/` inside the resume directory above. Its 895-file
readiness archive SHA-256 is
`b08c98e9f27e2f4fd06748de7048025316a342c981088964c86f89f33b1bdecb`.
Only `parity_capture.py` and its readiness tests changed after the native Rust
verification snapshot; `windows-native-logs/readiness-passed.json` records that
the native source is unchanged.

- Windows TCP ownership scans now decode directly from the ctypes buffer rather
  than copying the entire table for every row. A real 2,022-row, 48,540-byte
  table probe returned identical rows: five scans took 1.415–1.589 ms with
  copies versus 0.222–0.300 ms without them. This measures scan cost, not a
  live-parity improvement.
- Shared-menu readiness requires initialized selected-map audio duration and
  fresh cross-daemon observations, in addition to the existing identity and
  client-version checks. A retained-capture smoke rejected premature startup
  payloads and accepted the later initialized pair.
- All 146 host parity-tool tests passed. Native Windows passed the same suite
  with eight platform skips, workspace Clippy, the full Rust test command,
  release build, and external executable smoke. The release executable SHA-256
  is `2b58fed522ccb9c060b2abf895780f0e5351b6d89d108d84776e5eb06bce1b41`.
  The final Python checks ran as the limited interactive user; elevated SSH
  checks correctly rejected the writable Git executable. No tool-trust rule
  or persistent execution policy was changed.
- Real native listener and established-connection probes each returned exactly
  the creating process PID. Receipts and native command logs are retained with
  the generation.

The single stable capture and single lazer capture each completed all five
lifecycle barriers. Both captures exited zero; both comparisons exited two at
`capture has reused or unattested source HTTP samples`. Neither produced
payload-mismatch or reference-coverage counts: these are unavailable, not zero.
The raw captures, acquisition traces, rejection logs, and per-client
`windows-*-attestation-diagnostic.json` files remain retained.

| Client | Observations | HTTP connection failures | Payload mismatches | Reference-coverage failures |
| --- | ---: | ---: | --- | --- |
| stable | 235,860 | 3,718 | Not calculated | Not calculated |
| lazer | 152,678 | 4,904 | Not calculated | Not calculated |

All eight HTTP streams contain failed connections without connection-owner
attestation, exactly matching their deficits in ownership checks:

| Schema | Stable reference failures | Stable vosu failures | Lazer reference failures | Lazer vosu failures |
| --- | ---: | ---: | ---: | ---: |
| v1 | 470 | 474 | 627 | 604 |
| v2 | 457 | 463 | 590 | 612 |
| precise | 471 | 462 | 602 | 628 |
| StreamCompanion | 465 | 456 | 627 | 614 |

Every failure reports Windows socket error 10048. Classified by the interval
between promoted barriers, stable has 3,427 failures in menu, 273 in play, and
18 after pause promotion; lazer has 3,091 in menu and 1,813 after result
promotion. These intervals are not assertions about the exact client state at
each failed request.

The earliest failures occur around the 2,050th round of eight new HTTP
connections. Windows reports a 16,384-port dynamic TCP range starting at
49,152. Together with the collector's per-request connections, this is a
**high-confidence inference** of collector ephemeral-port exhaustion, not
evidence of daemon payload divergence. Ownership validation remains mandatory:
neither capture was promoted or compared with that gate bypassed. Cadence and
comparison policies were not relaxed, and no further GUI capture cycle was run.

Linux verification was not resumed at this boundary. Windows remained running;
Linux was not running (its unit retained a failed result). Authenticated
PowerShell and Windows MCP/QMP health passed, with a non-degraded Looking Glass
frame available. The recovered handback retains the explicit VM-lifecycle
authorization requirement but does not supply an explicit Windows shutdown
authorization. That controlled-shutdown authorization is the remaining gate
before any Linux transition; no concurrent VM start or service repair was
attempted.

### Bounded HTTP connection reuse generation

The subsequent transport-only generation is frozen under
`collector-http-reuse-z75gcsb1/` in the resume directory above; guest evidence
remains in `V:\vh5gcsb1`. The 896-file native verification archive has SHA-256
`cce00f4261367436fc81e4df96090465783330aa3393ae98938b1a6a8a8a046d`.
Only collector HTTP acquisition, its focused tests, and this documentation
changed from the preceding frozen generation. No daemon, comparison tolerance,
cadence policy, OS TCP setting, or trust requirement was changed.

The collector now reuses one HTTP connection per daemon/schema stream, with a
fresh process/listener/established-connection ownership observation for every
request. Failed requests are retained without retry; reconnects have the
256-attempt per-stream cap described above. The pre-change smoke observed eight
responses over eight accepted connections. Post-change verification passed:

- Focused host command:
  `python -m unittest discover -s tools/live-parity -p 'test_live_parity_http.py' -v`
  — four tests in 313.191 seconds.
- Full host command:
  `python -m unittest discover -s tools/live-parity -p 'test_*.py' -v`
  — 150 tests in 1,064.913 seconds, no skips.
- The same full suite under native Windows Python 3.13 and the limited
  interactive account — 150 discovered tests in 28.266 seconds, eight platform
  skips, successful result; the receipt confirms `elevated: false`.
- Every endurance execution measured 3,000 rounds across eight streams:
  24,000 responses, eight connection attempts, eight accepted connections, and
  24,000 fresh valid sample-to-ownership bindings. Regression cases also cover
  announced closure, bounded reconnects, changed process identity, truncated
  responses without replay, and missing/reused ownership observations.

The single fresh stable capture completed menu → play → paused → result →
process-exit, exited zero, and cleared artifact/process/socket attestation.
Its unchanged strict comparison exited one with actual payload differences:

| Metric | Measured result |
| --- | ---: |
| Total observations, HTTP and WebSocket | 254,546 |
| HTTP rounds across all eight streams | 17,886 |
| HTTP responses and fresh valid ownership bindings | 143,088 each |
| HTTP connection attempts / unique attested client ports | 8 / 8 |
| HTTP transport failures | 0 |
| Payload mismatches | 4 |
| Reference-coverage failures | 0 |
| vosu-contract failures | 0 |
| Missing barriers / missing streams | 0 / 0 |

Each HTTP stream has exactly 17,886 requests, responses, and fresh connection
attestations, using one connection attempt and one client port. All eight
reported active-window WebSocket streams have zero stalls. The precise streams
measured approximately 87.08 Hz upstream and 97.16 Hz vosu; these are
observations from this run, not a promised rate.

The four mismatches are the two equivalent maximum-achievable PP projections,
each over HTTP and WebSocket, during pause:

| Schema | JSON pointer | Upstream | vosu | Mismatch rows |
| --- | --- | ---: | ---: | ---: |
| v1 | `/gameplay/pp/maxThisPlay` | 11.76 | 11.52 | 2 |
| v2 | `/play/pp/maxAchievable` | 11.76 | 11.52 | 2 |

The retained sample pairs report 62 misses; v2 explicitly reports paused play.
The direct paused observation also has identical beatmap time 22,812 ms,
maximum combo 865, and smooth health. The PP divergence is measured, but its
calculator/state cause was not investigated in this transport-only change.
Connection reuse removed socket churn and attestation rejection in this run;
it does not by itself establish every cause of the preceding socket failures
or establish full live parity.

Per the explicit stop condition, no lazer capture or second stable capture was
started, and no failing gate was retuned. `windows-stable-summary.json` contains
all per-stream counters; `windows-stable-mismatch-context.json` retains the
exact mismatching sample pairs. The full capture, strict report, acquisition
log, GC trace, source archive/manifests, native and host verification receipts,
and GUI observations are preserved alongside `generation-receipt.json`.

At the boundary, the client and generation-owned processes had exited. The
native and stable scheduled launchers remained `Ready`, both `Limited`; no
lazer launcher or capture existed. Windows remained running; Linux was not
touched. No VM lifecycle operation, subagent, commit, or push was performed.

### Local feature-branch integration

The authorized local integration reviewed 134 modified tracked files and three
new source/test files, grouped into native feature/provenance changes and
live-parity tooling/evidence changes. No cache directories, temporary build
outputs, or raw capture artifacts are included in those commits.

The integration checkout matches all 896 entries in the frozen HTTP-reuse
source manifest except this verification document. The existing native Windows
Clippy, workspace tests, release build, packaged-executable smoke, and limited
Python-suite evidence therefore remain applicable to the unchanged code; they
were not rerun. No VM actions or new captures were performed.

Local integration checks passed:

- `cargo fmt --all -- --check`.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
- `cargo test --locked --all-features -p vosu -p vosu-pp --lib --test state_api_compat --test server_compat --test performance_invariants --test process_memory --test oracle_parity --test live_contracts`
  — 818 top-level tests passed; the one ignored proxy helper also passed in its
  isolated child invocation.
- `python -m unittest discover -s tools/live-parity -p test_live_parity_http.py -v`
  — four tests passed in 284.696 seconds, including 3,000 rounds, 24,000 responses,
  eight connections, and 24,000 fresh valid ownership bindings.
- `python tools/live-parity/run.py config-schema` emitted valid JSON.
- `sha256sum --check --strict assets/SHA256SUMS` validated all 18 entries.

The earlier host limitations remain recorded above: protected-build tests need
a system-tool PATH rather than the user-writable Rust shim path, and the retained
full Rust suite used serial tests after concurrent inotify exhaustion. Neither
host policy nor those tests were weakened for integration.

This is local source integration, not a live-parity pass. The four paused PP
mismatches above remain unresolved, their semantic cause is not established,
and no comparison, transport, ownership, artifact, or cadence gate changed.

### 1.0 release gate: strict Windows stable (2026-10-04)

The fresh `windows-stable-r10` capture passed the unchanged strict comparator:
305,857 observations, zero payload mismatches, zero reference-coverage failures,
zero vosu-contract failures, no missing streams or lifecycle barriers, and no
unobservable reference path families. All eight active HTTP streams and all
eight active WebSocket streams recorded zero stalls. Capture and comparison
both exited 0 under the limited interactive `agent-desktop` token.

This evidence applies to the frozen 905-path working-tree source archive
`dfb9b4ddf52304a78e1019ee533927f7fbcc45dbe93c4a7512c4fc4bd0cc661d`,
whose base HEAD was `44e463078036da046c73fed039a23a3c76553f95`; the archive
includes uncommitted changes and is not evidence for that bare commit alone.
The freshly built vosu executable has SHA-256
`6ec7c9248317b107aec84e88f3b50db217af7c6ada2dda3607b8e259dfbd841e`.
Reference tosu 4.26.2 was built from pinned revision
`2359305a63f2aaebb201bc8d2eee58177f52590e`; the exact stable client reports
version 1.3.3.7 and has SHA-256
`3adc2d001aacced5f92ba8ae24121980d5e23f5653f37d14cf4765bca1abd9f9`.

The isolated offline profile retained `Guest` without a password or automatic
login. The original Reaction [Advanced] mania chart was played with No Fail
only. Both daemons attested menu, initialized active play, pause, natural
result, and exact client termination; fresh UI observations retain the pause,
550-miss result, and cooperative exit. Readiness, comparison tolerances,
transport, ownership, coverage, and cadence gates were not relaxed.

Retained artifacts are under
`~/.cache/vosu-release-gates-fuobodv5/`:

| Artifact | SHA-256 |
| --- | --- |
| `windows-stable-r10-capture.json` | `e907be1e81cf0687a6fef06292e4527dae677e2e096a46b6f3be98a43139e30b` |
| `windows-stable-r10-report.json` | `002633be794b882dcbc9855c7b9406a6c072deabcd52f6ce724640357ee9b625` |
| `windows-stable-r10-result.json` | `d2a7c2b885f73f13f075ae787c769b69f6fd3dee575b8e870fe53ad65fe16725` |

Earlier failing captures above remain historical evidence. This is one clean
strict stable pass, not a claim that every possible sampling boundary has been
exhaustively exercised, and it does not establish Windows lazer or Linux parity.

### 1.0 native Windows build verification (r6 retry1)

The successful native Windows build retry used the frozen 905-path source
archive at HEAD `44e463078036da046c73fed039a23a3c76553f95`, archive SHA-256
`803b3d0c3e357fb5b2bed4bcf0b0c63640293967235b4951dbcc05cb4c04d792`.
The archive includes local working-tree changes; HEAD alone does not identify
the verified source bytes.
The verifier receipt records non-elevated execution as
`win-kpupic6d4n8\agent-desktop` and pinned Rust
`1.96.1 (31fca3adb 2026-06-26)`, host `x86_64-pc-windows-msvc`.

All six receipt commands exited zero:

- Python live-parity tool suite:
  `python -m unittest discover -s tools/live-parity -p 'test_*.py'` —
  154 tests ran, 8 skipped, all remaining tests passed.
- `cargo fmt --all -- --check`.
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
- `cargo test --workspace --all-features --locked --no-fail-fast -- --test-threads=1`.
  Doctests ran; per-binary totals are recorded in the raw workspace log rather
  than consolidated here.
- `cargo build --release --no-default-features --locked --target x86_64-pc-windows-msvc`.
- External release smoke:
  `cargo test -p vosu-release-smoke --test release_smoke --locked -- --ignored --nocapture`,
  with `VOSU_RELEASE_SMOKE_BINARY` set to the retained executable below.

The retained release executable SHA-256 is
`70ec257bf0a84674df72ad29863de0dd7fcc09e82064b69eab7dd729e30ebdbd`.
Receipt and logs are in
`~/.cache/vosu-release-gates-fuobodv5/windows-native-r6/logs-retry1/`;
archive and manifest are in the parent `windows-native-r6/` directory. The
first r6 attempt's Python/Git-PATH receipt failed and remains failed historical
evidence. On retry, only the cache checkout's Git PATH was restored after MSVC
environment loading so the parity suite could find Git; source contents and
strict checks were unchanged.

This is native formatting, lint, test, production-build, and external-smoke
evidence only. It is not live-parity or signed-release evidence. The separate
historical menu-only and r7/r8 Windows lazer captures remain failed evidence;
the fresh r9 result below supersedes them. Native Linux live-parity evidence is recorded below.

### 1.0 release gate: strict Windows lazer (2026-10-04)

The fresh `windows-lazer-r9` capture passed the unchanged strict comparator:
397,650 observations, zero payload mismatches, zero reference-coverage failures,
zero vosu-contract failures, no missing streams or lifecycle barriers, and no
unobservable reference path families. All active HTTP and WebSocket streams
recorded zero stalls. Capture and comparison both exited 0 under the limited
interactive `agent-desktop` token.

The capture used the frozen r6 905-path source archive identified above, not
bare HEAD alone. Its freshly built vosu executable SHA-256 is
`19401aee4a82ff61fafb1dfdd406db2abe83fbf03a4ca4ed60cbd187cad9610b`.
Reference tosu 4.26.2 was built from pinned revision
`2359305a63f2aaebb201bc8d2eee58177f52590e`; the Windows lazer 2026.921.0
client SHA-256 is
`2ea71f129348e93ef04f065f3eacaef8941ab64b8b82fcbce3703e8ee7adc957`.
A 905-path hash comparison against the current working tree after this capture
found only the maintained verification document changed from the r6 manifest.

Both daemons attested initialized menu and active play before pause. Fresh
observations confirm Reaction [Advanced] mania with No Fail, actual pause and
resume, the natural 594-miss result, and cooperative client exit. The capture
also attested exact process termination and all four not-ready schemas from
both daemons. No readiness, tolerance, ownership, coverage, transport, or
cadence gate was relaxed. The isolated offline profile did not log in.

Retained artifacts are under `~/.cache/vosu-release-gates-fuobodv5/`:

| Artifact | SHA-256 |
| --- | --- |
| `windows-lazer-r9-capture.json` | `f74fb4cadd24e6fd34b5d1e655789e557ffd63be97b6db8cc994379a8a29603a` |
| `windows-lazer-r9-report.json` | `fdd2a936364086e5898c7fd6f4ed245f7eb556b57c533c98be90e48229d2e7ae` |
| `windows-lazer-r9-result.json` | `71baa3e90ad1a9069ae60612909ab2678ee72f82eb1e2fe4200e330527da20a0` |

Earlier failed captures remain preserved. This is one strict Windows lazer
pass, not evidence for Linux or every possible sampling boundary.

### Linux native lazer preparation (2026-10-04)

The compressed-reference fix passed the complete host parity-tool suite:
`PATH=/usr/bin:/bin /usr/bin/python3 -m unittest discover -s tools/live-parity -p 'test_*.py' -v`
— 155 tests in 960.896 seconds, successful result. An earlier invocation was
stopped by its 300-second command deadline and is not a successful suite run.
The local maintained-document link check passed with zero missing paths.

The frozen Linux source generation is retained at
`~/.cache/vosu-release-gates-fuobodv5/linux-preparation-r7/package-r7/`.
Its archive SHA-256 is
`f9faec9140baa9dd139f6b3a5203de3f04ee5b6365bb30e5a622f498f457bb88`.
All 905 source paths were verified; only `parity_artifacts.py` and its
`test_live_parity_config_cli.py` regression differ from the Windows native r6
archive. Daemon/calculator bytes, the reference ELF, client, chart, and
generator bundle are unchanged. Guest staging verified the manifest and
generator inputs, and the actual `check-config` command accepted the published
reference and the isolated Linux lazer 2026.921.0 client.

The first r7 capture stopped before build because the guest's system Rust
commands were rustup proxies. The next r8 attempt used a task-owned protected
copy of the installed Rust 1.96.1 toolchain under `/opt/vosu-release-r8/rust`;
compiler/Cargo hashes match the originals. Its native release build, including
`linux-process-events`, passed. Both daemons and the client ran unprivileged;
Yama remained at `ptrace_scope=1`. Capture then failed the unchanged client
ownership gate when an additional osu-named PID appeared during startup.
Receipts/logs remain preserved. This is preflight/build evidence, **not a
Linux live-parity pass**; client process-transition diagnosis is still pending.


### Linux native capture readiness diagnosis (2026-10-04)

The later r12/r13 captures accepted client ownership and initialized menu, but
still exited 2 without the required play, pause, result, and exit barriers.
They remain failed evidence, not Linux parity passes.

A retained real r13 active HTTP pair reported `Guest` in both daemons, the same
map checksum, and live clocks of 55,032/55,028 ms. Replaying the unchanged
readiness predicate over the capture's 4,934 HTTP v2 samples found 418 ready
observations but no two-second stable interval; the longest interval was
1,365.379603 ms. Each HTTP stream recorded 2,467 observations over the capture.
Recorded ownership-bracket medians were 292.979998 ms for the reference and
283.9979815 ms for vosu; these are whole assertion/request brackets, not isolated
CPU profiles.

A native socket probe reproduced a CLI-composition defect: after an ownership
lookup populated 244 inode entries, an unrelated listener query reset the
cache timestamp to zero and discarded every entry. Cache state now resides
in one shared object rather than rebound module globals. The same real native
CLI probe retained all 244 entries and the original timestamp after the
listener query, while still resolving its socket to the exact owning PID.
The five-millisecond lifetime, process-start checks, exact accepted-socket
tuple checks, and all strict readiness/comparison thresholds are unchanged.
This proves the cache-state fix, not a successful replacement live capture.

Failure receipts, bounded readiness/attestation analyses, actual active
snapshots, and screenshots are retained under
`~/.cache/vosu-release-gates-fuobodv5/linux-runner-r7/`; the full r13 capture
remains under `/home/vosu/vosu-linux-preparation-r7/` in the guest.

The post-fix complete host tool suite,
`/usr/bin/python3 -m unittest discover -s tools/live-parity -p 'test_*.py' -v`,
passed all 155 tests in 303.278 seconds.

The r14 replacement attested initialized menu, active play, pause, and natural
result from both daemons, but exited 2 because the attempted GUI close did not
terminate the client before the deadline. It remains failed evidence. A separate
r15 menu-only diagnostic verified cooperative `xdotool windowquit` followed by
the application's exit confirmation; fresh window discovery then returned no
windows. That deliberate early exit is also not a live-parity pass. Both
receipts/logs remain retained in `linux-runner-r7/`.

### Linux r16 capture and strict comparison (2026-10-04)

The unprivileged r16 run attested menu, initialized active play, shared pause,
natural result, and exact client termination from both daemons. Capture exited
0. Its original comparison exited 2 because the verifier omitted the required
`--features linux-process-events` from its expected Linux build command.
The verifier now requires that exact production command; Windows requirements
and all capture/comparison gates remain unchanged. The complete host parity
tool suite passed 155 tests in 258.747 seconds.

Recomparison of the untouched capture with the corrected verifier exited 1:
145,580 observations, 41,436 payload mismatches, zero reference-coverage failures,
zero vosu-contract failures, no missing streams/barriers, and zero stalls in all
eight active HTTP and eight active WebSocket streams. Every mismatch is in
strain graph data, axes, lengths, or StreamCompanion graph keys. This is **failed
Linux parity evidence**, not a pass.

The published reference is not the same source generation used for Windows:
the [v4.26.2 annotated tag](https://api.github.com/repos/tosuapp/tosu/git/tags/03202868c73c38938b65cce3f39b3c50e49b0fe4)
targets `b21b4526d8db9ce124f5a8bf475ef5421837f5b7`, whereas the workspace targets
`2359305a63f2aaebb201bc8d2eee58177f52590e`. The
[upstream comparison](https://github.com/tosuapp/tosu/compare/b21b4526d8db9ce124f5a8bf475ef5421837f5b7...2359305a63f2aaebb201bc8d2eee58177f52590e)
changes `BeatmapPP.updateGraph` from strain-section-count-based trailing axes
and `-100` padding to last-visible-object axes and separate `-50` object-gap
padding. The retained mismatch `116164` versus `116451`, and `-100` versus
`-50`, follows those two algorithms. No production graph code was changed to
match the older reference. An exact pinned-source reference is being prepared
for a replacement native Linux client capture.

Retained r16 evidence:

| Artifact | SHA-256 |
| --- | --- |
| Guest `linux-lazer-r16-capture.json` | `e965d4d23f1578102beef241ad8130bc728054e3b71fcbecdfc9dd4e252da9b7` |
| `linux-lazer-r16-r17-comparator-report.json` | `c4d991bf77697e77ad41deed31b6e2202871699b37ea83060c41a2cec3b8d4ab` |
| Corrected `parity_verify.py` | `e770c90a572227c8e082828d30bbecd23adf559363d61be46539d77ec3c60a5a` |

The full capture remains in `/home/vosu/vosu-linux-preparation-r7/` in the
guest. The report, original supervisor receipt/log, and separate corrected
comparison receipt are retained under
`~/.cache/vosu-release-gates-fuobodv5/linux-runner-r7/`. Original failures were
not overwritten or relabelled.

### Linux pinned-reference build readiness (2026-10-04)

The r17 attempt accepted the clean upstream `2359305` checkout, protected
Node 24.14.0, and the unchanged native Linux lazer identity. It exited 2 before
launching any client or daemon: constructing PATH in tool-name order put the
system Git directory before the protected Rust directory and selected the
system Cargo proxy instead of the attested Cargo executable.

On POSIX, the builder now retains the caller's relative order for only the
attested tool directories, then checks every selected executable as before.
It does not admit other inherited PATH entries. Windows ordering is unchanged.
The real guest CLI-composition probe selected Git and Python from `/usr/bin`,
Node/pnpm from the task-owned protected r17 installation, and Cargo/rustc from
`/opt/vosu-release-r8/rust/bin`. The affected build-contract suite passed all
18 tests, including the extended shadowing/injected-directory regression.

Reference-only prerequisites are the official Node 24.14.0 archive
(SHA-256 `41cd79bb7877c81605a9e68ec4c91547774f46a40c67a17e34d7179ef11729df`)
and pnpm 10.10.0 with the npm registry's SHA-512 integrity verification.
Their protected files are root-owned; the builds and runtime remain
unprivileged. These are upstream reference build tools, not vosu daemon runtime
requirements. The failed r17 receipt/log and the native selection probe remain
retained with the other Linux evidence.

### Linux pinned-reference native build and menu-only retry (2026-10-04)

The r18 attempt passed configuration validation but failed before building the
reference because the Node-build branch unconditionally indexed the Windows-only
`cmd` toolchain entry. It now passes the optional entry to the existing platform
helper, exactly as the vosu build branch does. Linux returns the unchanged
environment; Windows still rejects a missing protected `cmd.exe`.

The r19 run then built the pinned upstream `2359305` Node reference and the
native production vosu executable, launched the exact Linux lazer client, and
attested initialized menu from both daemons. It did not reach active play before
the unchanged capture deadline and exited 2. No comparison ran. This is native
reference-build smoke evidence, **not a Linux parity pass**. The complete host
parity-tool suite passed all 155 tests in 168.694 seconds after the platform fix.

The frozen r19 source contains 905 verified paths at base HEAD
`44e463078036da046c73fed039a23a3c76553f95`; archive SHA-256 is
`cb0cb4f9742547224dce6942fd532cbc6674d95a3b5d0ca04338ad4987dd9016`.
Its declared changes from the Windows generation are parity tooling and
documentation only; daemon/calculator source is unchanged. Original r18/r19
receipts and logs remain under
`~/.cache/vosu-release-gates-fuobodv5/linux-runner-r7/`; the r19 capture remains
in `/home/vosu/vosu-linux-preparation-r7/` in the guest.

Native client logging showed the menu returning to idle without entering Solo.
The [upstream menu implementation](https://github.com/ppy/osu/blob/master/osu.Game/Screens/Menu/ButtonSystem.cs)
binds Solo to `P`, not `S`. A separate r20 capture uses the same frozen r19 source
and pinned artifacts with the corrected keyboard navigation. Readiness,
ownership, coverage, cadence, comparison, and timeout requirements are unchanged.

### Linux pinned-reference r20 capture (2026-10-04)

The unprivileged r20 run used the same frozen r19 source and built the reference
from upstream `2359305`. Both daemons attested initialized menu, active play,
shared pause, natural result, and exact client termination. Fresh native client
frames show Reaction [Advanced] mania with No Fail only, pause/resume, the
594-miss result, and cooperative close. Capture exited 0; comparison exited 1.

The unchanged strict comparator reported 204,511 observations and four payload
mismatches, all in v1 `gameplay.pp.maxThisPlay` or v2 `play.pp.maxAchievable`:
`3.97` upstream versus `4.19` vosu at the pause boundary. There were zero
reference-coverage failures, vosu-contract failures, missing streams/barriers,
unobservable reference path families, or active HTTP/WebSocket stalls.
The strain-graph mismatches seen against the older published reference are
absent from this pinned-source comparison.

The retained HTTP context shows both daemons reporting 100 misses and `4.19`
before the transition to 101 misses. Upstream then recalculated `3.97`, while
vosu retained `4.19` when play paused. This is a sampling/progression-boundary
diagnostic, not evidence that the boundary is fixed or a Linux parity pass.

| Artifact | SHA-256 |
| --- | --- |
| Guest `linux-lazer-r20-capture.json` | `da93ca983a6a32bc943764c7e3cd2b3ebcf0eb392d5a10893e693d74f685729a` |
| `linux-lazer-r20-report.json` | `9156eb642cf25965bf1feed5b686301eb61ed82ab87d11c714061fa9d2e273d9` |
| `linux-lazer-r20-result.json` | `025068cb5f55e8cb2b672ce681802b432cf5e42a24986b4d68a59b70816a6100` |

The full capture remains in `/home/vosu/vosu-linux-preparation-r7/` in the
guest. Report, supervisor receipt/log, paused sample context, and UI evidence
are retained under `~/.cache/vosu-release-gates-fuobodv5/linux-runner-r7/`.
A separate r21 capture retains the same frozen source, pinned artifacts, and
all strict gates. The failed r20 evidence is not overwritten or relabelled.

### 1.0 release gate: strict native Linux lazer (2026-10-04)

The fresh `linux-lazer-r21` capture passed the unchanged strict comparator:
167,829 observations, zero payload mismatches, zero reference-coverage failures,
zero vosu-contract failures, no missing streams or lifecycle barriers, and no
unobservable reference path families. All eight active HTTP streams and all
eight active WebSocket streams recorded zero stalls. Configuration validation,
capture, and comparison each exited 0 under the unprivileged guest UID 1000.

The run used the frozen r19 905-path source archive
`cb0cb4f9742547224dce6942fd532cbc6674d95a3b5d0ca04338ad4987dd9016`,
based on local integration HEAD `44e463078036da046c73fed039a23a3c76553f95`.
The archive includes uncommitted changes; bare HEAD does not identify the
verified source. Its declared changes from the Windows generation are parity
tooling and documentation only; daemon/calculator source is unchanged.
The freshly built native vosu executable SHA-256 is
`26532d589f853dd2545f4fc631c8e3fe182ee1402fa259f82eba5e168bc15905`.
The attested release command includes `--features linux-process-events`.

Reference tosu 4.26.2 was built natively in Node-build mode from pinned revision
`2359305a63f2aaebb201bc8d2eee58177f52590e`. Its built `index.js` SHA-256 is
`8010decd679af49deb49cfb0ffc86329bd1faac0761d01e42e375d7187fb9c89`;
the capture also attests its complete distribution manifest and protected Node
toolchain. The Linux lazer 2026.921.0 client SHA-256 is
`093bc36983efeef17531b45046195f2a3cb37a6b69865763a799fbdd93239679`.

Both daemons attested initialized menu, active play, shared pause, natural
result, and exact client termination. Fresh native frames retain Reaction
[Advanced] mania with No Fail only, actual pause/resume, and the natural
594-miss result. Cooperative close returned an empty window list and both
exact process-exit attestations. The isolated Guest profile did not log in.
No readiness, tolerance, ownership, coverage, transport, or cadence gate changed.

| Artifact | SHA-256 |
| --- | --- |
| Guest `linux-lazer-r21-capture.json` | `b598451de1127ed9038db2dde1d1fbbcef87add2427c1f4fb774f5d397d49ef3` |
| `linux-lazer-r21-report.json` | `9134c33b452966010ab817d93adb2f174a21ec3c2a14b4b05fa04cd74dd9b1cf` |
| `linux-lazer-r21-result.json` | `698621741f535081f008116be6df736958979cb76d83329b2aa00f91c4459ca3` |

The full capture remains in `/home/vosu/vosu-linux-preparation-r7/` in the
guest. Report, supervisor receipt/log, configuration, and UI evidence are
retained under `~/.cache/vosu-release-gates-fuobodv5/linux-runner-r7/`.
The earlier r20 four-row paused PP failure remains failed historical evidence.
This is one clean strict native Linux lazer pass, not a fix or exhaustive
verification of every sampling boundary. Linux stable through Wine remains
experimental and not release-verified. No release or tag was created.


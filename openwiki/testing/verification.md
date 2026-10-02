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
- Calculation/state: `vosu-pp` tests, `performance_invariants`, and live parity when observable schemas or timing can change; for v2 stats, cover parsed/missing maps, modes, mods, clock rate, and catch timing-window semantics. The oracle-backed `live_contracts` and `oracle_parity` tests require the crate’s `oracle-debug` feature.
- Compatibility payloads: assert the original `hitErrors`/`hitErrorArray` arrays plus additive sibling `pressHitErrors` and `releaseHitErrors` arrays across v1, stream companion, v2, precise, play, and tournament fixtures; include the lazer mania hold release-correction case.
- Hot paths: the affected Criterion group in `benches/hot_paths.rs`, compared on the same machine; record observations in the [benchmark guide](benchmarks.md), not as universal thresholds.

## CI baseline

The required checks in `AGENTS.md` and `.github/workflows/ci.yml` are:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --lib --bins --tests --examples --all-features --locked
cargo test --locked --test process_memory -- --nocapture
cargo test --locked --test updater_native -- --nocapture
```

CI always publishes `CI required`, including documentation-only PRs. The cheap classification job checks local Markdown links without network or inference. Code changes run Linux Clippy/tests and a Windows MSVC cross-check; shared/platform/build/workflow changes also run native Windows verification. Pure schema/calculator changes retain cross-checks without automatically running native Windows. Manual dispatch runs both lanes. Branch protection should require `CI required` rather than conditionally skipped platform jobs. Cross-compilation is not native execution evidence.

MSVC SDK caching is independent of Cargo.lock and restricted to x86_64. Prepared SDKs are saved before later lint failures. Rust artifact caches use separate target/lane keys and weekly generations to limit archive churn; downloads are cached separately. Tagged releases package, extract, and exercise the exact Linux/Windows archives, using a small smoke-test package that does not depend on the daemon crate.

## Evidence workflows

`tools/live-parity/` is an interactive attested compatibility gate, not a normal unit test. Validate configuration, capture a controlled client lifecycle, compare, and generate diffs. It requires pinned artifacts, distinct loopback ports, and supported platform/client combinations.

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

When a change crosses [runtime boundaries](../architecture/overview.md), run the narrowest affected contract first and then the platform/live gate appropriate to the observable behavior. Do not substitute a compile-only check for behavior verification.

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
The isolated daemon initially could not resolve that older client's offsets:
`vosu.app` DNS resolution failed and `osuck.net` returned HTTP 500. The successful
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

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
- State bus, grants, projection or demand: core/plugin unit tests plus the M2 suites and schema validation below; `state_api_compat` replays the source oracle through the bus-backed adapters, so a projection change must keep its bytes.
- Configuration/counters: affected unit tests plus `cargo test --locked --test config_compat`; cover flat typed TOML parsing, precedence `vosu.toml > vosu.env > tosu.env > defaults`, canonical replacement before legacy removal, malformed startup/reload preservation of last-good state, watcher handling, and atomic writes.
- Process/memory: affected tests plus `cargo test --locked --test process_memory -- --nocapture` on every affected native OS.
- Updater/release: `updater_compat`, native `updater_native`, and `VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture` for package/startup changes.
- Calculation/state: `vosu-pp` tests, `performance_invariants`, `state_bus_invariants`, and `state_api_compat`/`server_compat` when observable schemas or timing can change; for v2 stats, cover parsed/missing maps, modes, mods, clock rate, and catch timing-window semantics. The oracle-backed `live_contracts` and `oracle_parity` tests require the crate’s `oracle-debug` feature. Live parity is optional ([ADR 0011](../decisions/0011-live-parity-optional.md)).
- Compatibility payloads: assert the original `hitErrors`/`hitErrorArray` arrays across v1, stream companion, v2, precise, play, and tournament fixtures, and the absence of every ADR 0021 extension member (`pressHitErrors`, `releaseHitErrors`, `keyHitErrors`, `hitOffsets`, `keys.all`, `catcher`) at every nesting level while the bus holds nonempty source facts; include the lazer mania hold release-correction case.
- Untrusted-input and crash-recovery logic (ZIP preflight, updater recovery, API-state selection, served paths, JSON Pointers): `cargo test --workspace --lib --all-features --locked prop_`. These property tests use a fixed seed; `VOSU_PROPTEST_RANDOM=1 PROPTEST_CASES=<n>` explores new cases, and the nightly workflow runs 20,000 cases per property. A failure persists its seed to `proptest-regressions/lib-prop-failures.txt`; commit that file with the fix.
- Lazer offsets: `python scripts/lazer-offset-canary.py [--tag <release>-lazer]` stages the generator on the client's own runtime exactly as the daemon does and fails naming any template field it cannot resolve. The nightly workflow runs it against the newest lazer release.
- Dashboard and tool host: `tests/tool_host_smoke.py` and `tests/dashboard_smoke.py` against a `--features test-harness` debug build; CI's `dashboard` job runs them when UI paths change, and the nightly workflow also runs the catalog-art and tool-catalog smokes.
- Hot paths: the affected Criterion group in `benches/hot_paths.rs`, compared on the same machine; record observations in the [benchmark guide](benchmarks.md), not as universal thresholds.
- Native Windows flakes: `python scripts/vm-stress.py --package vosu --lib --iterations 20 --keep-remote -- <filter> --nocapture` builds once and reruns the JSON-reported test executable directly. This avoids build-script invalidations between samples. Each executable runs from its package directory with the normal harness parallelism; `--jobs` only caps compilation. Inspect `summary.json` and `logs/iter-*.{stdout,stderr}.log` in the reported VM directory for timings and failures.

For socket backpressure tests, constrain the peer buffers and observe a blocked
write before resuming reads. A fixed megabyte-scale flood tests machine-specific
loopback throughput rather than the flush/shutdown ordering contract.

## M2 state-bus contracts

```sh
# Real host vtables, testkit plugins and bundled surface reads (K8)
cargo nextest run --locked --features test-harness --test state_bus
# Oracle parity and JSON Schema validation (X1); existing 1.x wire contract
cargo nextest run --locked --test state_bus_compat --test state_api_compat
# Real osu! projection: private paths and explicit subtree grants
cargo nextest run --locked --features test-harness --test osu_projection_privacy
# Owning state-bus/schema and osu! projection/demand unit tests
cargo nextest run --locked -p vosu-core --lib state::
cargo nextest run --locked -p vosu-osu --lib
```

`state_bus` exercises concurrent regular/precise batch atomicity through ABI
and surface readers, all-or-nothing rejection, internal-data exclusion from
public documents/events/tracing, explicit `requires`/`optional` subtree
grants, delayed identification and PID-generation replacement.
`osu_projection_privacy` uses the actual osu! projector and path sentinels,
not just a synthetic namespace, to guard the same API privacy boundary.

`state_bus_compat` walks every case in
`tests/fixtures/oracle/inventory.json`: sourced cases pass through encoded
`PUBLISH_BATCH`, decode, commit and audience projection; not-ready cases
assert that no session state exists. Its dev-only `jsonschema = 0.61.2`
dependency has default features disabled; validators load
`docs/v2/schemas/*.schema.json` and resolve `common-defs.schema.json` from
an in-memory registry, without network retrieval. Both public and
explicitly internal-granted documents validate for all six namespaces.
This is full JSON Schema validation in tests, not the production lane walker.
The test compares the [mapped values](../v2/state-schema.md#1x-to-20-field-mapping)
against the existing v2/precise builders, checks unchanged publication and
diff convergence, and rejects unlisted exemptions. Exact permitted differences
and the deferred stable tournament-manager aggregate are recorded in
[`DIVERGENCES.md`](../../tests/fixtures/upstream/DIVERGENCES.md);
they are not blanket exclusions for whole fixtures.

For M2 performance contracts use
`cargo nextest run --locked --features test-harness --test performance_invariants --test state_bus_invariants`
and the scoped commands in the [benchmark guide](benchmarks.md). Performance
evidence and D17's decision belong there, not in a functional test pass.

## M4a contract and adapter acceptance

A4 resolves the approved choices in the
[source/removal inventory](../../tests/fixtures/upstream/ADAPTER_CONTRACT.md)
and [P0/R0 surface contract](../v2/adapter-surfaces.md). Since the production
cutover, `state_api_compat` drives the actual producer → ABI/bus → authorized
surface → adapter registration and compares raw emitted bytes, preserving the
source pin and fixture inventory; no B1 ingress remains.
Do not strip extensions or normalize regular keys away. Nonempty extension
source facts must exist while the wire keys are absent. Restore schema
renames on adapter output; schema-only divergences authorize no wire skip.

Required cases cover four distinct physical slots vs logical actions,
pending/ready/missing-score tournament states, stable aggregate on every stable
session, nonselected Windows foreground, common null vs out-of-play raw values,
numeric inverse boundaries/properties, denied private siblings, and no demand
from HTTP reads. P0/R0 tests additionally cover foreign registration,
method/matcher collisions, coherent focus/roster cuts, stale root generations,
same-open-object bytes/metadata, close-once cancellation and native confinement.
Provider invariants exercise 100-client shared bytes, cache bounds and eviction,
1,000-update coalescing, RAII demand and zero unchanged-chart parses.
All waits have explicit finite timeouts and barriers, never ordering sleeps.

```sh
# A4 docs and unchanged schema contract
python scripts/check-doc-links.py
python scripts/check-brand.py
jq empty docs/v2/schemas/*.json
# Offline JSON Schema validation through the existing fixture suite
cargo test --locked --test state_bus_compat
# P0/P1/R0 implementation gates (after those units land)
cargo nextest run --locked -p vosu-surface -p vosu-core -p vosu-osu --all-features
cargo test --locked --features test-harness --test state_bus --test plugin_host --test osu_plugin --test api_v1_compat
cargo test --locked --test state_api_compat --test server_compat --test state_bus_compat
cargo test --locked --test performance_invariants --test runtime_wakeups --test config_compat
cargo test --locked --features test-harness --test daemon_smoke
# Native Windows, pending when the VM is occupied; never substitute cross-Clippy
python scripts/vm-test.py --test state_api_compat
python scripts/vm-test.py --test server_compat
python scripts/vm-test.py --package vosu-core --lib
python scripts/vm-test.py --package vosu-sys --lib
```

Main lists expected names/nonzero counts before the integrated gate; native
Windows evidence names keys, multi-session foreground/tournament and root
confinement cases. No VM lifecycle action is implied by these instructions.
Use `flock /tmp/vosu-smoke.lock` for smokes. D17 names and measurement policy
are [frozen separately](benchmarks.md#m4a-d17-contract), not passed by functional
tests. M4b owns live captures/golden tooling and M8 owns migration execution.

The exit run of these commands, with exact counts, the native Windows results
and the native-only symlink-privilege and flake observations, is recorded in
the [M4a outcome](../v2/implementation-plan.md#m4a-outcome). The D17 status is
in the [benchmark guide](benchmarks.md#m4a-d17-status-at-the-exit).

## M4b behavioral goldens

The approved A4b freeze is the
[behavioral golden contract](../../tests/fixtures/upstream/GOLDEN_CONTRACT.md):
capture authority, harness endpoint and dump envelope, case sandwiches, route
families and header set, the closed normalization inventory, fixture layout,
size budget, divergence-rule format, pin and brand policy. Upstream
observations are the only expected values; vosu dumps are replay inputs. The
source oracle stays the projection golden. A private raw capture is never a
fixture, and `/api/ingame` is never captured.

The commands below are frozen for the implementing units; they are required
future evidence, not a claim that the targets or subcommands exist yet.
`tests/adapter_golden.rs` requires the `test-harness` feature
(`[[test]] required-features`), so the exit command names it:

```sh
# L1 tooling exit, at the integrated head
python -m unittest discover -s tools/live-parity -p 'test_live_parity*.py' -v
cargo test --locked --features test-harness --test adapter_golden
cargo test --locked --features test-harness --test daemon_smoke adapter_capture
cargo test --locked --features test-harness --test server_compat adapter_capture
cargo test --locked --test state_api_compat --test server_compat
python scripts/check-brand.py
python scripts/check-doc-links.py
python scripts/check-file-length.py
# Native Windows; the VM command must forward --features test-harness
python scripts/vm-test.py --test adapter_golden -- --nocapture
python scripts/vm-test.py --test daemon_smoke -- adapter_capture --nocapture
```

Also prove that a default build compiles and that a release-profile
`test-harness` build still fails in a disposable target. Record nonzero case
counts by platform and family. A featureless, zero-case or unpinned run, and
any synthetic mechanics fixture, is not pilot or full evidence.

Before the pilot fixtures land, `tests/fixtures/upstream/PENDING_PILOT` names
the Cargo pin. The pinned test then passes but prints that it compared no
upstream case. Without the marker it fails with "no golden fixtures for pin
`<pin>`", and it also fails when the marker names another pin or sits beside
fixtures. X0 deletes the marker in the fixture commit. Until then, report
adapter goldens as pending. The comparator's nonzero coverage comes from the
self-tests over `tests/fixtures/golden-synthetic/`. That tree is written by
G0's writer with `provenance: "synthetic"`. Each of its platforms compares only
on the host OS its name starts with (`windows-*` on Windows, `linux-*` on
Linux), so run `adapter_golden` natively on both, for example with
`python scripts/vm-test.py --jobs 16 --test adapter_golden`.
`python tests/adapter_golden/generate_synthetic.py --foreign-export <path>`
regenerates the tree from both hosts' exports.

Captures run only under a written owner authorization for that session
(commit and pin, client build/install/profile, scenario, data/media policy,
private evidence path, GUI operator scope, review route); VM lifecycle
approval is not capture authorization. On the authorized platform:

```sh
python tools/live-parity/run.py check-config --config "$PARITY_CONFIG"
python tools/live-parity/run.py golden-capture --config "$PARITY_CONFIG" --scenario-set "$APPROVED_SCENARIO" --out "$PRIVATE_CAPTURE"
python tools/live-parity/run.py golden --capture "$PRIVATE_CAPTURE" --candidate-dir "$CANDIDATE_DIR"
cargo test --locked --features test-harness --test adapter_golden
python scripts/check-brand.py
```

Report the evidence items listed in the contract's exit section; name the
private evidence location by label only. Main reviews each candidate before a
separate fixture commit, and nothing is uploaded or published.

### O0 pilot run (`golden-capture`)

`golden-capture` builds `cargo build --locked --features test-harness` (Windows:
`--no-default-features --features test-harness`) from a clean committed tree
and refuses a modified tracked tree. It also refuses an executable without the
harness endpoint, which rules out every release or featureless build. It
launches and owns both daemons, each on its own loopback port with isolated
roots, and passes the vosu daemon a fresh `VOSU_TEST_ADAPTER_CAPTURE`
capability. It checks that the endpoint answers only with that capability. It
then prepares the run-owned path-security layout and launches the client.
Operator prompts go to stderr, one per barrier, in the order menu → play →
paused → result → no-client (close the client). At each barrier every
golden-role route-matrix case runs as two sandwiches: upstream twice, then
vosu dump → route → dump. A case gets at most three whole-case attempts.
Every daemon and client process and every file the run created is removed
before the command returns. Outputs are written once and never replaced,
both mode 0600:

- `--out`: the `vosu-golden-capture-v1` capture that `golden` reads. It is
  written only for a complete run.
- `<out>.transcript.json`: raw records of every attempt, the build, launch and
  barrier attestations and the cleanup result. A failed run writes only this
  file.

A run whose evidence would contain the capability writes nothing.

The run configuration adds a `golden` block (`config-schema` prints its
schema):

```json
"golden": {
  "authorization": {"reference": "<owner authorization id>", "platform": "windows-lazer",
                    "source_revision": "<pin>", "vosu_commit": "<40-hex HEAD>",
                    "client_version": "<client_expected_version>",
                    "scenario_set": "m4b-dry-run-v1"},
  "capture_root": "C:\\vosu-capture\\m4b-pilot-<utc>",
  "capture_slot": "osu!",
  "route_roots": {"/Songs/": "C:\\vosu-capture\\m4b-pilot-<utc>\\lazer\\files",
                  "/files/beatmap/": "C:\\vosu-capture\\m4b-pilot-<utc>\\lazer\\files"},
  "other_roots": {"files": "C:\\vosu-capture\\m4b-pilot-<utc>\\lazer\\files"},
  "inside_files": {"/Songs/": "<relative file under files>",
                   "/files/beatmap/": "<relative file under files>"},
  "fixture_values": {"songs_file": "<relative file under files>",
                     "songs_dir": "<relative directory under files>",
                     "beatmap_relative": "<file relative to the current beatmap>",
                     "skin_file": "<legacy skin file name>",
                     "calc_relative": "<relative chart under files>",
                     "calc_absolute_inside": "<absolute chart under files>"},
  "sentinel_chart": "<generated vosu-owned .osu to copy outside the roots>",
  "assets": [{"id": "<asset id>", "relative": "<path under capture_root>",
              "media_type": "<type>", "generator": "<generator>", "license": "<license>"}]
}
```

Every root the client declares must lie inside `capture_root`, which is
task-owned. For lazer, point the task-owned test profile's data directory
inside it. The upstream daemon serves lazer's legacy `/Songs/` and
`/files/beatmap/` routes from the files store, so both route roots name the
files slot (also declared in `other_roots`), and those file and path cases run.
Every golden case runs except these, each listed in the transcript with its
authority:

| Excluded cases | Platform | Authority |
|---|---|---|
| `files.skin.file.{get,head}` | lazer | GOLDEN_CONTRACT §6 stable `/files/skin/<name>` variant; lazer records `files.skin.lazer-file.*` instead (`lazer-legacy-skin-404`) |
| `files.skin.lazer-file.{get,head}` | stable | GOLDEN_CONTRACT §6 lazer variant (`lazer-legacy-skin-404`) |
| `path-security.skin.*` | lazer | ledger `lazer-legacy-skin-404`: lazer has no legacy skin root to traverse from |
| drive, UNC, ADS, trailing dot/space spellings | Linux | GOLDEN_CONTRACT §6 path-security, Windows only |
| `absolute-encoded`, `absolute-double-slash` spellings | Windows | GOLDEN_CONTRACT §6 path-security, POSIX absolute spellings |
| `path-security.*.symlink` | Windows, only when the symlink cannot be created | GOLDEN_CONTRACT §6 "where the platform permits" |

Any other unmet prerequisite (a missing fixture value or root) fails the run
before anything is sent. A case whose connection closes without a response is
recorded as the `closed` outcome and compared; it never fails the run. The
authorization must name exactly this platform, pin, commit, client version and
scenario set, or the run is refused before the build.

1. **Mandatory dry run first.** Use a config whose
   `authorization.scenario_set` is `m4b-dry-run-v1`: the menu barrier and
   three cases. Then run `golden` on it into a scratch candidate directory, in
   the VM, before the full pilot. This catches integration gaps cheaply.
   Discard the dry-run outputs once reviewed; they are not pilot evidence.

   ```powershell
   python tools\live-parity\run.py check-config --config $env:DRY_RUN_CONFIG
   python tools\live-parity\run.py golden-capture --config $env:DRY_RUN_CONFIG --scenario-set m4b-dry-run-v1 --out $env:DRY_RUN_CAPTURE
   python tools\live-parity\run.py golden --capture $env:DRY_RUN_CAPTURE --candidate-dir $env:DRY_RUN_CANDIDATE
   ```

2. **Pilot.** Use the same config with `authorization.scenario_set` set to
   `m4b-full-v1`, and a new `--out`:

   ```powershell
   python tools\live-parity\run.py golden-capture --config $env:PARITY_CONFIG --scenario-set m4b-full-v1 --out $env:PRIVATE_CAPTURE
   ```

   Then run `golden`, `adapter_golden` and the brand check as listed above.
   Report the transcript's barrier log, retried cases, exclusions and cleanup
   result alongside the contract's evidence items.

## M3 API, resources and actions

A3 freezes the following exit commands for implementation/integration owners.
These are required future evidence, not a claim that M3 targets already exist
or have run. List each new integration target first and verify expected names
and nonzero counts; a feature-gated zero-test success is not evidence.

```sh
cargo nextest run --locked -p vosu-plugin-api -p vosu-plugin-testkit -p vosu-sys -p vosu-core -p vosu-osu --all-features
cargo nextest list --locked --features test-harness --test plugin_host --test state_bus --test osu_plugin --test api_v1_compat
cargo nextest run --locked --features test-harness --test plugin_host --test state_bus --test osu_plugin --test api_v1_compat
cargo test --locked --features test-harness --test daemon_smoke
cargo test --locked --test state_api_compat
cargo test --locked --test state_bus_compat
cargo test --locked --test server_compat
cargo test --locked --test performance_invariants
cargo test --locked --test runtime_wakeups
cargo test --locked --test config_compat
cargo test --workspace --lib --all-features --locked prop_
python tests/tool_host_smoke.py --binary "$CARGO_TARGET_DIR/debug/vosu" --playwright-module "$VOSU_PLAYWRIGHT_MODULE"
RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc --workspace --no-deps --all-features --locked
python scripts/check-doc-links.py
python scripts/check-brand.py
jq empty docs/v2/schemas/*.json
```

`api_v1_compat` must inventory every route/method/status/body/header, overlay-only
listener/CORS policy, absence/rebind/fixed detach, real saturated pong closure,
no-store actions/jobs, 416/502/504 and GET-only tree index. Real host+ABI tests
prove lazy zero-resolve publication, atomic rejected reference batches,
multi-owner stable ids/restart secret/tamper/410, indexed alias proof/truncation,
immutable old bytes/new id, file and stream validators/ranges/HEAD, exact close
counts, and all cleanup paths (success, error, cancel, timeout, detach, loss,
disconnect, shutdown). Fixtures use distinct path sentinels, ids, generations,
byte patterns and caller/callee identities; privacy asserts authorized presence
as well as API absence. Queue tests fill the actual queue; cancellation tests
observe callee cancellation and retained terminal relay delivery. Use bounded
waits/barriers, instance-owned hooks, and no ordering sleeps.

Two same-argument actions must dispatch differently by `ACTION_INVOKE.action`.
Schema tests reject unsupported keywords, cycles/depth/work overflow and invalid
nested arguments/results, with the dev-only offline differential oracle.
Job tests cover live launch ownership/revocation, broker authority, retention,
waiter saturation, first-terminal races and 64-call relay-slot pressure.
M3 `plugin_crashed` uses the production instance-loss seam; native subprocess
crash/kill proof and process cancellation grace are M5, not simulated M3 proof.

J3 composition cases live in `tests/api_v1/{actions,jobs,relay,cli,import}.rs`.
They exercise retained terminals versus running-job pressure, per-job/global
waiter limits and recovery, genuine late ABI results after timeout, exact launch
ownership, and terminal relay delivery while the real dispatch queue is full.
`vosux_production_action_routes` launches the actual CLI binary against J2 routes
and the real private broker credential adapter; its roots use executable-channel
policy. It covers required/optional/none routing, explicit sessions, revision
polling, detached zero-GET behavior, and Linux SIGINT-to-DELETE. All waits are
bounded. Run subprocess-bearing integration smokes under `flock /tmp/vosu-smoke.lock`.
In-process instance-loss tests are not evidence of an OS crash or kill: that
process-host lifecycle evidence remains explicitly assigned to M5.
The real osu import case supplies a live-reader resource observation to the actual
session, then dispatches import through HTTP and the real plugin ABI. It checks
`queued`, exact committed archive bytes, preserved existing bytes, the selected
Songs directory, and exactly one emitted success terminal after cancellation/
shutdown. A test-only forwarding `HostServices` wrapper counts every production
osu plugin `ACTION_RESULT` before host acceptance, including rejected duplicates.
J3 also fixes the registration boundary: the plugin's action-target session is
installed before `SESSION_IDENTIFY` publishes it; rejection rolls that preparation
back. Capture still starts only after acceptance, and B2 settings delivery is
still serialized by the same plugin-state lock. A re-entrant identify/action
regression tests the formerly missing-target window deterministically.

The browser phase must use a real launch exchange and testkit, actual API-v1
subscribe/invoke/start/progress/abort traffic, no forbidden cross-origin headers,
no token persistence/disclosure, and unchanged v1 bridge smoke; no silent skip.
Alpha evidence uses an actual `2.0.0-alpha.1` binary, distinct normal/alpha
fixtures, source byte/inventory comparison and restart after source mutation:
one-time read-only import, no normal-root writes or subsequent source reads,
rebased writable paths and isolated broker/secret/cache/offset/static state.
Malformed sources stay untouched. Live import uses isolated test installations,
never the user's real library.

### X0 — daemon, browser, alpha and live exit

Host commands (the fixture plugin is built from this checkout, not downloaded):

```sh
source scripts/dev-env.sh
export CARGO_TARGET_DIR=/home/user/.cache/vosu-targets/m3-x0
export VOSU_PLAYWRIGHT_MODULE=/home/user/.npm/_npx/e41f203b7505f1fb/node_modules/playwright/index.mjs
flock /tmp/vosu-smoke.lock taskset -c 0-7,16-23 nice -n 10 cargo test --locked --features test-harness --test daemon_smoke --test api_v1_compat --test plugin_host --test prerelease_bootstrap
flock /tmp/vosu-smoke.lock python tests/tool_host_smoke.py --binary "$CARGO_TARGET_DIR/debug/vosu" --playwright-module "$VOSU_PLAYWRIGHT_MODULE"
/opt/anaconda/bin/python -m pytest scripts -q
```

The v1 curl/crash/restart/EOF smoke remains mandatory. The subsequent browser
phase clicks the dashboard's Open control, exchanges its real launch code,
rejects reuse, subscribes to the discovered fixture session and verifies an
echoed nonzero sentinel. A separate `wait_progress` fixture script reports
0.375, remains running until Abort, and acknowledges cancellation through the
ABI. Browser assertions require actual POST/GET/DELETE responses and the
cancelled job, not merely an AbortError in JavaScript. Missing Node, Playwright,
Chromium, fixture library or grants fails the phase. Set `VOSU_CHROMIUM` only
to select an explicitly installed browser executable. Alpha fixtures compare
all normal-root file names/bytes before and after import, then poison the old
config and restart at the imported port without consulting it.

Exit evidence (2026-11-24, after merging `v2/main`):

- **Linux.** The full workspace nextest run passed 2,517 tests (6 skipped).
  Formatting, workspace and Windows cross-Clippy, every `check-*.py`, and every
  `scripts/test_*.py` suite passed. So did the tool-host smoke (retained curl
  phase plus the required Chromium phase) and the dashboard smoke. Listed
  targets: `api_v1_compat` 33, `daemon_smoke` 2, `plugin_host` 8,
  `osu_plugin` 8, `state_bus` 6, `prerelease_bootstrap` 1.
- **Native Windows (`vm-test.py`).** `api_v1_compat` 33/33, `server_compat`,
  `vosu-core` lib 574/574 and `vosu-osu` lib 196 passed (1 ignored).
  `daemon_smoke` ran 0 tests because the target is Linux-only. `vosu-sys` lib
  passed 59 under `vm-test.py`; its two Windows file-symlink tests fail with
  error 1314 there, because the LIMITED scheduled task lacks
  `SeCreateSymbolicLinkPrivilege`. At High integrity (the `prepush.py` native
  context) `vosu-sys` passed 61/61.
- **Live Windows.** A release `2.0.0-alpha.1` binary ran from an isolated
  program folder in the VM, against the VM's own test installations. Stable
  passed 51/51 resource, range, HEAD, 304/416, 307, tree, skin-alias, 410
  retirement and legacy checks. Its import returned `queued`, added one
  byte-identical archive to the selected Songs folder, and left all 436
  existing files unchanged. Lazer passed 33/38; the five misses come from the
  built-in intro map and built-in skin, which have no background and no
  indexed skin files. Its import returned `submitted` to the selected
  executable, and the handoff child was reaped. The game-side import was not
  observed within 30 s. Concurrent clients kept fixed ids, and one owner's
  detach did not retire the other's resources. A selected-destination import
  on a just-identified stable session returned `target_gone`, so that case
  is unproven. The raw evidence is under
  `/home/user/gh/vecosystem/evidence/m3-exit/`.
- **Pending.** Linux lazer and stable/Wine live runs: their isolated
  installations exist only in the Linux VM, which cannot run beside the
  Windows VM. Also pending: live source mutation, gameplay traversal, and
  the M3 D17 decision.

#### Final API-v1 route inventory

All paths below have the `/api/v1` prefix and exist on the overlay listener
only. `{session}` accepts an identified id or `active`. Exact shapes only:
near misses, extra segments and upgrades to non-subscription routes are 404.
Every recognized shape supports OPTIONS (204); unsupported methods are 405
with the route's Allow list. Queries are strict and percent-decoded once.

| Path | Methods | Successful representation / important failures |
| --- | --- | --- |
| `/sessions` | GET | identified summaries; 200 empty list before attachment |
| `/sessions/{session}` | GET | summary; absent active, unknown or detached id 404 `unknown_session` |
| `/sessions/{session}/state` | GET | public merged state; optional single `pointer` selection |
| `/sessions/{session}/state/{namespace}` | GET | public namespace; unknown namespace 404 |
| `/sessions/subscribe` | GET upgrade | 101 session-summary stream |
| `/sessions/{session}/subscribe` | GET upgrade | 101 state stream; fixed detach closes, active rebinds |
| `/resources/{token}` | GET, HEAD | bytes / metadata; conditional 304, range 206/416; invalid 404, authentic retired 410, resolver 502/504 |
| `/trees/{token}/` | GET | bounded index (HEAD is 405) |
| `/trees/{token}/{path}` | GET, HEAD | indexed canonical member; confinement and family policy apply |
| `/sessions/{session}/chart/{kind}` | GET | redirect to current file/audio/background reference |
| `/sessions/{session}/skin/{path}` | GET | redirect through current indexed skin tree |
| `/actions/{action}` | POST | session-less job; `arguments`, bounded `wait`; 200 terminal / 202 running |
| `/sessions/{session}/actions/{action}` | POST | session-bound job; schema/capability/session checks before dispatch |
| `/jobs/{id}` | GET, DELETE | owned job revision/long poll or cancellation request; unknown/non-owned 404 |

Reads/subscriptions use API privacy projection and overlay CORS policy.
Resources/tree responses apply secure content policy and validators; actions,
jobs and errors are no-store. Action/job authority is a live exact tool launch
or native loopback broker, not an API key: missing credentials 401, insufficient
capability 403, invalid arguments 400. DELETE requests cancellation rather than
inventing a terminal state. Dashboard launch/exchange remains
`POST /api/tools/{tool}/launch` (dashboard CSRF) and
`POST /tools/_host/v1/session` (single-use code); the v2 client is
`GET /tools/_host/v2/bridge.js`, and v1 remains available unchanged.

#### Pending native/live runbook (do not use personal game libraries)

Run the native commands below only when the assigned VM is available. The
Linux-only daemon test is not Windows daemon evidence; native Windows
`server_compat`/`api_v1_compat` and sys confinement tests remain required.
Do not start or stop a VM as part of this unit.

For each of Windows stable, Windows lazer, Linux lazer and Linux stable/Wine:
prepare a fresh disposable client installation/profile with a licensed test
chart and a distinct chart/audio/background/skin byte sentinel. Complete the
client's first-run setup **inside that profile**, keep it offline, and record
its executable, client PID, install/data/Songs/store paths and source hashes.
Never pass a personal Songs/store path. Launch the exact fixture client:

```sh
# Linux lazer (fixture profile via isolated HOME/XDG); use absolute paths.
export LIVE_ROOT="$(mktemp -d /tmp/vosu-x0-live.XXXXXX)"
mkdir -p "$LIVE_ROOT"/{home,config,data,cache,runtime,evidence}
HOME="$LIVE_ROOT/home" XDG_CONFIG_HOME="$LIVE_ROOT/config" XDG_DATA_HOME="$LIVE_ROOT/data" XDG_CACHE_HOME="$LIVE_ROOT/cache" XDG_RUNTIME_DIR="$LIVE_ROOT/runtime" "$LAZER_FIXTURE_EXE"
# Linux stable/Wine: a newly initialized fixture-only Wine prefix.
WINEPREFIX="$LIVE_ROOT/wine" wine "$STABLE_FIXTURE_EXE"
# In another terminal, same isolated daemon roots for this run:
HOME="$LIVE_ROOT/home" XDG_CONFIG_HOME="$LIVE_ROOT/config" XDG_DATA_HOME="$LIVE_ROOT/data" XDG_CACHE_HOME="$LIVE_ROOT/cache" XDG_RUNTIME_DIR="$LIVE_ROOT/runtime" timeout --signal=TERM 600 "$CARGO_TARGET_DIR/debug/vosu" --update=false
```

On Windows, use a disposable OS profile or sandbox for lazer (its selected
profile must not resolve into the user's AppData), and a separate portable
stable fixture installation. In that sandbox's PowerShell:

```powershell
$env:USERPROFILE = $LiveRoot
$env:APPDATA = "$LiveRoot\\Roaming"
$env:LOCALAPPDATA = "$LiveRoot\\Local"
New-Item -ItemType Directory -Force $env:APPDATA,$env:LOCALAPPDATA | Out-Null
Start-Process -FilePath $FixtureClientExe -WorkingDirectory (Split-Path $FixtureClientExe)
& $FixtureVosuExe --update=false
```

Use the disposable profile's OS-known folders as well as these variables;
changing environment variables alone is not proof of Windows isolation.
Close the test daemon normally after each bounded (at most ten minute) run.
Against the overlay origin configured by that isolated daemon:

```sh
export BASE=http://127.0.0.1:24050
curl --fail --max-time 10 "$BASE/api/v1/sessions" | tee "$LIVE_ROOT/evidence/sessions.json"
export SESSION="$(jq -r '.sessions[] | select(.plugin.id=="vosu.osu") | .session' "$LIVE_ROOT/evidence/sessions.json")"
curl --fail --max-time 10 "$BASE/api/v1/sessions/$SESSION/state" > "$LIVE_ROOT/evidence/state.json"
jq '."vosu.chart".resources, ."vosu.skin".resources' "$LIVE_ROOT/evidence/state.json"
# Choose each published URL from that output, not an arbitrary filesystem path.
curl --fail --max-time 10 "$BASE$RESOURCE_URL" -o "$LIVE_ROOT/evidence/resource.bin"
cmp "$EXPECTED_FIXTURE_FILE" "$LIVE_ROOT/evidence/resource.bin"
curl --fail --max-time 10 -I "$BASE$RESOURCE_URL"
curl --fail --max-time 10 -H 'Range: bytes=1-7' "$BASE$RESOURCE_URL" -o "$LIVE_ROOT/evidence/range.bin"
dd if="$EXPECTED_FIXTURE_FILE" bs=1 skip=1 count=7 status=none | cmp - "$LIVE_ROOT/evidence/range.bin"
# Same isolated root environment as the daemon; vosux discovers its alpha broker.
"$VOSUX_BIN" vosu.osu beatmap.import --session "$SESSION" "{\"sourcePath\":\"$FIXTURE_OSZ\"}"
```

Windows equivalents use `curl.exe --max-time 10`, `Get-FileHash` and
`& $VosuxExe vosu.osu beatmap.import --session $Session
('{"sourcePath":' + (ConvertTo-Json $FixtureOsz -Compress) + '}')`.
Expect stable `queued/refreshRequired=true`, byte-identical new archive in the
selected fixture Songs directory, and unchanged pre-existing same-name files;
F5/restart is game-owned. Expect lazer `submitted/refreshRequired=false`,
handoff only to the selected fixture executable/profile, with no direct
Realm/store writes by vosu. Observe the fixture chart becoming available in the
game; retain the result and before/after file inventories. Repeat with two
fixture clients running and explicit session ids: changing foreground must not
change the selected import destination. Check no orphaned handoff children.

Exercise menu → play → pause → result → client exit; fetch chart, audio,
background, skin index and a case/alias member while attached. Verify 200/206,
exact bytes, HEAD validators, conditional 304, invalid range 416 and unpublished
member 404. Mutate the stable source: the old immutable id must yield old bytes
or 410, never silently new bytes; a newly published id yields new bytes.
After detach, verify retired session/resource statuses and no private native
paths in API JSON, frames or errors. Fetch `/json`, `/websocket/v2` and the
retained skin/chart legacy routes to confirm unchanged legacy behavior.
Record every client/version/command/hash/status and pending observation here;
neither a fixture-only integration test nor cross-Clippy closes this live gate.

Native Windows exit commands (integration owner; A3 does not use/start a VM):

```sh
python scripts/vm-test.py --package vosu-sys --lib
python scripts/vm-test.py --package vosu-core --lib
python scripts/vm-test.py --package vosu-osu --lib
python scripts/vm-test.py --test api_v1_compat
python scripts/vm-test.py --test daemon_smoke
python scripts/vm-test.py --test server_compat
```

Record Windows stable/lazer, Linux lazer and stable/Wine plus concurrent-client
observations: selected-client roots/import, no overwrite/store mutation, joined
handoff children, resource bytes/ranges, aliases and unchanged legacy APIs.
Cross-compilation is not native evidence. D17 requires the separately named
[M3 benchmark contract](benchmarks.md#m3-d17-contract); functional success does
not close inherited M1/M2 performance findings.


## CI baseline

`.github/workflows/ci.yml` runs these checks; `scripts/prepush.py` runs the same set locally and in the Windows VM before `scripts/land.py` moves `main` ([ADR 0018](../decisions/0018-enforce-rules-with-checks.md)):

```sh
python scripts/check-doc-links.py
python scripts/check-file-length.py
python scripts/check-brand.py
python scripts/check-module-boundaries.py
python scripts/check-crate-boundaries.py
python scripts/assemble-release-license.py
python scripts/check-asset-hashes.py
python -m unittest discover -s scripts -p 'test_*.py'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo nextest run --workspace --lib --bins --tests --examples --all-features --locked
cargo xwin clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
# UI paths changed: build with --features test-harness, then
python tests/tool_host_smoke.py --binary target/debug/vosu --playwright-module <playwright/index.mjs>
python tests/dashboard_smoke.py --binary target/debug/vosu --playwright-module <playwright/index.mjs>
# Native Windows (scripts/native-ci.ps1 in the VM):
cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo nextest run --workspace --lib --bins --tests --examples --all-features --locked --target x86_64-pc-windows-msvc
cargo test --workspace --doc --all-features --locked --target x86_64-pc-windows-msvc
```

CI runs on pull requests, on `main`, and on `ci/**` landing branches, and always publishes `CI required`, including for documentation-only changes. The classification job runs the repository check scripts without network or inference. Code changes run Linux Clippy/tests and a Windows MSVC cross-check. Shared, platform, build, or workflow changes also run native Windows verification; pure schema/calculator changes keep the cross-check without native Windows. Dashboard, tool host, and catalog paths add the `dashboard` smoke job. Manual dispatch runs every lane. The `main` ruleset requires `CI required`, with no bypass actors, rather than the conditionally skipped platform jobs. Cross-compilation is not native execution evidence. `scripts/test_gate_tooling.py` fails when `ci.yml` gains a check that the local gate does not run.

Locally, `prepush.py`'s `test-linux` step runs `cargo nextest run` inside a podman container built from `scripts/ci-container/Containerfile`, which mirrors `ci.yml`'s `ubuntu-latest` validation runner's package set (today Ubuntu 26.04; the Containerfile is the one place that release is named). The step builds a `cargo nextest archive` on the host, then replays it as a non-root user in the container with the source tree and `CARGO_TARGET_DIR` bind-mounted read-only at their own host paths, so compile-time-baked paths like `env!("CARGO_MANIFEST_DIR")`/`CARGO_BIN_EXE_*` still resolve and no workspace remap is needed. The image is tagged with the Containerfile's own content hash, so it rebuilds only when that file changes (a cold build adds roughly 10s; a warm one is instant) and the whole step adds under ~10s over a plain host `cargo nextest run` once the image is cached. This is what catches a failure specific to that distribution's packages before it reaches hosted CI — for example, ubuntu-latest's uutils-coreutils `sleep` refusing to run under a renamed executable name (run 45295358591), which a host run on a GNU-coreutils machine cannot see. Without `podman` on PATH the step falls back to a plain host run and records that fallback in the evidence; `scripts/test_ci_container.py` keeps the replayed command matched to ci.yml's and exercises the podman-unavailable/cargo-nextest-unavailable failure paths.

The nightly workflow (`.github/workflows/nightly.yml`) runs the lazer offset canary, the property tests with random seeds and 20,000 cases each, and all four browser smokes. Its failures reach the team through GitHub check-suite notifications.

MSVC SDK caching is independent of Cargo.lock and restricted to x86_64. Prepared SDKs are saved before later lint failures. Rust artifact caches use separate target/lane keys and weekly generations to limit archive churn; downloads are cached separately. The Ubuntu validation lane builds with `CARGO_PROFILE_DEV_DEBUG=line-tables-only`: full debuginfo made the all-target test build large enough to exhaust the hosted runner's disk. Tagged releases package, extract, and exercise the exact Linux/Windows archives, using a small smoke-test package that does not depend on the daemon crate.

## Evidence workflows

`tools/live-parity/` is an optional interactive diagnostic, not a normal unit test or a required check ([ADR 0011](../decisions/0011-live-parity-optional.md)). Use it to investigate a suspected divergence from upstream: validate configuration, capture a controlled client lifecycle, compare, and generate diffs. It requires pinned artifacts, distinct loopback ports, and supported platform/client combinations. Dated captures, release gates, and native runs are kept in the [verification history](verification-history.md) as evidence for their recorded source generations.

### Live parity for the bus-backed adapters

Since M4a the compared vosu endpoints are the three bundled adapter
projections mounted through the permanent surface: HTTP `/json`, `/json/sc`,
`/json/v2`, `/json/v2/precise` and the `/ws`, `/tokens`, `/websocket/v2`,
`/websocket/v2/precise` feeds. The tool attests the 2.0 workspace package
version and compares the adapters' real bytes against the pinned upstream
reference with no extension exemption: ADR 0021 removed every 1.x extension
member (the `adr0021-*` rows of the
[divergence ledger](../../tests/fixtures/upstream/DIVERGENCES.md)), so any
reappearance, including nested tournament copies, is a `keys` mismatch. The
generated vosu configuration no longer writes the retired
`ENABLE_KEY_OVERLAY` or `V2_SUBSCRIPTION_DEBOUNCE`; real feed selectors drive
precise demand. Only the owner starts a capture, with a validated
configuration, distinct loopback ports and a supported native pair, traversing
menu → active play → paused → result → client exit:

```sh
python tools/live-parity/run.py config-schema
python tools/live-parity/run.py check-config --config "$PARITY_CONFIG"
python tools/live-parity/run.py capture --config "$PARITY_CONFIG" --out "$CAPTURE"
python tools/live-parity/run.py compare --capture "$CAPTURE" --report "$REPORT"
python tools/live-parity/run.py diff --capture "$CAPTURE" --out-dir "$DIFF_DIR"
```

A capture remains diagnostic evidence for its recorded build only; it neither
replaces nor relaxes the deterministic gate. That gate is `state_api_compat`
(committed source oracle replayed through producer → ABI → bus → surface →
adapter bytes), `server_compat` (the mounted production listener matrix) and
`daemon_smoke` with `test-harness`. Fix a reported mismatch in the owning
adapter or record an approved ledger row with exact route and member scope;
never add a comparator exemption. These release-build diagnostic captures are
never golden evidence. M4b owns the pilot/full golden capture commands under
its [own contract](#m4b-behavioral-goldens); until committed fixtures exist,
report missing captures as pending M4b/release evidence, never as a pass. The
1.x captures in the verification history attest vosu 1.2.5 only.

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

`tests/` contains compatibility (`state_api_compat`, `server_compat`, `config_compat`, `updater_compat`), native (`process_memory`, `updater_native`), daemon smoke (`daemon_smoke`), and invariant (`performance_invariants`, `state_bus_invariants`) contracts. `crates/vosu-release-smoke/` tests an external packaged executable. Focused unit/service tests live beside their owning modules. `crates/vosu-test-harness/` supports integration tests only.

Keep unit tests for distinct consumer-visible behavior: boundaries, precedence, errors, state transitions, security, numerical results, and concurrency or resource bounds. Do not add tests that only repeat constants/help text, construct and read back fields, prove standard-library clone/ownership behavior, or echo arguments through mocks. When committed source-oracle fixtures already cover a wire contract, add a focused unit test only for an uncovered case; do not duplicate the same default or not-ready payload. Preserve provenance-pinned upstream tests through their owning workflow rather than culling them as first-party tests.

Filesystem fixtures must use names legal on both Windows and Linux. Build
expected native paths one component at a time with `Path::join`; reserve
literal `/` separators for URL or wire-format assertions. HTML-escaping tests
that create real directories can use `&`, rather than Windows-invalid `<`/`>`.

When a change crosses [runtime boundaries](../architecture/overview.md), run the narrowest affected contract first and then the platform/live gate appropriate to the observable behavior. Do not substitute a compile-only check for behavior verification.


## M6a rule matrix and cutover inventory

Approved 2026-11-24: Q1–Q7 are frozen in the
[package contract](../v2/catalog-v2.md#m6a-validation-contract-approved-2026-11-24),
[lane profile](../v2/state-schema.md#sampling-lanes) and
[matcher contract](../v2/plugin-abi.md#61-process-matching).
Each corpus row records a stable rule id, document pointer, valid control
and one-mutation rejection. Inventory categories:

| Rule family | Required discriminators |
| --- | --- |
| `manifest.shape`, `manifest.shared` | Three kinds, required/default fields, forbidden/unknown fields, semver, confined assets, HTTP(S) homepage, positive bounded resolution |
| `manifest.context` | Official identity, both collision directions, segment boundary, same identity deduplication, longest namespace, source/kind-local replaces |
| `manifest.capability` | Grammar, duplicates/intersection, per-kind permissions, internal subtree, own/authoritative field revisions, literal game star |
| `manifest.resource`, `manifest.action` | Reserved/core coverage vs own provides, kind/provide pairing, MIME families/tree shape, action bijection and swapped versions |
| `manifest.matcher` | 0/1/15/16 UTF-8 bytes, NUL, regex syntax, matcher iff game; unchanged runtime overlap/prefilter |
| `manifest.requirement` | Exact/caret/tilde/wildcard/intersections/prereleases/build metadata, coverage, single-candidate conjunction |
| `manifest.schema` | Whole-document byte/depth/ref/visibility/work limits, siblings, wildcard/map/array and independent expected lanes |
| `catalog.shape`, `catalog.mirror` | Packages/art/targets, null/defaults, source-local uniqueness, unsupported vs malformed, ordered fields and asset-backed equality |
| `consumer.legacy`, `consumer.cutover` | Manifest precedence/no fallback, unchanged legacy/settings bytes, real production loader parity and rollback |

### Consumer migration ledger

| Baseline / consumer | Integrated validation authority | Retained host ownership |
| --- | --- | --- |
| K0 plugin manifest; F2 actions | SDK `manifest::PackageManifest`, typed plugin/tool/overlay views, shared capability/context/requirement predicates and action declarations | Application supplies its own version and exact official/registration context; intrinsic validity alone is not contextual validation |
| K1 namespace compiler; core `state/schema` and plugin host state | SDK `manifest::schema` full-document compiler and shared lane-demand union; core's independent walker/visibility implementation removed | Embedded bytes, provenance, schema/provide equality at registration, grants, rollback/drop lifetime and actual demand |
| Core plugin registry and root `runtime/dev_plugins.rs` | Shared package validation using the live `ValidationSnapshot`; bundled and development schemas have explicit provenance | Confined asset opening, native-library/ABI loading and runtime trust |
| D18 core `plugin/matcher.rs` | Shared SDK matcher document checks | Anchored runtime engine, platform matching, enabled-set overlap/256-entry prefilter and installed-order tie-break; overlapping uninstalled catalog candidates stay valid |
| T0 root `tools/manifest_v2.rs`, `tools/capabilities.rs` and discovery | SDK package/requirement validation; v2 adapter converts validated fields to host presentation, not a second parser | Loaded-provider availability, exact action/version lookup, live launch/session authority and job isolation; no legacy permission grants |
| Root `counters/service/{manifest,discovery}.rs` | SDK overlay view and pure `manifest::legacy_metadata` parser | Manifest-first single discovery/no invalid-manifest fallback, confined bounded file reads, settings and download ownership; metadata/settings remain unwritten |
| SDK `catalog/` and offline packaging schemas | Strict catalog validation and asset-backed mirror comparison; structural schema parity in `tests/packaging_schema.rs` | M6b/M7 must supply package schema bytes and call comparison before installation; no network catalog cutover in M6a |

The plugin-specific API is a typed package view, not an independent validator.
Legacy `tool.toml` is explicitly separate input through M7, not a v2 fallback.
M6b owns source/credential/trust/provider selection and dependency/install
orchestration; it reuses the SDK's coverage, version and candidate-conjunction
predicates. M7 owns final legacy-host removal and first-start migration.

### Resolved specification approvals

Main adopted Q1–Q7 on 2026-11-24 under delegated owner authority:

| Approval | Adopted contract |
| --- | --- |
| Q1 | Metadata-only array/map `*`, no literal `*` property collision; escaped pointers; lexicographically sorted distinct `(pointer,lane,activity)` triples, exact deduplication only |
| Q2 | Full-document reference/visibility checks, RFC 6901 local refs and fixed trusted common-defs, supported ref siblings, cycle/dangling rejection, 262,144-byte inclusive / depth-32 / 50,000-work bounds; unsupported lane-bearing constructs fail, without importing the action-value keyword profile |
| Q3 | Explicit trusted bundled/official schema provenance; copied `vosu.osu` identity/title grants no visibility exception |
| Q4 | Reject duplicates in capability/action/kind/slot/covers lists and unknown or cross-kind fields |
| Q5 | V2 resolution has exactly two integers in `1..=16384`; legacy behavior unchanged |
| Q6 | Both Linux hint fields require 1–15 UTF-8 bytes without NUL on every platform; Windows may ignore valid hints at runtime only |
| Q7 | Matcher iff a game provide; own leaf provides require an own whole-namespace schema declaration; foreign leaves require the supplied authoritative revision, and missing revision fails |

### Offline schema use and limits

Use the committed [manifest](../v2/schemas/manifest.schema.json) and
[catalog](../v2/schemas/catalog-v2.schema.json) schemas with an offline Draft
2020-12 engine. Parse TOML to its JSON data model first; raw TOML is not schema
input. Every ref is local; fixed HTTPS ids are not fetch instructions.
Defaults are annotations, not injected values. Shape-invalid corpus rows must
fail both validators, while SDK-only semantic failures may pass the schemas.

Schemas cannot prove semver/prerelease behavior, UTF-8 byte lengths, Rust regex
syntax, contextual officialness/namespace collisions, action/version
correspondence, canonical extracted lanes or package/schema equivalence.
The SDK remains authoritative; mirror verification needs actual loaded schema
assets. A catalog-only/schema pass grants no trust, permissions, host support,
download safety or installation approval. `jsonschema` is dev-only; production
does not evaluate published values with a JSON Schema engine.

## M6a outcome and evidence

Integrated source baseline: **`7ea4958`**, V0–V6. The corpus has **266 paired
rejection controls** (valid control, one mutation, rule id and pointer) and
**38 literal semver truth rows**, shared with the SDK's `manifest` filter.
Counts are rows, not separate test functions: the ungated root target has
**22 tests**, or **23 with `test-harness`/all features**. Child inventories are
`tests/catalog_v2/{manifest,plugin,declarations,catalog,context,capability,schema,mirror,matcher,legacy,consumers}.rs`;
the shared truth fixture is
`crates/vosu-plugin-api/tests/fixtures/requirement_truth_table.rs`.

### Linux evidence supplied by integration

Main supplied UnitV6's observed results at exactly `7ea4958`; V7 does not
claim to have rerun these Rust commands. UnitV6 used an isolated target
directory and CPU set `0-7,16-23`.

| Command / evidence | Result |
| --- | --- |
| `cargo test --locked -p vosu-plugin-api manifest` | 62 passed |
| `flock /tmp/vosu-smoke.lock cargo nextest run --workspace --all-features --locked` | 2,556 passed, 6 skipped; includes the all-feature root corpus |
| `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` | Passed (integration report) |
| Scoped Windows `env -u C_INCLUDE_PATH cargo xwin clippy` | Passed (integration report); cross-check only, not native execution |
| All nine `scripts/check-*.py`, including `check-deleted-assertions.py --base v2/main` | Passed (integration report) |

No new compiler/load-time cost measurements were taken. Functional green and
cross-Clippy are not a performance pass; inherited M1/M2/M3 findings remain
unchanged. The six workspace skips are not evidence for skipped behavior.
This record is not a complete prepush/smoke/release gate.

### Reproduction commands

From the repository root, using the pinned toolchain:

```sh
source scripts/dev-env.sh
export CARGO_TARGET_DIR=/home/user/.cache/vosu-targets/m6a-v7
# Required first for a fresh target's real plugin consumer fixtures:
taskset -c 0-7,16-23 nice -n 10 cargo build --locked -p vosu-plugin-testkit --all-features
taskset -c 0-7,16-23 nice -n 10 cargo test --locked -p vosu-plugin-api manifest
taskset -c 0-7,16-23 nice -n 10 cargo test --locked -p vosu-plugin-api catalog
taskset -c 0-7,16-23 nice -n 10 cargo test --locked -p vosu-plugin-api manifest_schema
taskset -c 0-7,16-23 nice -n 10 cargo test --locked -p vosu-plugin-api catalog_schema
taskset -c 0-7,16-23 nice -n 10 cargo test --locked --test catalog_v2_compat -- --list
taskset -c 0-7,16-23 nice -n 10 cargo test --locked --test catalog_v2_compat -- --nocapture
taskset -c 0-7,16-23 nice -n 10 cargo test --locked --features test-harness --test catalog_v2_compat
taskset -c 0-7,16-23 nice -n 10 flock /tmp/vosu-smoke.lock cargo nextest run --workspace --all-features --locked
```

These are reproduction instructions, not additional V7 execution claims.

### V7 documentation checks

V7 ran all nine checks successfully on its documentation worktree over
`7ea4958`, after `source scripts/dev-env.sh` with
`CARGO_TARGET_DIR=/home/user/.cache/vosu-targets/m6a-v7`:

```sh
for check in scripts/check-*.py; do
  if [ "$check" = scripts/check-deleted-assertions.py ]; then
    taskset -c 0-7,16-23 nice -n 10 python "$check" --base v2/main
  else
    taskset -c 0-7,16-23 nice -n 10 python "$check"
  fi
done
```

Results: 48 asset digests matched; brand, crate/module boundaries, file length
and lint-expectation checks reported zero errors. Local links checked 66
maintained Markdown files with zero missing paths. The deleted-assertion check
passed with 198 historical removals acknowledged by an existing trailer.
Standalone checks passed for 18 workspace members on Linux and Windows
**xwin**, which is not native Windows execution. No VM action was taken.

### Native Windows evidence

Main ran both commands natively in the Windows agent VM on 2026-11-24, over
`2c349eb`, using the staged HEAD and `--jobs 16`:

```sh
python scripts/vm-test.py --jobs 16 --package vosu-plugin-api --lib -- manifest
python scripts/vm-test.py --jobs 16 --test catalog_v2_compat
```

Results: `manifest` **59 passed**, the same count the Linux lib filter lists.
`catalog_v2_compat` (all features) **23 passed**. Zero failures and zero
ignored. These runs cover native Windows filesystem, confined-asset and
manifest/metadata precedence behavior; cross-Clippy alone did not. The
main-owned integrated landing gate is still required.

## S1 vosulay supervisor evidence (D16)

S1 tested [D16](../v2/implementation-plan.md#10-adopted-decisions-and-risks)
with a throwaway in-process spike plugin, `crates/vosu-spike-vosulay` on
branch `spike/s1-vosulay-supervisor`, which was never merged. It starts
vosulay's host executable as a child: inside a job object on Windows, and with
`PR_SET_PDEATHSIG` on Linux. The spike source, its scripts and its receipts are
evidence inputs only, and no production code is derived from them. Receipts
are kept outside the repository:

- Windows: `/home/user/gh/vecosystem/win-osu/evidence/s1-final-20261124/` on
  the host, and `V:\bt\units1win\` in the guest.
- Linux: `/home/user/gh/vecosystem/evidence/s1-linux/`, with the summary in
  `SUMMARY.md`.

**Verdict: D16 stands on both operating systems.**

- Windows passed on 2026-11-24, conditional on launching the vosulay host
  unelevated at Medium integrity.
- Linux passed on 2026-11-25 with a daemon that holds file capabilities.
  Chromium's user-namespace sandbox was the active mechanism there; the setuid
  helper was not.

The M5 consequences are in the
[S1 outcome](../v2/implementation-plan.md#s1-outcome-2026-11-25).

### Evidence classes

Each class proves only what its own run observed. A later class does not
upgrade an earlier one.

| Class | What it shows | What it does not show |
| --- | --- | --- |
| Linux host-only run, 2026-11-23 (no `vosu --setup`, no setuid helper) | The child had `NoNewPrivs: 0`, `CapPrm`/`CapEff` zero and a nonzero `CapInh`. `kill -9` of the daemon removed the vosulay child within 1 s. | This daemon held **no file capabilities**, and no CEF process existed. It is not capability or sandbox evidence. |
| Windows pre-control runs, 2026-11-23/24 | Strict job creation and assignment succeeded. `IsProcessInJob` was true. `taskkill /F` on the daemon removed a live host that had no CEF children. | `IsProcessInJob(NULL)` means membership in *some* job: the scheduled-task environment already supplies one. The High-integrity real-client attempt ended in CEF exit 38, which is Chromium's auto-de-elevation result. It is neither a sandbox failure nor a job failure. |
| Windows controlled matrix, 2026-11-24 | Live packaged CEF ran under the original strict job at Medium integrity, and the live tree was killed when only the daemon died. The negative control is included. | It says nothing about launching from an elevated daemon. The S1 daemon itself ran at Medium. |
| Linux VM run with a file-capability daemon, 2026-11-25 | Supervision, zero host capabilities with `NoNewPrivs: 0`, a live namespaced and seccomp-filtered CEF tree, and core-only forced death. | The setuid `chrome-sandbox` path did not run. The daemon was a debug build with file capabilities, not a release package. |

### Windows final matrix (2026-11-24)

All accepted cells ran under these identical conditions:

- the same packaged vosulay 1.5.3 bootstrap, with CEF
  `156.0.15+gcf671c0+chromium-156.0.8078.80`;
- `--config V:\bt\units1win\matrix\config.json`;
- real osu!lazer in interactive session 1;
- `/IT /RL LIMITED` scheduled-task launchers.

In every cell, `environment.json` records the game and runner tokens at RID
8192 (Medium), not elevated. The initial `GetForegroundWindow` PID equals the
game's PID.

Spike plugin source `a522e20a4b` adds `VOSU_SPIKE_JOB_MODE`. Unset or
`strict` keeps the original kill-on-close job, and `none` skips only that
assignment. The matrix rebuilt only the spike crate (`matrix/build.log`, exit
0). No CEF sandbox setting changed, and no `--no-sandbox` or breakaway flag was
used.

| Artifact | SHA-256 |
| --- | --- |
| Matrix spike DLL `C:\s1spike\dev-plugin\lib\vosu_spike_vosulay.dll` (`matrix/dll-hash.json`) | `4A74F2A1B5C3FFDEC36FF7EB1BA827BFA2B4C1AB2E31BDC21B1DCFC494F51074` |
| Debug daemon `C:\s1spike\bin\vosu.exe` (`evidence/hashes.json`) | `DCA79901075C002507D8B8EB8E517C20DF80B7C8CD4ADE1DAF3798C00D4E4D85` |
| Packaged `vosulay.exe` | `0C73CC1DD7F1F2752ACE2AA1D12AA08C99FA08137C4A3FF0ADBBBF1612FF5709` |
| Packaged `vosulay.dll` | `C881F031B91F1CB02F1715EBD116252F9CE138E19C592BFDB263C2F43D22CA23` |
| Packaged `libcef.dll` | `07168EACD81BC09AB9AB895D39BC1A80913B09C844F0B5A4730F5F89A71133F5` |
| Pre-control spike DLL (earlier source `768ad1112c`) | `93D1CB5F8E0D9FBD91AC525E1AC9AB0B55FE08245308781ECA7F58D095CDDBC0` |
| Host `matrix-receipts.zip` | `d290992fcbea0d13bec92096cb74a180d036e2c05ae98d5266f72ff702fdd7cd` |
| Host `matrix.bundle` | `b783121b86d1d9668f39db964407c33b83faf00e4612a9fdc94cc676fedde8bd` |
| Host `spike.bundle` | `704d2914897859773989c6f413562fd5f005971aab145c9b73aefd75a27bf85d` |

Where the hashes come from:

- The daemon and package hashes were taken for the same `C:\s1spike` paths
  during the pre-control run (`evidence/hashes.json`). The matrix used those
  same paths, but no matrix receipt re-hashed them.
- The three host-file digests were computed when S0 recorded this evidence.

| Cell | Live packaged CEF | Forced daemon death |
| --- | --- | --- |
| (a) Standalone vosulay, no supervisor | **PASS**: host 10836, GPU 9548, renderer 2172 and utilities (`a/tree-live.json`). | No daemon. Standalone cleanup left no recorded process after 2.157 s. |
| (b) Medium daemon, original strict kill-on-close job | **PASS**: daemon 11448 → host 16288 → GPU 5456, renderer 15884 and utilities (`b/tree-live.json`). | **PASS**: `taskkill /F /PID 11448` left every recorded live-tree PID absent at 2.156 s, including renderer and GPU (`b/kill.json`). No `/T` was used. |
| (c) Medium daemon, assignment disabled (negative control) | **PASS**: daemon 13376 → host 5384 → GPU 2132, renderer 1992 and utilities (`c/tree-live.json`). | **Survived as expected**: after only daemon 13376 was killed, the host and all four CEF children remained at 2.193 s (`c/kill.json`). They were then cleaned up explicitly. |
| (d) Breakaway variants | Not run, because (a) and (b) both passed. | Nothing here justifies changing a production job limit. |

Tokens were read with `OpenProcessToken`, `GetTokenInformation` and
`IsTokenRestricted`, and are identical across (a)–(c):

- The host is Medium and unrestricted.
- Each GPU process is Low (RID 4096) and restricted.
- Each renderer and storage utility is Untrusted (RID 0) and restricted.
- The network utility is Medium and unrestricted, and declares
  `--service-sandbox-type=none`.

No process ran in an AppContainer, and AppContainer is not claimed as the
mechanism.

The proof that the strict job was assigned is the source/env switch plus the
opposite kill results of (b) and (c). It is not a generic membership query.

The preserved short samples for (c) (`c-reused-client`, `c-short`,
`c-focus-lost`, `c-attach-refused`) are retained, not used. (c) has no
`done.json`, because its cleanup loop failed on an already-gone PID. The
independent `matrix/cleanup.json` shows no remaining processes or `UnitS1Win*`
tasks.

Exit 38 sources were checked at pinned revisions (`exit38-sources.json`).

- Chromium `chrome_result_codes.h` maps 38 to
  `CHROME_RESULT_CODE_NORMAL_EXIT_AUTO_DE_ELEVATED`.
- `chrome_main_delegate.cc` relaunches without elevation.
- CEF `main_runner.cc` logs any nonnegative result as a failure.
- The sandbox API's own 38 is a different enum and does not explain the exit.

### Linux VM run (2026-11-25)

Environment:

- Guest: `vosu-gaming` (Arch, linux-zen, Plasma Wayland, user `vosu`).
- vosu: spike `66de385`, **debug** build, loaded with `--plugin`.
- vosulay: `9f9f560` (1.5.3), release build with
  `libvosulay_{lookup,overlay}.so`.
- osu!lazer 2026.726.0, headless under gamescope with vosulay's documented
  `LD_PRELOAD` compositor.
- Session variables: `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, `DISPLAY` and
  `XAUTHORITY`.

`02-grant-caps.sh` gave the daemon binary the capabilities `vosu --setup`
grants, `cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep`, and staged
`chrome-sandbox` as root:root 4755. The receipts come from `evidence2/` unless
noted otherwise. `daemon-capability-receipt.txt` was transcribed verbatim by
Main from the session transcript of the same VM run.

| Criterion | Result |
| --- | --- |
| Supervision | vosu 77973 → vosulay host 77995, a direct child (`process-tree.txt`). |
| Daemon capability status | `getcap` on the daemon binary, read right after the grant, prints `cap_sys_ptrace,cap_sys_resource,cap_perfmon,cap_bpf=ep`. In the `s1-ev.sh` runs, daemon PIDs 68050 and 70843 show UID 1000, `CapPrm`/`CapEff` `000000c001080000`, `NoNewPrivs: 0` and `Seccomp: 0`. That mask is bits 19, 24, 38 and 39 (`CAP_SYS_PTRACE`, `CAP_SYS_RESOURCE`, `CAP_PERFMON`, `CAP_BPF`): exactly the four file capabilities. The supervised vosulay children in those same runs (68073 and 70865) show `CapPrm` 0, `CapEff` 0 and `NoNewPrivs: 0`. `vosu.log` also reports `Process events backend: eBPF`. Source: `daemon-capability-receipt.txt`. |
| vosulay host status | `CapInh`, `CapPrm` and `CapEff` are zero, `NoNewPrivs: 0`, and `CapBnd` is the full default `000001ffffffffff`. The host inherited no daemon capability. Recorded in `evidence/vosulay-status.txt` and in the `CapEff`/`NoNewPrivs` lines of `process-security.txt`. |
| Live CEF sandbox | The renderer zygotes run in fresh PID, user and net namespaces, for example `pid:[4026532715]` and `user:[4026532554]`, against the host's `4026531836`/`4026531837`. Their status shows `NoNewPrivs: 1` and `Seccomp: 2`. Zygote 78056 holds `CapEff 0x200000` (`CAP_SYS_ADMIN`) only inside its own user namespace, as Chromium's namespace sandbox does. |
| Core-only forced death | After SIGKILL of the daemon alone, `kill-verification.txt` reports **0 of 25** surviving. The 25 entries are 13 distinct PIDs: the host plus 12 CEF processes. The script's two `pgrep` patterns matched each CEF PID twice. So 0 of 13 distinct processes survived after 3 s, with zombies excluded. |
| Sandbox mechanism | The kernel allows unprivileged user namespaces, so Chromium used its namespace sandbox. The staged setuid `chrome-sandbox` was not exercised. |

Evidence limits recorded with the PASS:

- The daemon capability readback comes from the `s1-ev.sh` runs, not from the
  `evidence2/` run (daemon 77973) that produced the CEF and forced-death
  receipts. For daemon 77973 itself, the only capability evidence is its
  `Process events backend: eBPF` log line. The binary, file capabilities and
  launch path were the same across those runs.
- No vosu or vosulay artifact hashes were recorded.
- The daemon was a debug build. This is file-capability evidence, not
  packaged-release evidence.
- Session and focus input from vosu was not exercised. vosulay attached through
  its own compositor, and the daemon's lazer offsets were unavailable in this
  debug build.
- Every run shows a `[tokio-rt-worker] <defunct>` child of the daemon. S1 did
  not attribute it.
- The setuid-helper path still needs evidence on a kernel that forbids
  unprivileged user namespaces. Per AF.7, the separately installed vosulay
  runtime owns that provisioning.

### Requirements S1 established and did not implement

- **Elevated daemon on Windows.** When the daemon runs elevated to read
  elevated osu! stable, vosulay's official in-process supervisor must still
  launch the host at Medium integrity. It uses a verified unelevated token for
  the same interactive user and session. If no suitable token exists, the
  launch fails visibly. It never falls back to High integrity and never uses a
  breakaway flag or `--no-sandbox`. S1 ran the daemon at Medium, so the launch
  from an elevated daemon is still unimplemented and unproven.
- **No stdin protocol.** vosulay's `watch_stdin` (`VOSU_TOOL_PROTOCOL=1`)
  only reads to EOF, which it treats as shutdown. Session and focus lines that
  the spike wrote there had no effect.
- **Ecosystem boundary (F/AF.8).** The attached vosulay child reads session
  and focus through its own API-v1 subscription
  (`/api/v1/sessions/subscribe` and the selected session's state), and that
  subscription is the only data authority. The supervisor's bounded private
  IPC carries only lifecycle, backend configuration, editor intent and the
  chosen-session policy. This replaces S1's stdin-forwarding sketch. The
  production supervisor, the Medium launch and the API-v1 conversion belong to
  the vosulay ecosystem owner, not M5.
- **Separate launch policies.** The generic `vosu plugin-host` confinement
  ([plugin ABI §7.3](../v2/plugin-abi.md#73-linux-privilege-drop-and-confinement-before-loading-plugin-code))
  must not be applied to the vosulay child. `no_new_privs` would disable the
  setuid helper. The plugin host's user-namespace denial would disable the
  namespace sandbox that S1 Linux observed.

### Defects in the throwaway S1 scripts

These scripts are throwaway. Fix the following before any rerun; nothing here
is imported into production.

- `01-build.sh` builds `--bin vosu -p vosu-spike-vosulay` in one Cargo
  invocation, which is invalid (`s1-build.log`). Build the daemon and the spike
  crate separately.
- `03-run-and-verify.sh` waits for a `vosulayx-*` process name. vosulay 1.5.3
  re-execs as `vosulay-<hash>`.
- `03-run-and-verify.sh` looks for a `chrome-sandbox` process to prove that the
  sandbox started. No such process exists while the namespace sandbox is
  active, so `sandbox-status.txt` falsely reported that CEF had not started
  even though the zygotes were live. Use the namespace, `NoNewPrivs` and
  `Seccomp` readback instead.
- `s1-ev2.sh` collects PIDs with two overlapping `pgrep` patterns, which
  double-counts the CEF processes (25 entries for 13 PIDs). Deduplicate the
  PIDs before counting.
- A daemon launched over SSH has no session display environment, so vosulay
  exits. Export the session's `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, `DISPLAY`
  and `XAUTHORITY`.
- vosulay sometimes exits a few seconds after `--toggle-edit` in the headless
  setup. Evidence was captured 4 s after the toggle.
- Record `getcap`, the daemon's `/proc/<pid>/status` and the SHA-256 of every
  binary inside the evidence directory of the same run. In the 2026-11-25 run,
  Main transcribed the capability readback separately afterwards, and no
  hashes were taken.
- osu!lazer's Velopack self-update needs root for `/opt/osu`. The pending
  package was moved to `/home/vosu/s1-spike/velopack-held/` for the run and
  then restored.

## M5 test and fixture conventions (A5)

These are the frozen names and artifact rules the M5 units implement. They do
not claim that the tests exist or pass. The contract they test is the
[A5 freeze](../v2/implementation-plan.md#a5-approved-contract-freeze-2026-11-24),
and the D17 names are in the
[benchmark guide](benchmarks.md#m5-d17-contract).

**Native target.** `tests/plugin_host_native.rs` is a thin root that declares
the child modules in `tests/plugin_host_native/`:

- `relay`, `broker`, `lifecycle`, `state`, `resources`, `actions`, `parity`,
  `crash`, `backpressure`, `sync`, `restart` and `approval`;
- the security modules `linux_security`, `windows_job` and `parent_death`;
- `support/`.

**Approval smoke (M5 U0).** `approval` starts a real daemon with P0's
testkit as a `--plugin` process package and drives the dashboard's
`/api/plugins` routes: approve and persist, a denied prompt, revoke, a changed
library refused on restart, near-miss mutations without the dashboard origin
or CSRF token, and a noninteractive start. The daemon is the release build
named by `VOSU_PLUGIN_APPROVAL_DAEMON` (staged by
`python scripts/plugin-host-fixtures.py --release-daemon`, or
`python scripts/vm-test.py --release-daemon` in the Windows VM), otherwise the
debug `vosu`, and each test prints which one it ran. Only a release-daemon run
is the prerelease release evidence. Two cases are `#[ignore]` and run with
`-- --ignored approval_`: `approval_release_daemon_refuses_in_process_without_the_test_harness`
needs `VOSU_PLUGIN_APPROVAL_DAEMON`, and
`approval_stable_release_daemon_refuses_every_plugin` needs
`VOSU_PLUGIN_APPROVAL_STABLE_DAEMON`, a release build whose version has no
prerelease part.

Every test name starts with its module path, so each filter in the M5 plan
(`relay`, `broker`, `lifecycle`, and so on) matches a nonzero set. Main lists
`-- --list` before running. A filter that matches zero tests is a failure.

The following required cases are frozen by name:

- `linux_security::linux_packaged_capability_drop` is `#[ignore]`. It runs
  only after an explicitly authorized `vosu --setup`, against the binary named
  by `VOSU_PLUGIN_HOST_PACKAGED_BINARY`.
- `windows_job::windows_core_death_kills_job_tree` is not ignored, and runs in
  the ordinary native Windows target.

An ordinary run that skips the ignored Linux case does not complete N0.

**N0 packaged Linux drop procedure.** `linux_packaged_capability_drop` and
its controls perform exactly these steps. L0's unprivileged native tests
(`vosu-sys` `plugin_process::linux`, `vosu-core`
`plugin::process::linux`) do not replace any of them.

1. Confirm with `getcap` that `VOSU_PLUGIN_HOST_PACKAGED_BINARY` carries the
   setup capabilities (`cap_sys_ptrace`, plus `cap_bpf`, `cap_perfmon` and
   `cap_sys_resource` in the process-event profile). Start that daemon and
   confirm its own `/proc/<pid>/status` `CapEff` is nonzero. Without both,
   the test fails; it never passes against an uncapable binary.
2. Load P0's testkit process plugin through the approved path (A5.5).
   Identify the plugin host as the daemon's child whose `argv[1]` is
   `plugin-host`, and hold it by a pidfd.
3. Read the host's security state twice: from the testkit constructor probe
   (marker `<probe>-<pid>`, written with the probe's own
   `/proc/self/status` readback before any plugin code beyond the
   constructor runs), and again after the plugin has created its threads.
4. Assert `CapInh`, `CapPrm`, `CapEff` and `CapAmb` are 0, `NoNewPrivs` is 1,
   `Seccomp` is 2, the user-namespace probe fails with `EPERM`, and no UID is
   0. Record `CapBnd` without asserting it. Because the launcher sets
   `no_new_privs` before `execve` (plugin-abi.md §7.3 "Launch"), the host's
   earliest observation already shows `NoNewPrivs: 1` and zero sets.
5. Refusal control: run the daemon in an environment that refuses the
   filter (for example a wrapper seccomp filter that denies `seccomp`).
   Expect a `FAILED` frame with stage `security` and code
   `security_unavailable`, the plugin `failed` and not retried, no
   constructor marker, and the host reaped as its pidfd shows.
6. Core-only death: `SIGKILL` only the daemon, both while the host is live
   and during its bootstrap. The host must be gone as its pidfd shows, never
   judged by PID absence.

The cgroup v2 descendant-cleanup evaluation (plugin-abi.md §7.3
"Descendant cleanup follow-up") is a separate N0 receipt. A plugin host's
own process group is not a qualifying option: it does not survive a
core-only kill, and descendants can leave it with `setsid`.

**Fixture artifacts.** P0 owns two `crate-type = ["cdylib"]` example targets:

- `crates/vosu-plugin-testkit/examples/testkit_process_plugin.rs`;
- `crates/vosu-osu/examples/osu_process_plugin.rs`, which has
  `required-features = ["test-harness"]` and is never part of a release
  build.

Each target has a non-bundled `runtime = "process"` manifest under
`tests/fixtures/plugin_host/{testkit,osu}/`. The testkit's namespace and
action assets live beside its manifest. The osu! fixture stages the bundled
plugin's own settings, namespace and action schema files, and its manifest
differs from the bundled one only in `runtime` and `[targets]`. The bundled
osu! manifest stays target-free and in-process. The osu! fixture keeps the
official `vosu.osu` identity that parity needs. The ordinary development
loader refuses that identity, so loading it for parity needs an approved
official path (X0).

The preparation helper, `scripts/plugin-host-fixtures.py`, builds both
targets with `--message-format=json-render-diagnostics`. It takes the
`cdylib` path from the matching `compiler-artifact` record and never guesses
a file name or a hash suffix. It copies each library to
`<artifact dir>/plugin-host-fixtures/`, so the later `--all-features
--examples` build of the same example cannot replace the bytes under test,
and prints `ARTIFACT name=<example> sha256=<hex> path=<copy>`. It exports two
absolute paths:

- `VOSU_PLUGIN_HOST_TESTKIT_LIBRARY`;
- `VOSU_PLUGIN_HOST_OSU_LIBRARY`.

Run a native test under it, for example
`python scripts/plugin-host-fixtures.py -- cargo test --locked --features test-harness --test plugin_host_native -- --list`.
With `--env-file <path>` it appends the variables in the `$GITHUB_ENV`
format instead. ci.yml runs it before each nextest step, and so do
`prepush.py`'s `test-linux` step, `native-ci.ps1` and `vm-test.py` (whenever
the selected tests can include `plugin_host_native`).

The native target reads these variables and fails, rather than skips, when a
variable is unset or names a missing file. It never runs Cargo.

Each test stages a fresh package directory. It copies the manifest, the assets
and the library into `lib/`, then prints this receipt:

```text
RECEIPT artifact=<name> sha256=<hex> path=<staged path>
```

Native Windows builds the same example targets in the guest after staging.
Linux artifacts are never copied there.

Windows charges two first-time costs to each fresh library, both measured in
the VM.

- **Image evaluation of each never-seen hash.** The VM runs Smart App Control
  in evaluation mode with user-mode code integrity, and it evaluates an image
  on its first load. A fresh 0.8 MB DLL took 0.5–1.1 s to load, even in a
  Defender-excluded directory. A copy of the same hash at another path then
  loaded in about 12 ms.
- **Defender on-access scans in unexcluded directories.** The first read of
  a fresh file took 1.6–3 s for 194 MB and 0.2 s for 0.8 MB, against about
  50 ms excluded. The vm-test work roots (`V:\bt\vmt-*`, temporary directory
  included) are not excluded.

Both costs landed inside the plugin host's 5-second startup deadline. A run's
first wave of parallel tests took 3.3–5.5 s to create and some timed out,
while later creates took about 50 ms. The first-wave times were nearly equal,
because every test waited on the same one-time evaluation of the freshly
built `vosu.exe` on its first execution: a fresh 31 MB executable took about
3 s to start the first time, even in an excluded directory.

Three measures remove these costs from the deadline:

- `support::host` runs `vosu plugin-host` once per test process before the
  first host. It has no descriptors, so it refuses at once with exit 70. This
  was the decisive fix: in three full native runs afterwards, every create
  took 52–117 ms.
- On Windows, `scripts/plugin-host-fixtures.py` loads and frees each staged
  library once (`PRIMED ... first_load_ms=` in its log), so each build's
  library hash is evaluated before any test runs.
- `native-ci.ps1` and `vm-test.ps1` export `VOSU_PLUGIN_HOST_SCRATCH_DIR`, a
  unique directory under the excluded `V:\agent-desktop\tmp`.
  `support::scratch` places each test's packages, receipts and instance data
  there. Unset, it uses the system temporary directory.

**Per-run isolation.** Each test stages its own copy of the testkit library,
and writes a control file, `testkit-fixture.control`, beside that copy. The
library's load-time constructor finds the file through its own module path
before `create`. The file names the run's nonce, its receipt directory, its
fault mode, the trigger opcode, and the range each attached session reads.
Parallel tests therefore never share a control file or an environment
variable. This replaces the earlier per-test `VOSU_PLUGIN_HOST_FIXTURE_DIR`
variable: tests that drive a `PluginHost` in the test process share one
process environment.

- The testkit's constructor, `create`, dispatch, crash, stall and security
  probes append receipts named `<probe>-<pid>` to that directory, plus an
  ordered `journal-<pid>`. Every line carries the run's nonce. A line with
  another nonce fails the test.
- Barriers and acknowledgements use files or the ABI in that directory. A
  blocked dispatch waits for `release-<pid>` holding the nonce. There is no
  global PID-keyed registry and no ordering sleep.
- A "constructor ran" claim needs a marker that names the child PID. A "never
  ran" claim needs the marker's absence and the absence of a leftover PID.

The testkit fault modes are `constructor_crash`, `create_crash`,
`create_block` (`create` waits for `release-<pid>` before it returns),
`dispatch_crash`, `dispatch_abort`, `dispatch_block`, `cancel_ignored`,
`destroy_ignored`, `ring_stall` (the child stops itself: `SIGSTOP`, or
`NtSuspendProcess`), `corrupt_frame` (the child publishes a
length-mismatched frame into its own plugin → host ring and wakes core) and
`write_block` (N0: after `release-<pid>`, `create`'s own thread sends 32 KiB
`VOSU_OP_LOG_RECORD`s until one send stays blocked on the full plugin → host
ring; a watchdog writes `write_blocked` once a send has waited 250 ms).
Distinct data-source targets come from per-package plugin ids and process
names. Each instance reads its own target's bytes through one read plan per
attached session. The `support::smoke` tests prove that each mode triggers
under the production launcher; `restart` uses `create_block` for the
first-load deadline.

**Services mode (I0).** A control file with `mode=services` switches the same
library to `testkit_process_plugin/services.rs`, which uses core's ordinary
services instead of the fault fixture's attach. An owner identifies each
session, declares the control's `root` as its `files` root, and publishes
`/marker`, `/count` and five resource references. Those are a root-relative
file, one reference escaping the root, a two-member tree, and a generated
stream of `stream` bytes plus its `short` variant, which ends at half its
declared size. With `observer=1` the plugin never identifies a session.
The test drives the plugin through its `drive` action: publishing,
publishing a batch core must refuse, republishing references, re-declaring
the root, subscribing, and issuing `ACTION_CALL`s. A `call_block` drive
also blocks dispatch behind the `probe` and `release` barriers. The `work`
action is the callee: it sends progress and then answers, or it holds the
invocation until the `answer` drive or a cancel. Every message the plugin
receives writes a receipt. `support::services` stages each package under
its own manifest, plugin id and process name. It validates each package
against the host's live snapshot, so a `requires` can name a namespace or
action an earlier instance registered. `support::api` serves the production
API v1 router over the test's `PluginHost`. A test without a discovery
service re-offers its target after a restart, as discovery would once the
restarted matcher registers.

Two more N0 receipts come from every run. `security` repeats the
constructor's readback in `create` after the plugin started a thread of its
own. On Linux that readback holds the UID, all five capability sets,
`NoNewPrivs`, `Seccomp`, the thread count and `userns`, the result of
`unshare(CLONE_NEWUSER)` in a forked single-threaded child. On Windows it
holds the process's own immediate job: its limit flags and process IDs.
When the control has a `spawn` line (an argument vector separated by
U+001F), `create` first starts that process as the plugin's descendant and
writes `descendant child=<pid>`.

The `*_crash` modes and `dispatch_abort` all end the child with the real
`std::process::abort`: `SIGABRT` on Linux, the fail-fast status `0xC0000409`
on Windows. `dispatch_abort` also writes an `abort` receipt first. It is the
mode `restart` and `crash` use to prove real crash handling, and
`constructor_crash` and `create_crash` are the real-abort startup cases.

**Closed (C0, 2026-11-25): Windows Error Reporting held crashed plugin
hosts.** P0 found that a real Windows fail-fast was held by Windows Error
Reporting for 3–4 s before the process exited (Application Error 1000, then
WER 1001), so a constructor or `create` crash reached core as a startup
`timeout`, not a `crash`. C0 measured the candidates in the VM:

- without a fix, a real abort was held 1.2–5.6 s and an access violation
  1.3–5.8 s;
- `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION` ended the access violation
  promptly but not the fail-fast, so the job's limits are unchanged;
- the plugin host now adds `SEM_NOGPFAULTERRORBOX | SEM_FAILCRITICALERRORS`
  to its error mode before it maps its channel or loads plugin code
  (`vosu_sys::plugin_process::suppress_crash_reporting_hold`,
  plugin-abi.md §7.4 "Crash reporting"). Both faults then exited within
  about 125 ms.

Evidence on native Windows: `constructor_crash` and `create_crash` report
stage `crash` with exit status `0xC0000409`, and
`crash::crash_a_real_abort_with_a_live_job_and_session_detaches_only_its_own`
asserts that status as an `exited` loss (see "M5 C0 unit evidence").

**Closed for startup-deadline semantics (C0, 2026-11-25): a first load of a
newly installed plugin DLL.** A newly installed or updated plugin DLL has not
been evaluated by Windows, so its first `LoadLibrary` pays the image
evaluation described above inside the startup deadline. P0 measured 0.5–1.1 s
for a never-seen 0.8 MB DLL against about 12 ms for a copy of a seen hash.
Core now gives the first start of each library digest in a core run a
30-second first-load deadline; every later start of that digest, automatic
restarts included, keeps the 5-second deadline (plugin-abi.md §7.6 "First
load", the [spec] decision). The cost is that a genuine `create` hang on a
library's first start is detected after 30 s instead of 5 s.
`restart::restart_a_never_seen_library_gets_the_first_load_deadline` proves
both deadlines on Linux and native Windows with shortened values (2 s and
6 s), holding `create` through `create_block`. The test harness still loads
each staged library once before the tests run, so its own timing stays
independent of the evaluation.

Still open for X0: the first-load time of a realistic release plugin DLL and
of several concurrent first loads were not measured, so the 30-second margin
is not yet proven against them.

**Process identity.** A death proof reads a pidfd, or a process handle and its
exit status. It never relies on PID absence alone.

**Security readback.** Before a constructor runs and again after
plugin-created threads exist, the readback records:

- UID;
- all five capability sets;
- `NoNewPrivs`, `Seccomp` and the result of the user-namespace probe.

**Windows readback.** Token and job identities are read independently of the
code under test.

**Commands (later, by the owning units and Main):**

```sh
cargo build --locked -p vosu-plugin-testkit --example testkit_process_plugin
cargo build --locked -p vosu-osu --example osu_process_plugin --features test-harness
cargo test --locked --features test-harness --test plugin_host_native -- --list
cargo test --locked --features test-harness --test plugin_host_native -- --nocapture
# Packaged Linux, after explicitly authorized setup only:
cargo test --locked --features test-harness --test plugin_host_native \
  linux_security::linux_packaged_capability_drop -- --ignored --nocapture
cargo test --locked --test process_memory -- --nocapture
```

None of these commands were run by A5.

**W0 Windows launch evidence.** These native Windows unit tests run with
`python scripts/vm-test.py --package vosu-sys --lib -- plugin_process` and
`python scripts/vm-test.py --package vosu-core --lib -- plugin::process`.
Their child processes are the test binaries themselves, run again in a helper
role.

- `spawn_assigns_the_strict_job_before_resume_and_inherits_exactly_the_ring_objects`:
  - Before resume, the suspended child's handle table holds exactly the four
    ring objects, each with only the access it needs, at the values named on
    its command line.
  - No handle refers to an inheritable sentinel, the parent process or the
    job.
  - At its first test instruction, the child reads its own immediate job and
    finds it holds only itself.
- The `failure_*` and `missing_program_*` tests inject an assignment failure
  and a resume failure, and trigger a real `CreateProcessW` failure. Each one:
  - proves through an independent handle that the child was terminated with
    the unwind exit code;
  - finds no marker, so the child never ran;
  - finds the parent holding exactly one handle per ring object.
- `nested_job_core_death_kills_plugin_host_and_descendant` builds the tree
  test → core (job O) → plugin host (job I, nested in O) → descendant, and
  terminates only the core process. The plugin host and its descendant exit
  even though the test still holds job O.
- Two negative controls survive the same core-only death for 2 s: one skips
  the assignment, and one leaks a job handle into the child.
- `concurrent_ordinary_spawns_never_inherit_plugin_host_ring_handles` checks
  the handle table of every suspended ordinary child spawned during 64
  plugin-host spawns. With the `vosu_sys::process_spawn` gate removed, the
  first ordinary child inherited a ring handle.

The real-daemon core-only kill remains `windows_job::windows_core_death_kills_job_tree`
(N0).

**Finding (tools follow-up, not fixed in M5).** `src/tools/process.rs`
assigns a tool service to its kill-on-close job only after
`tokio::process::Command` has spawned and started it. The tool therefore runs
unconfined for a short time, and anything it starts in that window is outside
the job. The plugin-host spawn does not have this race: it creates the child
suspended and assigns the job before resume (plugin-abi.md §7.4). Tool spawns
do take the process-wide inheritance gate.

### M5 T1 and B0 unit evidence (2026-11-25)

Recorded by Main from the unit reports. Results from a unit's own checkout,
before integration.

- **T1 (relay).**
  - Linux: `cargo test --locked -p vosu-core --lib plugin::process` passed
    114, and the native `plugin_host_native relay` filter passed 5.
  - Native Windows (`vm-test.py`): `relay` passed 5, and `plugin::process`
    passed 115.
- **B0 (broker).**
  - Each `broker_*` test runs against real x64 and x86 target processes with
    distinct bytes, on Linux and native Windows.
  - Linux: `cargo test --locked -p vosu-core --lib read_plan` passed 75,
    `plugin_host_native broker` passed 5 (and 5 consecutive clean runs), and
    `--test process_memory` passed 8.
  - Native Windows: `read_plan` passed 75, and `broker` plus `relay` passed
    10.
  - The cases cover the B0 counterexamples:
    - a multi-stage chain is one transport submission;
    - foreign handles and raw PIDs are refused;
    - refusals happen before any read and match the in-process runtime;
    - a cancelled ID stays reserved until its execution finishes;
    - an old child's result never completes a new child's waiter.
- **Integrated head.** Workspace nextest through
  `scripts/plugin-host-fixtures.py` passed 2862 (6 skipped).

### M5 C0 unit evidence (2026-11-25)

Results from the C0 checkout (`m5/process`), before integration. Native
Windows runs went through `vm-test.py`; every native run went through
`scripts/plugin-host-fixtures.py`.

- **Core.** `cargo test --locked -p vosu-core --lib plugin::process` passed
  136 on Linux. It covers the restart window (`restart`), the supervisor
  state machine (`supervisor`), and a core write stalled past the 2-second
  window as one `pressure_timeout` loss (`launcher::tests`).
  `host::tests::supervision` and `host::actions::tests::grace` cover the
  host side: restart as a new endpoint, stop or shutdown during the backoff,
  per-instance teardown, and the A5.4 grace.
- **Native `restart`** (4 tests): a real abort chain backs off and then
  latches `failed`, with no sixth start until a manual restart; a teardown
  during the backoff never respawns; teardown destroys one real child and
  keeps another; and the first-load deadline. Restart delays were 1.05,
  2.04, 4.05 and 8.04 s on Linux, and 1.05, 2.04, 4.05 and 8.02 s on
  Windows.
- **Native `crash`** (4 tests, each with a second plugin that must stay
  healthy: same child, no loss, still answering a job):
  - `crash_an_os_kill_is_one_loss_that_restarts_after_the_backoff`: `SIGKILL`
    or `TerminateProcess` of a child with a live job and session. It is one
    `exited` loss with the kill's exit status: the job fails
    `plugin_crashed`, the session's state closes, `destroy` never runs, and
    the restart comes 1.04 s (Linux) or 1.03 s (Windows) after the reap.
  - `crash_a_ring_stall_that_fills_the_ring_is_one_pressure_timeout`: the
    child stops itself and core writes three 768 KiB frames into the real
    2 MiB ring. Two are accepted, the third answers `VOSU_ERR_NOT_READY`,
    and the loss is `pressure_timeout` after 2.02 s on both platforms. The
    stopped child is killed and reaped, then restarted.
  - `crash_an_ignored_cancel_is_killed_when_the_grace_expires`: a job times
    out (`timed_out`, never changed later). The child receives
    `ACTION_CANCEL` for exactly that invocation and ignores it, and is killed
    1.50 s (Linux) or 1.51 s (Windows) after the deadline, with the grace
    set to 1.5 s. The loss is `action_grace_expired`. The callee's other
    running job fails `plugin_crashed`, while another plugin's running job
    and child are untouched.
  - `crash_a_real_abort_with_a_live_job_and_session_detaches_only_its_own`:
    the job's own delivery calls `std::process::abort`. The child is reaped
    292 ms (Linux) or 104 ms (Windows) after the abort, with exit status
    `-6` (`SIGABRT`) or `0xC0000409`. The job fails `plugin_crashed`, the
    session detaches and its state closes, and the next generation attaches
    to the same target under a new handle. The other plugin keeps its
    session and state and gets no detach.
- **WER fix, native Windows.** `support_fault_constructor_crash_*` and
  `support_fault_create_crash_*` report `stage crash` with exit status
  3221226505 (`0xC0000409`), not `timeout`.
- **Counts.**
  - Linux: `plugin_host_native -- lifecycle restart crash::` passed 17.
  - Native Windows: `crash:: restart lifecycle` passed 16 (the Linux-only
    constructor privilege-drop test is not built there), and `broker::
    support_fault` passed 14.
  - Workspace nextest passed 2908 (6 skipped), and `prop_` passed 45.
  - fmt, Linux and xwin Clippy, and every `check-*.py` were clean
    (deleted-assertions against `v2/main`).
- **Integration fix.** B0's
  `broker_an_old_childs_result_never_completes_a_new_childs_waiter` used to
  create instance 1 again by hand after its loss. Under C0 supervision, the
  instance restarts by itself after its backoff, so the test now waits for
  that restart.

### M5 N0 unit evidence (2026-11-25)

Results from the N0 checkout (`m5/process` at `7cdbcc4` plus N0), before
integration. Every native run went through `scripts/plugin-host-fixtures.py`;
native Windows runs went through `vm-test.py`.

**Two kinds of core.** The ordinary cases kill a *core helper process running
production `PluginHost`/`ProcessHostLauncher` code*: the test binary
re-executed in a helper role (`parent_death::helper_process_entry`). It starts
the real `vosu plugin-host` exactly as the daemon would. It is a real core for
the kill semantics, but it is not the daemon. At `7cdbcc4` the daemon installs
no process launcher; M5 U0's approval gate adds that. The `_daemon` variants
run the same assertions against the real `vosu` daemon. They load the package
through `--plugin`, with a `plugins/approvals.json` record written before
start. They are `#[ignore = "needs M5 U0 integrated: …"]` until U0 merges.

Each tree is: core, the plugin host running the testkit, optionally a
descendant the plugin started from `create` (control `spawn`), and an outside
control process. A helper core starts the control itself; a daemon core has
the test start it. Every process is held by a pidfd or a handle opened while
it was known alive and checked against the start time it reported. A death
is read from that pidfd or handle; PID absence is never the evidence.

**Cases.**

- `parent_death` (Linux and native Windows). Each case kills only core.
  - `parent_death_core_kill_while_live_ends_the_plugin_host`: after `READY`.
  - `parent_death_core_kill_during_bootstrap_ends_the_plugin_host`: the kill
    comes while `create` is still blocked (`create_block`). The plugin host
    never reaches `READY`, and `create_released` never appears.
  - `parent_death_core_kill_during_a_blocked_write_ends_the_plugin_host`:
    core is stopped (`SIGSTOP`, or `NtSuspendProcess`), then the flood is
    released (`write_block`). The plugin host fills its 2 MiB plugin → host
    ring, and the test kills core after `write_blocked`. The plugin host
    must be seen gone less than 2 s after the release. The blocked send
    started after the release, so the host's own full-ring exit (exit 75)
    could not come that early. No send returned, and on Windows the exit
    status is not 75.
  - Every case asserts that the outside control is still alive 1 s after
    the plugin host died.
  - On Windows the descendant dies with core. On Linux it outlives core,
    which is the §7.3 residual below; the cases assert that it survives
    and then kill it themselves. On Linux the descendant also reports
    `NoNewPrivs: 1`, `Seccomp: 2` and `CapEff` 0: a plugin's helpers
    inherit the drop and the filter.
- `windows_job::windows_core_death_kills_job_tree` (native Windows, not
  ignored). The same core-only kill of a live tree with a descendant,
  preceded by readbacks the test takes itself:
  - the plugin host's immediate job when the library loaded (the
    constructor's readback): kill-on-close, no breakaway, and only the
    plugin host in it;
  - the descendant's immediate job: the same limits, holding the plugin
    host, the descendant and only processes whose parent is one of them
    (the console host Windows creates for the console descendant). Every
    process in that job is tracked and must end with core.
  - the tokens of core, plugin host and descendant: equal user SID,
    integrity level and elevation;
  - the handle tables of the plugin host and the descendant: no handle of
    the job object type, so neither can keep a job alive.
- `linux_security`:
  - `linux_security_host_readback_before_constructor_and_after_threads`.
    The constructor's readback (`Threads: 1`) and the `security` readback
    taken after the plugin started its own thread both show `CapInh`,
    `CapPrm`, `CapEff` and `CapAmb` 0, `NoNewPrivs` 1, `Seccomp` 2,
    `userns=EPERM`, no UID 0, and `CapBnd` recorded only. Its core has no
    file capabilities, so this is also the no-setup evidence: the sequence
    completes and process plugins load without `vosu --setup` (§7.3
    "Without setup or capabilities").
  - `linux_security_refused_filter_fails_closed_without_constructor`. Core
    runs under a wrapper seccomp filter that hands `seccomp(2)` to the test
    through a user notification. While a call waits for its answer, its
    caller is alive and stopped in the syscall. The test therefore opens a
    pidfd on each refusing plugin host before it answers `EPERM`.
    `create` fails with stage `security`, code `security_unavailable`. After
    2.5 s, past the first restart delay, the instance is `Failed` with 0
    restarts and 0 crash events. No constructor or `create` receipt exists.
    The refused plugin host ended and was reaped, as its pidfd shows, and
    core has no child left but the outside control.
  - `linux_packaged_capability_drop`, `…_refusal_control` and
    `…_core_only_death` are `#[ignore]`d. Together they are the frozen
    packaged steps 1–4, 5 and 6, against the daemon named by
    `VOSU_PLUGIN_HOST_PACKAGED_BINARY`. Each first checks step 1: `getcap`
    shows `cap_sys_ptrace`, with the three process-event capabilities all
    present or all absent, and, except under the refusal wrapper, the
    started daemon's own `CapEff` is nonzero. The refusal control reads
    `GET /api/plugins` (state `failed`, reason `security`, detail
    `security_unavailable`, 0 restarts and 0 crash events after 2.5 s).
    The wrapper's `no_new_privs` leaves that daemon without file
    capabilities, which this control does not need.

**Linux results.**

- `plugin_host_native -- parent_death linux_security support` passed 19,
  with 6 ignored. The whole `plugin_host_native` target passed 46, with 6
  ignored (3 packaged cases and 3 `_daemon` variants).
- The plugin host was seen gone 1–6 ms after the kill in every case.
- In the blocked-write case the 64th 32 KiB record blocked. The plugin host
  was seen gone 267–310 ms after the release.
- The refusal control held the refused plugin host by its pidfd in every
  run.
- `CapBnd` was `000001ffffffffff`.
- `cargo test --locked --test process_memory -- --nocapture` passed 8.
- Workspace nextest through the fixtures script passed 2914, with 12
  skipped, and `prop_` passed 45.

**Native Windows results.**

- `plugin_host_native -- windows_job parent_death support` passed 18, with 4
  ignored. The whole target passed 44, with 4 ignored (the `_daemon`
  variants), and `process_memory` passed 8.
- `windows_core_death_kills_job_tree`:
  - At load, the plugin host's job held only the plugin host.
  - The descendant's job held three processes: the plugin host, the
    descendant and the descendant's console host.
  - Core, plugin host and descendant tokens were equal: Medium integrity
    (`0x2000`), not elevated.
  - Neither the plugin host nor the descendant held a job handle.
  - After the kill of core alone, all three job members ended with exit
    status 0. The plugin host was seen gone 3 ms after the kill, and the
    outside control survived.
- `parent_death`:
  - Kill to plugin host gone: 3–5 ms.
  - Each plugin host and descendant exited with status 0. That is the
    status the job's kill-on-close termination gives, not the plugin host's
    channel-loss exit 75.
  - In the blocked-write case the 64th record blocked, and the plugin host
    was seen gone 273–290 ms after the release.
- The first run failed on two test defects, fixed since:
  - the descendant's job also lists the console host Windows creates for a
    console child;
  - a fixed-size `TokenElevation` query needs its exact length.

**Descendant cleanup evaluation (ABI §7.3 "Descendant cleanup follow-up").**
Measured on the host: CachyOS kernel 7.3.5, cgroup v2, with a running
systemd user manager. The shell ran in a user-owned scope under
`user@1000.service`. `sleep` processes stood in for core, plugin host and
grandchild; each grandchild was started with `setsid`.

1. *Transient user scope bound to core.* Core ran alone in its own
   transient scope: `systemd-run --user --scope -p KillSignal=SIGKILL`. The
   plugin host ran in a second transient scope with `BindsTo=` and `After=`
   on core's scope and `KillSignal=SIGKILL`, and started a `setsid`
   grandchild. After `SIGKILL` of core alone, the host scope was empty
   within 22 ms, the grandchild was gone, and both units were inactive.
2. *Leaving the scope.* Under the same setup, the grandchild wrote its own
   PID into `cgroup.procs` of another cgroup the user owns. It survived the
   core-only kill.
3. *No user manager reachable.* With no session bus and an unusable
   `XDG_RUNTIME_DIR`, `systemd-run --user --scope` failed ("Failed to
   connect to user scope bus via local transport: No such file or
   directory") with exit status 1. That failure is visible.
4. *Delegated leaf without systemd.* A `setsid` grandchild was placed in a
   leaf under the user-owned cgroup. It survived a core-only kill in the
   leaf. `echo 1 > cgroup.kill` then emptied the leaf. Nothing binds a
   leaf's lifetime to core; emptying it needs a living killer.

Assessment:

- Both options need no new privilege and no setup change.
- The delegated leaf fails the third rule: core-only death leaves the
  grandchild alive (case 4).
- The transient scope passes cases 1 and 3, but only under conditions
  outside a per-host leaf:
  - Core must be the only process in its own unit. Started from a terminal
    or a session scope, the daemon shares its unit, so it would first have
    to move itself into a transient scope of its own. Started by
    `vosu.service`, it would move out of the service it runs under.
  - The launcher would need a D-Bus client and a per-spawn
    `StartTransientUnit` round trip, and would have to hold each plugin
    host before plugin code until systemd confirmed the move. That is an
    L0/E0 launch-protocol change.
  - A same-user descendant can still leave the scope (case 2), the defect
    this section already holds against a per-host process group.
  - Not measured: a daemon started in a login session's
    `session-N.scope`, which lies outside `user@1000.service`.

Decision: N0 implements neither option. The gap stays the accepted residual
of §7.3 "Scope": on Linux, core-only death kills the plugin host through
its parent-death signal, and descendants a plugin starts itself survive
unless the plugin arranges otherwise. The Linux `parent_death` cases assert
that survival, so a later mechanism must update them. X2 records the
residual with this receipt. A transient-scope design would need its own
[spec] decision covering core's own unit placement and the launch
handshake.

**Packaged Linux run (pending owner-authorized setup).** Build and package
the release binary exactly as the release workflow does, outside `/tmp`.
File capabilities need a filesystem with xattrs that is not mounted
`nosuid`.

```sh
source scripts/dev-env.sh
export CARGO_TARGET_DIR=/home/user/.cache/vosu-targets/<checkout>
TAG="v$(cargo metadata --no-deps --format-version 1 | python -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "vosu"))')"
PKG=/home/user/.cache/vosu-n0-packaged
python scripts/prepare-offset-generator.py --rid linux-x64
cargo build --release --locked --no-default-features --features linux-process-events \
  --target x86_64-unknown-linux-gnu
python scripts/package-release.py package --platform linux --tag "$TAG" \
  --binary "$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/vosu" --output "$PKG/dist"
python scripts/package-release.py extract --platform linux --tag "$TAG" \
  --archive "$PKG/dist/vosu-linux-$TAG.zip" --destination "$PKG/linux"
# Owner-authorized only: the pkexec prompt, then setcap.
"$PKG/linux/vosu" --setup
getcap "$PKG/linux/vosu"
VOSU_PLUGIN_HOST_PACKAGED_BINARY="$PKG/linux/vosu" \
  python scripts/plugin-host-fixtures.py -- cargo test --locked --features test-harness \
  --test plugin_host_native linux_security::linux_packaged_capability_drop -- --ignored --nocapture
```

The filter matches all three packaged cases, which need U0's approval-gated
`--plugin` launch in the packaged build. Expect `3 passed`. The testkit
library comes from `plugin-host-fixtures.py`'s debug example build, which
the packaged plugin host loads like any user plugin. The daemon variants of
`parent_death` and `windows_job` run after U0 integrates with
`-- --ignored _daemon --nocapture`, on Linux and in the Windows VM.

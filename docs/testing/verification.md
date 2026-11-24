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
- State bus, grants, projection or demand: core/plugin unit tests plus the M2 suites and schema validation below; keep `state_api_compat` unchanged to guard the B1-served 1.x APIs.
- Configuration/counters: affected unit tests plus `cargo test --locked --test config_compat`; cover flat typed TOML parsing, precedence `vosu.toml > vosu.env > tosu.env > defaults`, canonical replacement before legacy removal, malformed startup/reload preservation of last-good state, watcher handling, and atomic writes.
- Process/memory: affected tests plus `cargo test --locked --test process_memory -- --nocapture` on every affected native OS.
- Updater/release: `updater_compat`, native `updater_native`, and `VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture` for package/startup changes.
- Calculation/state: `vosu-pp` tests, `performance_invariants`, `state_bus_invariants`, and `state_api_compat`/`server_compat` when observable schemas or timing can change; for v2 stats, cover parsed/missing maps, modes, mods, clock rate, and catch timing-window semantics. The oracle-backed `live_contracts` and `oracle_parity` tests require the crate’s `oracle-debug` feature. Live parity is optional ([ADR 0011](../decisions/0011-live-parity-optional.md)).
- Compatibility payloads: assert the original `hitErrors`/`hitErrorArray` arrays plus additive sibling `pressHitErrors` and `releaseHitErrors` arrays across v1, stream companion, v2, precise, play, and tournament fixtures; include the lazer mania hold release-correction case.
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
back. Capture/B1 still starts only after acceptance, and B2 settings delivery is
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

Host evidence (2026-11-24): daemon smoke 2/2, API-v1 compatibility 33/33,
plugin host 8/8 and alpha bootstrap 1/1 passed. The affected-crate all-feature
nextest run passed 687 tests (3 pre-existing ignored tests); the retained curl
and required Chromium browser phases passed. All 232 script tests plus 47
subtests, formatting, workspace Linux/Windows cross-Clippy, pinned actionlint
1.7.12 and applicable repository checks passed. Native Windows and all four
live-client combinations below remain **pending**, not inferred from host tests.

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

### Native Windows evidence: pending

**Pending; not run for this outcome.** The VM is busy. V7 neither starts nor
uses it, and no native path/metadata/consumer completion claim is made.
Main will run and record results for these exact host-side commands when the
already-running VM is available:

```sh
taskset -c 0-7,16-23 nice -n 10 python scripts/vm-test.py --package vosu-plugin-api --lib -- manifest
taskset -c 0-7,16-23 nice -n 10 python scripts/vm-test.py --test catalog_v2_compat
```

M6a remains pending native evidence and the main-owned integrated landing gate.
Neither cross-Clippy nor portable platform-policy tests replace native Windows
filesystem, confined asset and manifest/metadata precedence behavior.

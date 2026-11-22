---
type: Plan
title: "vosu 2.0 implementation plan"
description: "Crate layout, milestones, compatibility safety, ecosystem fan-out, build-time goals, and owner decisions for implementing the frozen vosu 2.0 contracts."
---

# vosu 2.0 implementation plan

Status: proposed for owner approval. Inputs, all at `v2/plan` `f07cceb`:
[ADR 0019](../decisions/0019-plugin-runtime-and-trust.md),
[ADR 0020](../decisions/0020-game-agnostic-data-model.md),
[ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md),
[ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md),
[ADR 0023](../decisions/0023-authenticated-catalog-sources.md), the frozen
specs [plugin ABI](plugin-abi.md) with [`include/vosu_plugin.h`](../../include/vosu_plugin.h),
[state schema](state-schema.md) with [`schemas/`](schemas/),
[API v1](api-v1.md), [catalog v2](catalog-v2.md), and
[brokered reads](brokered-reads.md), plus the 1.x tree (`src/`, `crates/`,
`tests/`, [`scripts/module-boundaries.txt`](../../scripts/module-boundaries.txt),
[architecture](../architecture/overview.md), [source map](../architecture/source-map.md)).
`AGENTS.md` is binding for every step below.

## Summary

vosu 2.0 turns the single 1.x daemon crate into a small core plus bundled
plugins (ADR 0019), publishes game state as owned namespaces (ADR 0020),
serves API v1 beside bug-for-bug upstream adapters (ADR 0021), and installs
plugins, tools, and overlays through a capability resolver and catalog v2
(ADR 0022, ADR 0023). The work is sequenced so that every milestone lands on
a long-lived integration branch with the full `prepush.py` gate green and the
1.x observable behavior provably unchanged until a milestone deliberately
replaces it. The first milestone is a behavior-neutral crate split, which
alone addresses the build-time problem ADR 0019 measured. Plugin API and an
in-process host follow, then the state bus, API v1, the adapters rebuilt as
bus projections, the out-of-process plugin host, catalog v2 and the
resolver, the dashboard/overlay/tool hosts on the new model, and finally the
1.x migration and the day-0 release gate. Implementing the specs exposed
nine contract gaps (section 9); eight need an owner decision before the
milestone that first depends on them, and each decision has a
recommendation (section 10).

## 1. Ground rules

- **Integration branch.** All 2.0 work lands on `v2/main` (decision D1).
  1.x fixes keep landing on `main`; `main` is merged into `v2/main` at least
  weekly and before every milestone lands. A milestone lands only through
  `python scripts/prepush.py` evidence for exactly `HEAD` and
  `python scripts/land.py --base v2/main --topic <milestone>` (M−1 adds
  `--base`). `--skip` never reaches `land.py`.
- **Moves are content-identical.** Every file relocation lands as a pure
  `git mv` commit (no edits), followed by a separate edit commit, so that
  `git merge main` keeps following renames for 1.x fixes. `land.py`
  fast-forwards, so both commits survive on `v2/main`.
- **Interim bridges are named and dated.** A milestone that must keep a 1.x
  surface alive across a structural change (for example the 1.x snapshot feed
  in M1–M3) names the bridge, the code that implements it, and the milestone
  that deletes it. No other compatibility shim, alias, or re-export is added
  (`AGENTS.md` clean-cutover rule).
- **Checks keep meaning.** `scripts/check-module-boundaries.py`,
  `scripts/check-file-length.py`, `scripts/ci-changes.py`, and
  `scripts/test_gate_tooling.py` are updated in the same change that
  relocates the code they inspect (details in M0). A relocation that silently
  stops the dashboard smokes or native Windows verification from running is a
  defect.
- **Verification by change** keeps the exact `AGENTS.md` commands. They keep
  working because the integration tests stay in the root `vosu` package,
  which gains the new crates as dependencies (section 5).

## 2. Target workspace layout

The workspace splits along the existing module boundaries (ADR 0019 "Source
layout"). Crate names avoid the upstream project's brand so that no
`scripts/brand-allowlist.txt` entry is needed (decision D10). "Allowed
dependencies" lists workspace crates only; it is recorded in a new
`scripts/crate-boundaries.txt` and enforced from `cargo metadata` (M0).

| Crate | Responsibility | Allowed workspace dependencies |
|---|---|---|
| `crates/vosu-plugin-api` | ABI v1 types mirroring `include/vosu_plugin.h` (`repr(C)`, layout-tested), message codecs (flat hot-path layouts, CBOR), the Rust SDK (`Plugin`, `HostServices`, `Message`, `export_plugin!`, the bundled-plugin registry), `PlanMemory` (synchronous reads over read plans, decision D5), manifest model shared by core and SDK users. Ships the C header unchanged. | none |
| `crates/vosu-sys` (core-owned) | Native boundary: `src/platform*`, `src/process*` (`ProcessMemory`, Linux/Windows backends, scanner), process-event backends `src/instance/events*` with the eBPF build step. All platform `unsafe`. | none |
| `crates/vosu-net` (core-owned) | Hardened HTTPS streaming and archive extraction (`src/net*`), later credential origin scoping and the per-host private-network exception (ADR 0023). | `vosu-sys` |
| `crates/vosu-surface` (core-owned) | Rust-only API for **bundled** surface plugins: exact route and WebSocket-feed registration on a listener role, typed read access to state-bus snapshots, root-confined file responses, management-route provenance. Not part of the C ABI (decision D3). | `vosu-plugin-api` |
| `crates/vosu-core` | Config, logging, report, updater, desktop integration, generic discovery/generations/focus (generalized `src/instance*`), plugin registry and host (in-process dispatch, `vosu plugin-host` supervision and channel), state bus and demand compiler, listeners/origins/auth/API keys/broker/file confinement/WebSocket hub primitives, API v1 (sessions, subscriptions, reads, resources, trees, jobs), catalog v2, sources, credentials, resolver, 1.x migration, lifecycle. | `vosu-sys`, `vosu-net`, `vosu-plugin-api`, `vosu-surface` |
| `crates/vosu-osu-model` | osu! value types and osu!-exact helpers shared by reader, calculator, and adapters: `src/model/{memory,states,enums}*` and the calculation root helpers (`src/calculation/root/{helpers,mods,scores}.rs`: JS-compatible rounding and number writing, mods, grades, legacy hits). | none |
| `crates/vosu-osu-calc` | Live and legacy calculation: `src/calculation/{beatmap,progressive,engine}*` over `crates/vosu-pp` (ADR 0004 contracts unchanged). | `vosu-osu-model`, `vosu-pp` |
| `crates/vosu-osu-reader` | Client decoding: `src/memory*` (stable, lazer, offsets, loader, snapshots), the pointer-width read layer now in `src/process.rs::ProcessHandle`, the embedded `assets/offsets.json` and generator bundle, `src/runtime/offset_generator*`, osu!-specific discovery inputs (client argv parser `StartupArgs`, Wine path resolution `resolve_osu_path`). | `vosu-osu-model`, `vosu-plugin-api`; interim `vosu-sys` until M1 |
| `crates/vosu-osu-state` | Reducer, capture scheduling, gameplay state: `src/state*` plus the calculation backend (`src/calculation/state_backend*`). | `vosu-osu-reader`, `vosu-osu-calc`, `vosu-osu-model` |
| `crates/vosu-osu` | The bundled osu! plugin: manifest, matchers, session identification, namespace projection (`vosu.osu`, common layer, plugin-only subtree D2), roots and resource kinds, actions. | `vosu-plugin-api`, `vosu-osu-state`, `vosu-osu-reader`, `vosu-osu-calc`, `vosu-osu-model` |
| `crates/vosu-adapter-support` | Shared upstream-wire machinery: `src/api/{json,fixed,wire_fixtures}.rs`, legacy filter parsing, JS-number JSON writer. | `vosu-osu-model` |
| `crates/vosu-adapter-upstream` | Upstream v2 adapter: `/json/v2`, `/json/v2/precise`, `/websocket/v2`, `/websocket/v2/precise`, `/files/...`, `/Songs/{path}`, `/backgroundImage`, `/api/calculate/pp`, `/api/ingame` (api-v1.md "Route ownership"). | `vosu-surface`, `vosu-plugin-api`, `vosu-adapter-support`, `vosu-osu-model`, `vosu-osu-calc` |
| `crates/vosu-adapter-gosumemory` | gosuMemory-compatible `/json`, `/ws`. | `vosu-surface`, `vosu-plugin-api`, `vosu-adapter-support`, `vosu-osu-model` |
| `crates/vosu-adapter-streamcompanion` | streamCompanion-compatible `/json/sc`, `/tokens`. | same as gosumemory |
| `crates/vosu-dashboard` | Dashboard pages, server-side render, embedded `assets/server/`, report page, plugin/tool/overlay management UI, `VOSU_DASHBOARD=off`. | `vosu-surface`, `vosu-plugin-api` |
| `crates/vosu-overlay-host` | Overlay hosting: counter discovery/download/settings/profiles (`src/counters*`), the community overlay catalog (`src/server/catalog.rs`), `/websocket/commands` (`src/server/commands*`), overlay static serving. | `vosu-surface`, `vosu-plugin-api`, `vosu-net` |
| `crates/vosu-tool-host` | Web-only tool hosting: static UI serving, launch sessions, bridge, tool settings (`src/tools/mod.rs` minus services). | `vosu-surface`, `vosu-plugin-api` |
| `crates/vosu-proptest` (dev-only) | `vosu_proptest_config()` from `src/proptest_support.rs`, shared by every crate with `prop_*` tests. | none |
| `crates/vosu-plugin-testkit` (test-harness only) | Bundled test plugin with echo/progress/cancel/stream actions and a synthetic game, for API v1, bridge, and plugin-host tests and as the SDK reference example. | `vosu-plugin-api` |
| `.` (`vosu`) | `main.rs`, `vosux` and `plugin-host` argv dispatch, Windows resources, bundled-plugin linkage, integration tests, benches, examples. | every crate above |
| `crates/vosu-pp` and members, `crates/vosu-test-harness`, `crates/vosu-release-smoke` | Unchanged roles (`AGENTS.md` "Source ownership"); the harness depends on the specific crates it drives instead of the old monolith. | unchanged |

Layout rules:

- Plugins (`vosu-osu*`, adapters, hosts) never depend on `vosu-core`. Bundled
  surface plugins reach core only through `vosu-surface` traits that core
  implements; the game plugin reaches core only through the C ABI via
  `vosu-plugin-api`. This is also the build-time property that matters: a
  `vosu-core` edit relinks but does not recompile plugins (section 8).
- Features: the root `linux-process-events` feature forwards to `vosu-sys`;
  `test-harness` forwards to every crate that has harness-only code. Each such
  crate carries its own release guard
  (`#[cfg(all(feature = "test-harness", not(debug_assertions)))] compile_error!`)
  because `build.rs` only guards the root package today.
- `build.rs` splits: the eBPF skeleton (`build.rs::build_linux_process_events`,
  `src/instance/events/bpf/process_events.bpf.c`) moves to `crates/vosu-sys/build.rs`;
  the offset-generator bundle (`build.rs::embed_generator_bundle`) moves to
  `crates/vosu-osu-reader/build.rs`; `embed_windows_resource` stays at the root.
- Embedded assets stay at their paths (`assets/server/`, `assets/offsets.json`)
  so `assets/SHA256SUMS`, `package.metadata.vosu`, and the
  [offsets procedure](../operations/offsets-asset.md) are untouched; crates embed
  them by relative `rust-embed` folders (`../../assets/...`).
- Every crate has `[lints] workspace = true`; crates with `unsafe` keep
  `#![forbid(unsafe_op_in_unsafe_fn)]`. The crate-level
  `#![cfg_attr(test, expect(clippy::disallowed_methods, ...))]` in `src/lib.rs`
  is copied only to crates whose tests actually use a disallowed method;
  elsewhere it would fail as an unfulfilled expectation.

## 3. 1.x module mapping

Line counts include tests (`src/<module>/` plus `src/<module>.rs`). Actions:
**move** (relocate, unchanged behavior), **split** (parts go to different
crates), **rewrite** (replaced by spec-driven code), **delete**.

| 1.x module | Lines | 2.0 home | Action | Milestone |
|---|---|---|---|---|
| `src/platform*` | 2,459 | `vosu-sys` | move | M0 |
| `src/process*` | 5,822 | `vosu-sys`; `ProcessHandle` read layer to `vosu-osu-reader`; `resolve_osu_path` (`src/process/linux.rs`) to `vosu-osu-reader` | split | M0, M1 |
| `src/net*` | 2,377 | `vosu-net` | move; extend for credentials (ADR 0023) | M0, M6 |
| `src/config*` | 2,565 | `vosu-core` | move; drop removed keys (`src/config/schema.rs` lines 145–175, 257–276) and add `API_V1_SUBSCRIPTION_DEBOUNCE`, the dashboard key | M0, M3, M4, M7 |
| `src/logging*` | 981 | `vosu-core` | move | M0 |
| `src/report*` | 1,156 | `vosu-core` (generation); report page to `vosu-dashboard` | move, split | M0, M7 |
| `src/updater*` | 3,979 | `vosu-core` | move; major-version consent first ships on `main` (M−1) | M0 |
| `src/desktop` | 1,023 | `vosu-core` | move | M0 |
| `src/app.rs` | 700 | `StartupArgs` (osu! client argv parser) to `vosu-osu-reader`; startup decisions/guards to `vosu-core` | split | M0 |
| `src/instance*` | 4,479 | generic discovery, generations, PID retention, focus to `vosu-core` (parameterized by manifest `[match]`, plugin-abi.md §6.1); event backends to `vosu-sys`; osu!-specific classification and the lazer offset-resolver lifecycle to `vosu-osu`/`vosu-osu-reader` | split, rewrite | M0, M1 |
| `src/model` | 3,464 | `model/{memory,states,enums}` to `vosu-osu-model`; `model/api/{v1,sc,v2}` to the matching adapter | split | M0, M4 |
| `src/calculation*` | 9,199 | `root/*` helpers to `vosu-osu-model`; `state_backend*` to `vosu-osu-state`; rest to `vosu-osu-calc` | split | M0 |
| `src/memory*` | 21,756 | `vosu-osu-reader` (optionally split stable/lazer later, section 8) | move | M0 |
| `src/state*` | 9,972 | `vosu-osu-state` | move; config input narrowed | M0 |
| `src/api*` | 10,686 | `v1*` to gosumemory adapter, `sc*` to streamCompanion adapter, `v2*` and `precise.rs` to upstream adapter, `json.rs`/`fixed.rs`/`wire_fixtures.rs` to `vosu-adapter-support`, `snapshot.rs` replaced by the core state bus | split, rewrite | M2, M4 |
| `src/server*` | 18,838 | core: `runtime.rs`, `origins.rs`, `auth.rs`, `api_keys.rs`, `broker.rs`, `files.rs` with `files/{directory,validators}.rs`, `websocket.rs` with `upgrade.rs`, `service/{routing,responses,websocket_transport,relay}.rs`, and `routes.rs` rewritten as a registration-based exact matcher; `v2_subscription*` generalized into the API v1 field-subscription hub; `files/skin.rs` split between the osu! plugin's tree lookup and the upstream adapter; `service/providers.rs` to the upstream adapter; `commands*` and `catalog.rs` to `vosu-overlay-host`; `pages.rs`, `service/dashboard.rs` to `vosu-dashboard` | split, rewrite | M1–M7 |
| `src/runtime*` | 11,066 | lifecycle, service, worker, cli, config to `vosu-core` lifecycle; `projection.rs` split into core hub publication and adapter encoders; `render.rs`, `preview_scheme.rs` to `vosu-dashboard`; `offset_generator*` to `vosu-osu-reader` | split | M0–M7 |
| `src/counters*` | 4,444 | `vosu-overlay-host` | move | M7 |
| `src/tools` | 6,744 | `manifest.rs` replaced by `manifest.toml` validation in core; `catalog.rs`, `catalog_sources.rs`, `catalog_art.rs`, `replaces.rs` to the core catalog (catalog v2 only); `mod.rs` UI/session/settings parts to `vosu-tool-host`; `process.rs` job-object/backoff code reused by the plugin-host supervisor, service supervision deleted; `http.rs` proxy deleted (static UI serving kept); `cli.rs` deleted (replaced by `vosux <plugin-id> <action>`) | split, rewrite, delete | M5–M7 |
| `src/vosux.rs` | 607 | root argv dispatch; tool launching replaced by action invocation through the broker credential (api-v1.md "Authorization") | rewrite | M3, M7 |
| `src/assets.rs` | 16 | `ServerAssets` to `vosu-dashboard`; `DaemonAssets` and the generator bundle to `vosu-osu-reader` | split | M0 |
| `src/proptest_support.rs` | 40 | `vosu-proptest` | move | M0 |
| `src/lib.rs`, `src/main.rs` | 66 | root composition | rewrite | M0, M7 |
| `crates/vosu-pp*` | — | unchanged; depended on only by `vosu-osu-calc` | none | — |
| `crates/vosu-test-harness` | — | depends on specific crates | edit | M0 |
| `crates/vosu-release-smoke` | — | extended with 2.0 packaged-binary assertions | edit | M8 |

### Cycles that must be broken

`scripts/module-boundaries.txt` has two strongly connected components. Both
must be acyclic before any crate boundary can cut through them (Cargo rejects
crate cycles). Each break is a behavior-neutral change inside the single
crate (M0, step 1), removing the named rules-file lines.

1. **`calculation` → `state` → `memory` → `calculation`, and `memory` ↔ `instance`.**
   - `memory -> calculation` (rules line 20): readers call
     `js_round_number`, `net_date_binary_to_date`, `mods_checksum`,
     `fix_decimals`, and `osu_mods` (`src/memory/lazer.rs:10`,
     `src/memory/stable.rs:1`, `src/memory/lazer/helpers.rs:115,403`,
     `src/memory/stable/batched.rs:745`, `src/memory/stable/readers.rs:431`),
     all defined in `src/calculation/root/{helpers,mods}.rs`. Fix: move
     `src/calculation/root/{helpers,mods,scores}.rs` into `src/model/` (they
     become `vosu-osu-model`), so readers depend on the model only.
   - `calculation -> state` (rules line 7): `src/calculation/state_backend.rs:18`
     implements the reducer's `StateCalculationBackend`. Fix: move
     `state_backend.rs` and its tests under `src/state/` (state already
     depends on calculation, rules line 59), and delete the source-map
     paragraph that calls this edge intended.
   - `memory -> instance` (rules line 21): `resolve_for_instance`,
     `resolve_for_instance_with_logger`, and `resolve_for_instance_inner`
     (`src/memory/lazer_loader.rs:227-252`) take
     `Arc<crate::instance::Instance>`. Fix: pass a narrow
     `GenerationIdentity` value (PID, generation, cancellation token, cache
     directory) defined in `memory`; `instance` keeps calling the loader
     (rules line 15) until M1 hands the resolver lifecycle to the osu! plugin.
   - `memory -> logging` (rules line 22) is not a cycle but would make the
     osu! reader depend on core: only `src/memory/lazer_loader.rs:15` uses
     `Logger`. Fix: the loader takes a small logging callback, which becomes
     `VOSU_OP_LOG_RECORD` in M1.
   - `state -> config` (rules line 60) is not a cycle either but would make
     the osu! crates depend on core: `src/state/{bundle.rs:137,context.rs:269,readers.rs:142}`
     and `src/state/bundle/reducer.rs:74-80` take `watch::Receiver<Arc<Config>>`.
     Fix: a `ReaderSettings` value built by `src/runtime/` from `Config`.
   - `instance -> app` (rules line 13): `src/instance.rs:20` and
     `src/instance/discovery.rs:16` use `StartupArgs`, which parses the osu!
     client's own argv. Fix: move `StartupArgs` next to discovery (it becomes
     `vosu-osu-reader` input).
2. **`server` ↔ `tools`.**
   - `server -> tools` (rules line 58): `src/server/routes.rs:790`
     (`tools::manifest::valid_id`) and `src/server/service/routing.rs:6,23`
     (`Arc<ToolHost>`). Fix: `server` defines a `RouteExtension` trait for the
     reserved `/tools` prefix and the dashboard tool routes; `ToolHost`
     implements it; `src/runtime/` wires it; `valid_id` moves to a small
     `ids` module owned by `server`. This trait is the seed of `vosu-surface`.
   - `tools -> server` (rules line 66) remains, pointing at core primitives
     (`server::files`, `server::websocket` limits, `server::origins`,
     `server::routes::PeerAddress`, `server::broker::BrokerToken`).

`check-module-boundaries.py` additionally rejects a cyclic rules file (an SCC
check over the recorded edges), so a cycle cannot return.

## 4. Milestones

```text
M−1  main: 1.x consent release; v2/main branch infrastructure
 └─ M0   crate split, no behavior change
     └─ M1   plugin API + in-process host + osu! plugin behind 1.x surfaces
         └─ M2   state bus and vosu.* namespaces
             ├─ M3   API v1, resources, jobs, bridge
             │   ├─ M4a  adapters as bus projections
             │   │   └─ M4b  live golden fixtures (owner capture)
             │   └─ M5   plugin-host process (Linux, then Windows + measurement)
             └─ M6a  manifest.toml + catalog-v2.json validation
                 └─ M6b  sources, credentials, resolver
 F    ecosystem fan-out: after M3 + M6a and v2.0.0-alpha.1
 M7   dashboard, overlay host, tool host on the new model; /api/ingame  (needs M3, M5, M6b)
 M8   1.x migration, release smoke, day-0 gate, release              (needs all, M4b included)
```

M3 and M6a proceed in parallel after M2. M5 follows M3 because its exit
runs the full opcode set, actions and resources included, out of process.
M4a needs M3's root/tree machinery only for file routes, so its
JSON/WebSocket projection may start in parallel with M3; M4a and M5 are
independent of each other. M7 needs M3, M5, and M6b. M8 needs everything,
M4b included.

Every milestone's exit criteria include the **common gate**: `prepush.py`
green for `HEAD` (formatting, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
`cargo test --workspace --lib --bins --tests --examples --all-features --locked`
including doctests, repository checks and `scripts/test_*.py`, Windows MSVC
cross-Clippy, native Windows Clippy/tests/doctests through
`scripts/native-ci.ps1`, and the dashboard smokes when UI paths changed),
then hosted CI through `land.py`. Only the milestone-specific commands are
listed below.

### M−1: transition safety on `main` and integration-branch infrastructure

Scope:

- **1.x major-version consent (ships on `main` as 1.3.0, as early as
  possible).** ADR 0022: it protects only installations that receive it before
  2.0 is published. `parse_latest_release` (`src/updater/release.rs:26-56`)
  returns a new `ReleaseDecision::NeedsConsent` when the candidate's major
  version exceeds the running one; automatic updates stop there; the
  dashboard update UI asks; `RunUpdates` accepts an explicit consent flag.
- **Integration branch.** `scripts/land.py` gains `--base <branch>`
  (default `main`): `ensure_fast_forward_of_main` (lines 83–93) and
  `fast_forward_main` (lines 144–145) take the base. Branch protection on
  `v2/main` requires `CI required`. `ci.yml` needs no trigger change
  (`ci/**` already runs); `nightly.yml` gains a `v2/main` checkout so random
  proptests and the lazer canary also run there. `scripts/test_gate_tooling.py`
  covers `--base`.
- **Prerelease publishing.** `scripts/sign-release.py --publish` (lines
  120–122) publishes drafts with only `draft=false`; for a semver
  prerelease tag it must also set `prerelease=true` and `make_latest=false`,
  so `v2.0.0-alpha.N` tags never become the repository's latest release. 1.x
  already ignores prereleases (`src/updater/release.rs:35-51`).

Exit: `cargo test --locked --test updater_compat`; native
`cargo test --locked --test updater_native -- --nocapture` on Linux and
Windows; new unit tests for `NeedsConsent` beside `src/updater/tests_release.rs`;
the dashboard smokes (update UI changed); `scripts/test_gate_tooling.py`
(run by `prepush.py`) covering `--base`; the first milestone landed through
`land.py --base v2/main`.

Risks: users who skip 1.3.0 still update to 2.0 silently (accepted by ADR
0022); keep 1.3.0 small so it ships before M0 starts.

### M0: crate split without behavior change

Scope, in three landable steps:

1. **Cycle breaks** inside the single crate (section 3), with
   `scripts/module-boundaries.txt`, [source map](../architecture/source-map.md),
   and the cyclic-rules check updated together.
2. **Extraction** into `vosu-sys`, `vosu-net`, `vosu-core` (config, logging,
   report, updater, desktop, startup decisions), `vosu-osu-model`,
   `vosu-osu-calc`, `vosu-osu-reader`, `vosu-osu-state`, and `vosu-proptest`.
   `api`, `server`, `runtime`, `instance`, `tools`, `counters`, and `vosux`
   stay in the root package for now: they still depend on osu!-typed state,
   which core must not. Integration tests, benches, and examples stay in the
   root package and switch imports from `vosu::<module>` to the new crates.
3. **Tooling** in the same change as the moves:
   - `scripts/check-module-boundaries.py` runs per crate (rules files under
     `scripts/module-boundaries/<crate>.txt`) and checks
     `scripts/crate-boundaries.txt` against `cargo metadata --no-deps`.
   - `scripts/file-length-baseline.txt` entries are renamed to the new paths
     with identical counts (a reviewed edit; `--update` refuses additions).
   - `scripts/ci-changes.py`: `UI_PREFIXES` and the `render*` rule cover the
     relocated dashboard/tool/server paths, and the schema-only exemption
     list (`src/api/`, `src/model/`, `src/calculation/`) names the new crate
     paths; otherwise UI smokes and native verification silently stop.
   - `AGENTS.md` "Source ownership", the property-test rule
     (`vosu_proptest::vosu_proptest_config()`), [source map](../architecture/source-map.md),
     and [overview](../architecture/overview.md) "Deployment variants".
   - A build-time measurement script and its first results (section 8).

Exit (all from `AGENTS.md` "Verification by change", since every area moves):
`cargo test --locked --test state_api_compat`;
`cargo test --locked --test server_compat`; `cargo test --locked --test daemon_smoke`;
`cargo test --locked --test config_compat`; `cargo test --locked --test performance_invariants`;
`cargo test --locked --test process_memory -- --nocapture` on Linux and Windows;
`cargo test --locked --test updater_compat` and native `updater_native`;
`cargo test --workspace --lib --all-features --locked prop_`;
both native release builds; `VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture`;
both dashboard smokes against a `--features test-harness` build;
`python scripts/check-module-boundaries.py`, `check-file-length.py`,
`check-asset-hashes.py`; `cargo bench --locked --package vosu --bench hot_paths -- serializers_cold`
(also `raw_snapshot_reads`, `publication`, `v2_subscription`) compared
like-for-like with the pre-split commit: no difference beyond run-to-run noise.

Risks: lost cross-crate inlining in debug and test builds (release keeps thin
LTO and one codegen unit, `Cargo.toml` `[profile.release]`); `pub(crate)`
items and `#[cfg(test)]` helpers used across new crate boundaries (for
example `src/runtime/test_support.rs`) need explicit `pub` or a
`test-harness`-gated module; a long-lived diff against `main` (mitigation:
content-identical move commits, land M0 within days, pause large 1.x
refactors meanwhile).

### M1: plugin API, in-process host, osu! plugin behind the 1.x surfaces

Scope:

- `vosu-plugin-api`: plugin-abi.md §1–§5 and §9; layout tests asserting every
  size and offset in the §3 table with `core::mem::offset_of!`, and a C
  translation unit with `_Static_assert`s on the same table compiled from a
  test, so header and Rust cannot drift; CBOR codecs over `ciborium`; flat
  payload validation (offsets inside the buffer, 64 KiB
  `VOSU_READ_PLAN_MAX_BYTES`, alignment); `export_plugin!` producing
  `vosu_plugin_entry_v1` for `cdylib` builds or a registry entry under the
  `bundled` feature; `PlanMemory`.
- Core plugin host: bundled registry iteration and external `dlopen`
  (`libloading`); manifest `kind = "plugin"` runtime fields (plugin-abi.md
  §8) for bundled and development plugins (`--plugin <dir>`, the analog of
  1.x `--tool <dir>`, trusted, development builds only until M6 sources
  exist); dispatch thread per instance and the `concurrent_dispatch` pool
  (§4); error model (§5); matching (§6.1) with overlap refusal; session
  lifecycle, identification, the 5-second hidden rule, focus rule and
  notifications (§6.2, api-v1.md "Focus rule"); read-plan executor (§6.3)
  over `vosu-sys`; scanning (§6.4) over `src/process/scanner.rs`; settings
  (§6.9); logging with the 200/s limit (§6.10); timers (§6.11).
- Discovery moves into core, parameterized by enabled plugins' matchers. The
  process-event backends filter exec events by hard-coded osu! names today
  (`candidate_comm`, `src/instance/events.rs:150-155`); the filter becomes
  the union of enabled plugins' `process_names` (truncated to the kernel's
  15-byte `comm`), and a plugin declaring path or command-line patterns
  widens it to unfiltered exec events plus the existing periodic
  reconciliation. `vosu-surface` is created here for bridge B1.
- `vosu-osu`: matchers reproducing `src/instance/discovery.rs` (lines 44–110
  and `should_skip`, line 118) or decision D6; identification; the lazer
  offset-resolver lifecycle moved from `src/instance.rs` into the plugin on
  `SESSION_ATTACHED`; reads through `PlanMemory`, so the 1.x readers stay
  unchanged; `concurrent_dispatch = true` with the stress test that
  plugin-abi.md §10 requires.
- **Interim bridge B1** (deleted in M4a): the bundled osu! plugin hands each
  `StateBundleSnapshot` to the existing `SnapshotManager`
  (`src/api/snapshot.rs`) through a `vosu-surface` hook, so every 1.x route
  keeps serving unchanged bytes.

Exit: `cargo test --locked -p vosu-plugin-api` (layout, codecs, `prop_*`
bounds tests); `cargo test --locked -p vosu-core` host tests (trust refusal of
`in-process` for an ineligible plugin, matcher overlap, identification
timeout, focus delivery order, read-plan validation including a dependent
stage whose `ref_op` length differs from the pointer width, log rate limiting,
timer cancellation); a new `tests/plugin_host.rs` that builds the testkit as a
`cdylib` and loads it in-process on Linux and Windows; all M0 exit tests,
unchanged; `cargo test --locked --test process_memory -- --nocapture` on both
OSes; `cargo bench --locked --package vosu --bench hot_paths -- raw_snapshot_reads`,
`cargo bench --locked --package vosu --bench memory_reader_bench`, and the
`live_reader_bench` example compared with M0. Threshold proposed for owner
sign-off: no more than 10% p50 regression on raw snapshot reads.

Risks: the 1.x readers issue many sequential reads; one plan per read adds a
message round trip each. D5 keeps that round trip on the calling thread; if the
benchmark still misses the threshold, convert the hottest chains (the lazer
precise chain named in brokered-reads.md "Plan shapes") to dependent multi-stage
plans before M2. Discovery parity (D6).

### M2: state bus and `vosu.*` namespaces

Scope:

- Core state bus: per-session immutable namespace documents published
  latest-only (ADR 0002), typed values (`vosu_value_kind`), batch commit (D4),
  coverage enforcement (`covers`, reserved-name `[[resource_kinds]]`,
  plugin-abi.md §6.6; undeclared publication refused), `null` semantics and
  namespace omission (state-schema.md "Namespaces"), `vosu.client` filled by
  core from attach and `SESSION_IDENTIFY` (state-schema.md "`vosu.client`").
- Schema registry: `docs/v2/schemas/*.schema.json` embedded with
  `include_str!` (core namespaces in `vosu-core`, `vosu.osu` in `vosu-osu`)
  as the single source; the `x-sampling-lane` demand compiler generalizing
  `V2SubscriptionHub` demand (`src/server/v2_subscription.rs`,
  state-schema.md "Sampling lanes"); `DEMAND_CHANGED` snapshots (§6.7);
  plugin `STATE_SUBSCRIBE`/`STATE_UPDATE` and `vosu.sessions` events (§6.12,
  §6.2); typed snapshot reads for bundled consumers through `vosu-surface`.
- `vosu-osu` projects each `StateBundleSnapshot` onto `vosu.status`,
  `vosu.chart`, `vosu.play`, `vosu.skin`, `vosu.osu`, and the plugin-only
  subtree (D2), publishing only changed leaves per batch, following
  state-schema.md "1.x-to-2.0 field mapping", "`keys`", "`skin`", and
  "Common-layer coverage". Resource references arrive in M3; until then the
  manifest declares no `[[resource_kinds]]`, so `vosu.chart.resources` and
  `vosu.skin.tree` are correctly absent.
- Bridge B1 still feeds the 1.x surfaces; reader demand is the union of the
  1.x hub's demand and the bus demand.

Exit: new `tests/state_bus_compat.rs`: for every case in
`tests/fixtures/oracle/inventory.json` (its `required_coverage` list,
`tests/state_api_compat.rs:46-74`), the published documents validate against
their schemas and every mapped value equals the 1.x `/json/v2` or precise
value at its 1.x path; plugin-only pointers never appear in any API-facing
document. `cargo test --locked --test state_api_compat` and
`--test server_compat` unchanged; `--test performance_invariants`; a new
`state_bus` Criterion group (batch publish at regular and precise cadence,
hit-error arrays of 0, 1,000, and 5,000 entries) compared with the
`publication` group from M1.

Risks: whole-array republication of growing hit-error arrays (§6.6 publishes
arrays only as whole CBOR subtrees) may cost more than 1.x's shared vectors;
if the bench shows it, add an append form as an additive ABI revision.
Tournament clients: `vosu.osu.tourney.clients[]` aggregates processes that
1.x selects as one instance (`src/instance.rs`); how they appear as sessions
is decision D6.

### M3: API v1, resources, jobs, bridge

Scope:

- Routes exactly per [API v1](api-v1.md): "Sessions" with "Focus rule" and
  "Visibility"; "Session events"; "Field subscriptions" (generalizing
  `src/server/v2_subscription.rs`, a separate hub with
  `API_V1_SUBSCRIPTION_DEBOUNCE`); "HTTP state reads"; "Status codes and
  error bodies"; "Resources and trees" with "Id registry" (HMAC under a
  persisted core secret, bounded registry) and "Headers and type allowlist";
  the `307` convenience routes; "Actions" ("Create a job", "Job object",
  `/api/v1/jobs/{id}`, "Authorization"); "Listener placement, CORS, and
  limits" (overlay listener only).
- Plugin side: `ROOT_DECLARE`, `RESOURCE_RESOLVE`, `TREE_LIST`,
  `TREE_LOOKUP`, `STREAM_*` (§6.5) over ADR 0005 confinement in
  `src/server/files.rs`; `ACTION_*` (§6.8) including relay and id remapping;
  timeouts producing `timed_out`. The osu! plugin declares `songs`, `skins`,
  and lazer file-store roots and the `file`, `audio`, `background`, `set`,
  and `skin` kinds, and moves lazer skin lookup from `src/server/files/skin.rs`
  into its `TREE_LOOKUP`.
- Bridge: `assets/server/tool-bridge.js` gains `vosu.invoke` and
  `vosu.start` exactly as api-v1.md "Authorization" defines, and an API v1
  `vosu.subscribe` (pointers such as `/vosu.play/combo`, `session` option);
  served at `/tools/_host/v2/bridge.js` (D12). The 1.x tool host and its
  launch sessions remain the source of `X-Vosu-Tool-Session` until M7.
- CLI: `vosux <plugin-id> <action> [json-args]` with `--detach` and
  `--session` through the broker credential. 1.x tool ids cannot contain a
  dot (`src/tools/manifest.rs:82-89`) and plugin ids always do, so both forms
  coexist until M7 removes the tool form.

Exit: new `tests/api_v1_compat.rs` (every route, status, error body, cache
header, `sandbox`/`nosniff`, type allowlist, range handling, CORS refusal of
`X-Vosu-Tool-Session` and `Authorization`, dashboard-listener refusal,
subscription frames, removed-index absence, 1 MiB limits, close `1011` on a
full pong queue, job states including `plugin_crashed`/`timed_out`);
`cargo test --workspace --lib --all-features --locked prop_` with new
`prop_*` tests for API v1 pointer parsing, resource-id parsing, and tree path
validation; `cargo test --locked --test server_compat` unchanged;
`cargo test --locked --test daemon_smoke` extended to read
`/api/v1/sessions` and fetch a resource; a browser test driving `vosu.invoke`,
progress, and `AbortSignal` cancellation against the testkit, run by
`python tests/tool_host_smoke.py`; a Criterion comparison of the API v1 hub
with `v2_subscription`.

Risks: resource-id stability across restarts depends on the content identity
the osu! plugin chooses (lazer store hash, stable path/size/mtime); the
registry must stay bounded under churn of map selection.

### M4a: upstream adapters as projections

Scope:

- Create the three adapter crates and `vosu-adapter-support`; move the
  builders (`src/api/{v1,sc,v2}*`, `src/api/precise.rs`,
  `src/model/api/*`), the HTTP providers (`src/server/service/providers.rs`,
  including ADR 0004's `LegacyCalculateProvider`), the stable
  `/files/skin/{path}` behavior, and the WebSocket encoders from
  `src/runtime/projection.rs`. Builders take a typed view assembled from the
  bus (`vosu.osu`, `vosu.client`, plugin-only subtree D2) instead of
  `RegularState`/`PreciseState`; core's hub keeps latest-only publication and
  per-snapshot encoding caches (`tests/performance_invariants.rs:509-701`).
- Remove what ADR 0021 removes: `pressHitErrors`, `releaseHitErrors`,
  `keyHitErrors`, `keys.all`, `catcher`, `hitOffsets`, the `/files/skin/`
  index, `/websocket/v2/subscribe`; lazer `/files/skin/{path}` returns 404;
  the removed configuration keys (`ENABLE_KEY_OVERLAY`,
  `ENABLE_MANIA_KEY_HIT_ERRORS`, `ENABLE_HIT_OFFSETS`,
  `V2_SUBSCRIPTION_DEBOUNCE`) leave the schema and enter the M8 migration
  list. Delete bridge B1 and `src/api/snapshot.rs`.
- The divergence file next to the fixtures (`tests/fixtures/upstream/DIVERGENCES.md`)
  lists every security divergence
  with its reason (sandbox header, path confinement, peer and Origin policy,
  request limits, ADR 0004 endpoint, `/api/ingame` functional compatibility).

Exit: `cargo test --locked --test state_api_compat`, rewritten to drive the
adapters from bus input and compared byte for byte with the committed source
oracle; `strip_vosu_extensions` (`tests/state_api_compat.rs:198`) is deleted
because the adapters no longer emit those fields;
`cargo test --locked --test server_compat` updated in the same change for each
intentional removal; `--test performance_invariants` ported to the adapters;
`--test daemon_smoke`; `cargo bench --locked --package vosu --bench hot_paths -- serializers_cold`,
`serialization_cache`, `publication`, and `--bench autoresearch_http_v2`
against M3, within the owner-approved threshold.

Risks: the adapters lose data the 1.x builders read directly from typed state
unless D2 lands; per-frame projection from the bus may cost more than 1.x
encoding from typed snapshots (measured above).

### M4b: live golden fixtures (owner-dependent)

Scope: extend `tools/live-parity` with (1) a route-probe matrix recording
status, headers, and error bodies, including near-miss routes; (2) a
vosu-side bus dump per sample through a `test-harness`-only endpoint so
captures can be replayed; (3) a `golden` command that normalizes volatile
values and writes fixtures under `tests/fixtures/upstream/<pinned revision>/`.
Then the **owner** runs captures against the pinned upstream revision
(`Cargo.toml` `source-commit`) on every supported pair in
`tools/live-parity/live_parity.py` `SUPPORTED_MATRIX` (Windows stable,
Windows lazer, Linux lazer), traversing menu → play → paused → result. A new
`tests/adapter_golden.rs` replays the bus dumps into the adapters and compares
every route and frame with the upstream capture, minus the divergence file.

Exit: `cargo test --locked --test adapter_golden`; the fixture inventory names
its revision and is covered by `scripts/brand-allowlist.txt` entries in
the `parity`/`fixture` categories where fixtures spell the upstream name.

Blocking: M8's release gate blocks on M4b. Nothing else does.

### M5: plugin-host process, capability drop, brokered reads

Scope:

- `vosu plugin-host` argv dispatch in `src/main.rs` beside `vosux`; one
  process per plugin instance; the shared-memory rings, framing, and 2-second
  backpressure window (§7.2); crash detection and 1/2/4/8/16-second restart
  backoff with the five-in-five-minutes `failed` state; read plans relayed
  byte for byte; bounded streams for generated resources.
- Linux: the §7.3 sequence before `dlopen` on the single initial thread.
  `vosu --setup` grants file capabilities (`src/platform/linux/capabilities.rs`),
  so the exec of `vosu plugin-host` regains them and must drop them first.
- Windows: `CREATE_SUSPENDED`, job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` before resume (§7.4), reusing
  `src/tools/process.rs:59-70`. Then the measurement brokered-reads.md "Open
  questions" requires: port `benches/brokered_reads_bench.rs` to a Windows
  helper (unnamed section + event versus named pipe), run it pinned on the
  native Windows guest, record results in [brokered reads](brokered-reads.md)
  and the [benchmark guide](../testing/benchmarks.md), and adopt the faster
  transport for Windows.

Exit: new `tests/plugin_host_native.rs` run with `-- --nocapture` on Linux and
Windows: the testkit and the osu! plugin run under `runtime = "process"`;
read plans against another session's handle return
`VOSU_ERR_PERMISSION_DENIED`; killing the plugin host detaches its sessions
with `reason: "lost"` and restarts with backoff; a stalled ring trips the
2-second window. On Linux an `#[ignore]` test, run natively after
`vosu --setup` on a packaged build, asserts that the plugin host's
`/proc/<pid>/status` shows `CapInh`, `CapPrm`, `CapEff`, `CapBnd`, and
`CapAmb` all zero and `NoNewPrivs: 1`; on Windows, terminating core
terminates the plugin host. `cargo bench --locked --bench brokered_reads_bench`
with the production transport stays within 2x of the recorded prototype
numbers; `cargo test --locked --test process_memory -- --nocapture` on both
OSes.

Risks: shared-memory setup and teardown robustness against a crashed host
(brokered-reads.md "Recommendation" keeps a socket fallback acceptable);
measured stage counts of the real regular capture path versus the bench
estimate (brokered-reads.md "Open questions").

### M6a: `manifest.toml` and `catalog-v2.json` validation

Scope: the unified manifest (catalog-v2.md "Package manifest v2", "Shared
fields", the three kinds, "Capability fields", "Capability coverage",
"Namespace and kind parsing", "Game capabilities"); `metadata.txt` as
read-only legacy overlay input; `catalog-v2.json` parsing and validation;
committed JSON Schemas for both documents under `docs/v2/schemas/` so
ecosystem packaging scripts can validate without vosu; the namespace-schema
declaration in `[[provides]]` (D7). Validation lives in `vosu-plugin-api` so
plugin authors and core use one implementation.

Exit: `cargo test --locked -p vosu-plugin-api manifest` and a corpus test
`tests/catalog_v2_compat.rs` with valid and invalid manifests for every rule in
catalog-v2.md, including reserved names, id prefix rules, and
`provides`/`[[actions]]` pairing.

### M6b: sources, credentials, resolver, private sources

Scope: catalog-v2.md "Sources and credentials" ("Source forms", "Forge
detection", "Per-forge private asset retrieval", "Credentials", "Origin
scoping on redirects", "Private networks", "Trust") on top of the manual
redirect loop in `src/net/streaming.rs`; "Resolver algorithm" (built-in
providers, provider selection, version solving, install plan with
`needs_approval` and in-process plan errors, dependency-only autoremove, game
detection); "API placement" (dashboard listener, CSRF); plugin trust tiers
complete §7.1 and the approval flow. `AGENTS.md`'s private-destination rule
is amended with ADR 0023's per-host exception in the same change.

Exit: `tests/catalog_v2_compat.rs` resolver scenarios;
`cargo test --locked -p vosu-test-harness --test network_integration`
extended with credential stripping on every cross-origin redirect, the
GitHub API asset redirect to presigned storage, and the private-network
exception limited to the exact source host; `cargo test --workspace --lib --all-features --locked prop_`
(ZIP preflight unchanged); `python tests/tool_catalog_smoke.py` and
`python tests/catalog_art_smoke.py` on catalog v2; `credentials` never
returned by any route (server_compat-style test).
**Owner-dependent:** ADR 0023 requires forge-specific retrieval to be
verified against each forge. The owner provides read-only tokens for private
test repositories on GitHub, GitLab (nested group), Codeberg, and one Gitea or
Forgejo instance; the results are recorded in
[verification](../testing/verification.md).

### M7: dashboard, overlay host, tool host on the new model

Scope:

- `vosu-dashboard`, `vosu-overlay-host`, `vosu-tool-host` become bundled
  surface plugins; core's router is registration-based and still exact
  (unknown and near-miss routes stay 404, `AGENTS.md`).
- Tools are web-only: delete service supervision, the `/tools/<id>/api/*`
  proxy, readiness, per-launch service tokens, the host API
  (`/tools/_host/v1/{self,settings,context,overlays}`), and the tool CLI
  ([tool protocol](../tools/protocol.md) "Process contract", "Host API",
  "Command-line tools"); tool sessions scope to the resolved
  `requires`/`optional` (api-v1.md "Authorization"); remove the 1.x `vosux`
  tool form. Replace [tool protocol](../tools/protocol.md) with the 2.0 tool
  protocol.
- Dashboard: plugin management (installed, approvals, settings from
  `settings_schema`, the verbatim trust warning in catalog-v2.md "Trust"),
  "Add support for `<name>`" from game detection, install plans, the
  `/api/ingame` deprecation banner with one-click vosulay install,
  `VOSU_DASHBOARD=off` and its configuration key (ADR 0019).
- `/api/ingame` clean-room implementation in the upstream adapter (section 6).
- Core lifecycle absorbs the rest of `src/runtime/`; the root package is
  `main.rs` plus bundled linkage, with a startup assertion that the registry
  contains exactly the expected bundled set (a dependency that is never
  referenced is not linked, so every bundled crate is named with `use … as _`).

Exit: rewritten `python tests/tool_host_smoke.py --binary target/debug/vosu`
(launch, bridge, invoke, progress, cancel, 401/403 rules) and
`python tests/dashboard_smoke.py --binary target/debug/vosu --playwright-module <playwright index.mjs>`
against a `cargo build --locked --features test-harness` binary;
`python tests/catalog_art_smoke.py`; `cargo test --locked --test server_compat`
(origins, CSRF, API keys, counters); `cargo test --locked --test config_compat`;
`cargo test --locked --test daemon_smoke` including `VOSU_DASHBOARD=off`; a
new `tests/ingame_compat.rs` derived only from the behavioral specification.

### M8: 1.x migration and day-0 release gate

Scope: first-start migration exactly per catalog-v2.md "First-start migration
from 1.x" (sources verbatim with `trusted: false` except official identities;
official tools queued through `replaces`; user tools disabled with
`needs-2.0-update`; overlays untouched) plus removed configuration keys,
idempotent like `src/config/migration.rs`; the day-0 check (catalog-v2.md
"Day-0 release check") as `scripts/check-day0.py` wired into `release.yml`
and covered by `scripts/test_gate_tooling.py`; README, quickstart, CHANGELOG,
architecture, and ADR statuses; packaging (section 7).

Exit: new `tests/migration_compat.rs` over 1.x config/data fixtures for Linux
and Windows path layouts, run twice to prove idempotence;
`cargo test --locked --test config_compat`;
`VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture`
extended to start `vosu plugin-host` from the packaged binary, read
`/api/v1/sessions`, and confirm the bundled set; native updater tests;
`python scripts/check-day0.py` passing against the four repositories; M4b
complete; an owner-run live session on Windows stable, Windows lazer, and
Linux lazer through menu → play → paused → result with one official tool, one
overlay, and vosulay installed through the resolver.

## 5. Compatibility safety during the rewrite

The 1.x behavior stays verifiable because the integration tests never move
out of the root package and never change except in the milestone that
deliberately replaces the behavior they pin. Every Rust test's imports
change once in M0; that is the only edit before the milestone listed.

| Test | Pins | Stays green unchanged through | Replaced or rewritten in |
|---|---|---|---|
| `tests/state_api_compat.rs` | wire output of all four schemas against the committed source oracle | M3 | M4a: drives the adapters from bus input; exact equality without `strip_vosu_extensions`; its `/websocket/v2/subscribe` test is deleted with the route, its coverage already provided by `tests/api_v1_compat.rs` (M3) |
| `tests/server_compat.rs` (+ `server_compat/listener.rs`) | routes, methods, CORS, body limits, auth matrix, file security, pages, counters, catalog hardening, WebSocket cadence and backpressure | M3 | M4a edits only the intentional removals; M7 edits tool/dashboard routes; the listener, origin, and file-security cases persist unchanged into 2.0 |
| `tests/performance_invariants.rs` | allocation-free reads, scanner bounds, encoding-cache sharing, latest-only coalescing | M3 | M4a ports the encoding/coalescing cases to adapter feeds |
| `tests/process_memory.rs` | native reads on both OSes | M0 onward (run natively at every milestone touching reads: M0, M1, M5) | — |
| `tests/config_compat.rs` | config format, migration, reload | M3 | M4a/M8 for removed keys |
| `tests/daemon_smoke.rs` | end-to-end startup and serving | every milestone; extended in M3, M7 | — |
| `tests/updater_compat.rs`, `updater_native.rs` | transactional update | M−1 adds consent | — |
| `tests/tool_host_smoke.py`, `tests/fixtures/tool-host/` | 1.x tool services | M6 (M3 adds a bridge case) | M7 rewrite for web-only tools |
| `tests/dashboard_smoke.py`, `catalog_art_smoke.py`, `tool_catalog_smoke.py` | dashboard and catalogs | M5 (M−1 adds the update-consent prompt) | M6b (catalog v2), M7 |

Golden fixtures come from two sources with different owners:

1. **Projection goldens need no live session.** `tests/fixtures/oracle/`
   already holds per-scenario inputs and the pinned upstream builders' outputs
   (`v1.json`, `v2.json`, `v2-precise.json`, `sc.json`), generated by
   `tools/oracle/generate.ts` at the revisions in `tools/oracle/oracle-lock.json`
   and checked by `tests/state_api_compat.rs` (`EXPECTED_REVISIONS`, lines
   39–45). M4a reuses them unchanged as the adapters' payload goldens.
2. **Behavioral goldens need a live session (owner-dependent, M4b).** Status
   codes, error bodies, headers, file routes, `/api/calculate/pp`, WebSocket
   framing and cadence, and near-miss routes are observable only from a
   running pinned upstream with a live osu! client. ADR 0021 requires these
   captures to be committed and checked in CI. The tooling extension lands
   first in M4b without the owner; the capture itself needs the owner's
   machines and clients. **M8 (the release) blocks on it; no earlier
   milestone does.** Each later pin bump repeats the capture (ADR 0021).

Between M4a and M4b the adapters are checked by the source oracle plus the
1.x behavioral tests in `server_compat`, which already encode the upstream
behavior 1.x reproduced; M4b turns that from "same as 1.x" into "same as the
pinned upstream".

## 6. Clean-room `/api/ingame`

ADR 0021 requires `/api/ingame` to be written from a behavioral specification
by contributors who have not read the upstream page assets. This repository's
own history contains such assets: `assets/server/ingame.{html,js,css}` were
deleted in `1ccd4cc` ("Remove in-game overlay integration", 2026-07-17,
recorded in `CHANGELOG.md` and [ADR 0008](../decisions/0008-external-in-game-overlays.md)).

- **Specification author:** a contributor named by the owner who attests to
  never having read the upstream page sources or this repository's files at
  those paths before `1ccd4cc`. Method: black-box observation only. Run the
  pinned upstream with its in-game overlay client and a live osu! session;
  record (a) the requests the page makes to the daemon (routes, query
  parameters, WebSocket messages, settings keys), capturing request lines and
  daemon-route response bodies but never the bodies of the page's own HTML,
  JavaScript, or CSS; (b) user-visible behavior: which overlays appear, edit
  mode entry and exit, placement persistence, reaction to overlay
  install/removal, error states. The output is
  `docs/v2/ingame-behavior.md` (normative behavior, no upstream text or code).
- **Who, recommended:** the owner if eligible; otherwise a contributor who
  has never worked in this repository's history before `1ccd4cc`. The plan's
  implementing agents are not eligible, because they routinely read history.
- **When:** in the same owner session as the M4b captures (D14), because both
  need the pinned upstream and a live client. If the owner is not eligible as
  author, the owner operates the machine while the named author observes. The
  M7 implementation blocks on this document; if the session slips, the
  endpoint lands as a separate step before M8 rather than holding M7.
- **Implementer:** a second clean contributor (or agent) given only the
  behavioral spec, ADR 0021's deprecation requirements (`Deprecation` and
  `Link: <…>; rel="successor-version"` headers, one first-request warning,
  the edit-mode notice, the dashboard banner), and the 2.0 overlay host. Agent
  prompts state the history restriction explicitly (no `git show`/`git log -p`
  of those paths). Lands in M7; `tests/ingame_compat.rs` is derived from the
  spec only; the divergence file records "functionally compatible, not
  byte-identical".

## 7. Ecosystem fan-out

ADR 0022 starts ecosystem work once the contracts are frozen (they are) and
the SDK and web bridge exist. **Trigger: M3 and M6a landed on `v2/main` and
`v2.0.0-alpha.1` published as a GitHub prerelease (M−1 publishing fix).** At
that point the SDK can build and load an external plugin (M1), plugins can
publish namespaces (M2), declare resources and actions, and tools can call
them through `vosu.invoke`/`vosu.start` and subscribe through API v1 (M3),
and every package kind has a validator and JSON Schema (M6a). Until M6b,
ecosystem plugins run through `--plugin <dir>`; until M5, only in-process.
One owner agent per repository; each repository pins the alpha tag and moves
to each newer alpha deliberately.

| Repository | Changes (ADR 0022, specs) |
|---|---|
| vosu-tools | Each `[service]` becomes a plugin crate on `vosu-plugin-api` (replacing `crates/vosu-tool-sdk`): `[api].write` routes become `[[actions]]` with `args_schema`/`result_schema`; read routes that return state become a plugin namespace with a published schema; file-returning routes become resources or trees; `[cli]` becomes actions with `cli = true` (`vosux <plugin-id> <action>`); `[permissions].host = ["context"]` and `songs:write` become `requires` on `vosu.osu` fields and the plugin-only paths subtree (D2, D8). Each `tool.toml` becomes `manifest.toml` `kind = "tool"` with `[ui]` and `requires`/`optional`; `kind = "plugin"` manifests with `runtime`, `[targets.*]`, `provides`. `packages/tool-web` (`bridge.d.ts`) adopts the 2.0 bridge; every `/json/v2` and `/websocket/v2/subscribe` consumer moves to API v1 pointers using state-schema.md's mapping table. `scripts/package.py` emits `catalog-v2.json` beside `catalog.json` (catalog-v2.md "1.x coexistence"). |
| vosulay | The always-on service (`tool.toml` `[service] startup = "always"`, `src/backend.rs`) becomes a plugin requesting `runtime = "process"` that owns the CEF in-game host (D16); the editor becomes a `kind = "tool"` UI; consumers of `/websocket/v2/subscribe` and `keys.all` (`src/backend.rs`, `src/runtime/editor/rulesets.rs`) move to API v1. It is the `/api/ingame` successor named in the `Link` header. Publishes `catalog-v2.json`; its own winget/Scoop/AUR packaging is unchanged. |
| vosu-overlays | `metadata.txt` becomes `manifest.toml` `kind = "overlay"` with `requires`/`optional` field ids (common-layer fields where possible so overlays work across games); `overlays/*/overlay.js` move from `/websocket/v2/subscribe` to `WS /api/v1/sessions/active/subscribe`; `scripts/package.py` publishes both catalogs. |
| vosu-overlay-quickstart | `app.js` uses API v1 (api-v1.md "Worked examples"), ships a `manifest.toml` template, `TUTORIAL.md` rewritten; a release tagged `v2.x` (required by the day-0 check). |
| packaging (`packaging/` here) | `nfpm.yaml` and AUR install hooks keep running `vosu --setup`; the plugin host is the same executable, so no new file capabilities; the `vosux` symlink stays; the release adds `include/vosu_plugin.h` as an asset for C plugin authors; `scripts/package-release.py` notes describe 2.0. Starts after M5 (plugin host exists). |
| docs | README routes, configuration, and CLI; quickstart; a plugin author guide (SDK, manifest, trust, runtime); an overlay author guide (API v1, resources, common layer); the 2.0 tool protocol; a 1.x→2.0 migration guide; each ecosystem repository's README. Public docs name only osu! as a supported game. Runs continuously from the trigger; final at M8. |

The day-0 gate (M8) passes only when vosu-tools, vosulay, and vosu-overlays
each have a published release with `catalog-v2.json` and the quickstart's
latest release is major version 2 (catalog-v2.md "Day-0 release check").

## 8. Build-time goal

Baseline (ADR 0019, Ryzen 9 9950X3D at `d01e151`, dependencies compiled, an
edit in `src/api/fixed.rs`): `cargo check` 2.4–2.7 s, `cargo build`
3.8–6.2 s, library test build 6.3–7.0 s, rebuilding the `vosu` crate alone
26.5 s. The cost is one 122,000-line crate: any edit recompiles all of it.

Expected effect of the target layout (inference, to be measured):

- **Leaf edits** (an adapter, the dashboard, a host plugin, one osu! crate)
  recompile only that crate and its dependents, then relink `vosu`. Adapter
  and host crates are 2,000–8,000 lines and have no workspace dependents
  except the root, so `cargo check` should fall well under one second and
  `cargo build` to roughly the link time; `cargo test -p <crate> --lib`
  builds one small test binary instead of the monolith's.
- **osu! crate edits** recompile that crate plus its dependents within the
  osu! chain (reader → state → plugin → adapters that use the model).
  `vosu-osu-reader` (~22,000 lines) is the largest; if its probe misses the
  goal, split it into stable and lazer crates — `src/memory/stable` and
  `src/memory/lazer` do not import each other.
- **Core and plugin-API edits** recompile everything downstream. Because
  plugins depend on `vosu-surface`/`vosu-plugin-api` rather than `vosu-core`,
  a `vosu-core` edit recompiles only core (an estimated 40,000–50,000 lines
  at M7 by the section 3 mapping, new code included) and relinks; a
  `vosu-plugin-api` edit is the worst case and should stay rare.
- **Clean builds** gain parallelism across crates on 32 threads. Release
  output is unchanged in kind (thin LTO, one codegen unit).

Goals to adopt (owner may adjust): leaf-crate edit `cargo check` ≤ 1.0 s,
`cargo build` ≤ 2.5 s, crate library test build ≤ 3.0 s; `vosu-osu-reader`
edit `cargo check` ≤ 2.0 s; a `vosu-core` edit no slower than today's
whole-crate rebuild.

Measurement (added in M0 as `scripts/measure-build.py`, results recorded as
the latest observation in the [benchmark guide](../testing/benchmarks.md)):
pinned toolchain, warm `target/`, CPUs pinned off the VM's cores as
brokered-reads.md "Machine and commands" does; for each probe file, append a
comment line, time `cargo check --locked`, `cargo build --locked`, and
`cargo test --locked -p <crate> --lib --no-run`, revert, five runs, report
median and range. Probes: the ADR's own `src/api/fixed.rs` (after M4a in
`vosu-adapter-support`), `src/memory/lazer/reader.rs`,
`src/state/bundle/reducer.rs`, `src/server/routes.rs` (core router),
`vosu-plugin-api`'s lib, and `src/runtime/render.rs` (dashboard). Measure
before and after M0 on the same machine and commit, then again at M4a and
M7. If linking dominates leaf edits, measuring `lld` or `mold` for local
debug builds is the next step; it does not change release builds.

## 9. Contract gaps found while planning

Implementing the frozen specs against the 1.x code exposed these gaps. Each
maps to an owner decision in section 10 (D2–D9) except item 8, which is a
documentation step; amendments are additive and land
before the milestone named, while no plugin has shipped.

1. Adapters must serve values that `vosu.osu` intentionally omits: raw
   folder and file paths (`src/model/api/v2.rs` `Folders`, `DirectPath`;
   `src/model/api/v1.rs` `Folders`, `BeatmapPaths`), and upstream-only
   derived data (`src/model/api/sc.rs` `mapStrains`, breaks, kiai and timing
   points), while state-schema.md says API v1 never serves a raw path (D2).
2. The ABI has no HTTP opcode, and api-v1.md says plugins never serve HTTP,
   yet the dashboard, overlay host, tool host, and adapters are bundled
   plugins whose job is serving routes (D3).
3. `VOSU_OP_PUBLISH_FIELD` carries one leaf and no batch boundary
   (`vosu_message.flags` is "0 unless the spec says otherwise"), so a
   multi-field change cannot be published atomically, which ADR 0002 and the
   overview's "Atomicity regressions" hotspot require (D4).
4. Read plans are asynchronous messages even in-process (plugin-abi.md §3,
   §5), while the 21,756-line 1.x reader stack reads synchronously (D5).
5. 1.x discovery is conditional — Linux's broad lazer fallback runs only
   when the named first pass is empty (`src/instance/discovery.rs:50-110`) —
   and argv-based skipping (`should_skip`, line 118); flat `[match]` tables
   cannot express either. Tournament clients aggregate into one 1.x instance
   but would be separate 2.0 sessions (D6).
6. External plugins have no way to hand core their namespace schema, so core
   cannot compile `x-sampling-lane` demand for third-party namespaces (D7).
7. 1.x services received install paths through `GET /tools/_host/v1/context`
   (`songs_dir`, `osu_dir`, skin folder); 2.0 plugins such as the downloader
   and practice tools have no equivalent (D8).
8. The shared naming contract that state-schema.md, catalog-v2.md, and the
   schemas cite is not a committed document; its rules are spread across
   catalog-v2.md "Namespace and kind parsing" and the schemas. Commit it as
   `docs/v2/naming.md` before M1 so the validators have one source.
9. ADR 0019 says "one crate per bundled plugin"; the build-time goal needs
   the osu! plugin as five crates and core as four (D9).

### Edge cases every milestone's tests must cover

- A session that identifies after the 5-second window becomes visible from
  then on; `active` with no identified session is 404 `unknown_session`;
  focus falls back to the most recently focused session when no client owns
  the foreground window (api-v1.md "Focus rule").
- Windows PID reuse: discovery's creation-time invalidation
  ([overview](../architecture/overview.md) "Discovery and reader boundary")
  must survive the move into core.
- A read plan for a detached handle answers `VOSU_ERR_TARGET_GONE`; a late
  result after `READ_PLAN_CANCEL` answers `VOSU_ERR_CANCELLED`.
- A resource evicted from the bounded registry mid-download (`STREAM_CLOSE`)
  re-resolves from scratch; a resource id is identical after a restart for
  the same content.
- A plugin host killed mid-job yields a job `failed` with `plugin_crashed`;
  a job past its timeout yields `timed_out`.
- A catalog redirect that changes origin drops the credential; the
  private-network exception never applies to package or artwork URLs on
  other hosts.
- Migration interrupted halfway and run again converges to the same state; a
  1.x user tool with a service is kept, disabled, and marked
  `needs-2.0-update`.
- Two enabled plugins with overlapping matchers: the second is refused at
  manifest validation; a catalogued but uninstalled plugin never triggers it.
- Unknown and near-miss routes remain 404 after route registration replaces
  the 1.x `Endpoint` enum (`src/server/routes.rs`).

## 10. Risks and owner decisions

Decisions, each with a recommendation:

- **D1 Integration branch.** Name `v2/main`, protected with `CI required`,
  landed through `land.py --base v2/main`, `main` merged in weekly.
  *Recommend: approve.* The alternative of landing 2.0 on `main` behind
  features would ship half-built 2.0 code in 1.x releases.
- **D2 Plugin-only state.** Add an `x-visibility: "plugins"` schema
  annotation (common-defs) and a `vosu.osu` subtree `internal` carrying raw
  paths and upstream-only derived values. Core never serves those pointers
  through API v1 (reads return absence, subscriptions never select them) and
  delivers them only to plugins whose manifest `requires` them; overlays and
  tools may not require them. Amend state-schema.md, api-v1.md, catalog-v2.md;
  bump `vosu.osu` to 1.1.0. *Recommend: approve before M2.* Rejected
  alternative: typed Rust access from adapters into the osu! plugin's
  internals, which contradicts ADR 0019/0021 ("adapters project the state
  bus") and couples them to reducer types.
- **D3 Bundled surface API.** Bundled plugins register through the C ABI for
  lifecycle, manifests, settings, and actions, and additionally receive the
  Rust-only `vosu-surface` API for route and WebSocket-feed registration and
  typed snapshot reads. External plugins never serve HTTP. Record as an ADR
  0019 amendment. *Recommend: approve before M1.* An HTTP opcode in the ABI
  would copy every response through messages and widen the third-party
  surface.
- **D4 Publish batches.** Define `vosu_message.flags` bit 0 on
  `VOSU_OP_PUBLISH_FIELD` as `VOSU_PUBLISH_CONTINUES`; core commits one
  immutable snapshot at the first message without it. Update the header,
  plugin-abi.md §6.6, and the layout tests. *Recommend: approve before M1.*
- **D5 Synchronous reads in-process.** Clarify plugin-abi.md §4/§6.3: for an
  in-process plugin with `concurrent_dispatch = true`, the host may deliver
  `VOSU_OP_READ_PLAN_RESULT` re-entrantly on the submitting thread before
  `send` returns; the SDK's `PlanMemory` relies on it. The osu! plugin ships
  the §10 stress test. *Recommend: approve before M1.* Rewriting every 1.x
  reader as an asynchronous plan state machine first would delay M1 by the
  size of `src/memory/` and risk parity.
- **D6 Discovery parity and auxiliary processes.** (a) Add
  `VOSU_OP_SESSION_DECLINE` (plugin → host, CBOR `{"handle"}`): the plugin
  rejects a matched process that is not a client (the conditional lazer
  fallback, argv skips); core detaches it and does not re-offer that PID
  generation. (b) Tournament client processes stay attached but are
  deliberately never identified, so plugin-abi.md §6.2 keeps them hidden
  while the osu! plugin still reads them for `vosu.osu.tourney.clients[]`;
  the only cost is one log line per client. *Recommend: (b) as specified
  today; (a) only if the M1 matcher translation of the
  `src/instance/discovery.rs` tests fails; decide by M1 exit.*
- **D7 Namespace schemas for external plugins.** Add `schema = "<path>"` to
  `[[provides]]` entries naming a whole `field:` namespace (catalog-v2.md),
  mirrored in `catalog-v2.json`. *Recommend: approve before the fan-out
  trigger (M6a).*
- **D8 Install paths for plugins.** Serve them through the D2 subtree
  (`vosu.osu.internal.paths`), so a plugin that writes into Songs declares it
  in `requires` and the install plan shows it. *Recommend: approve with D2.*
- **D9 Multi-crate plugins and core.** Allow a bundled plugin and core to
  span several crates, recorded in the architecture docs and ADR 0019's
  "Source layout". *Recommend: approve before M0 step 2.*
- **D10 Crate and plugin names.** `vosu-adapter-upstream`,
  `vosu-adapter-gosumemory`, `vosu-adapter-streamcompanion`; plugin ids
  `vosu.adapter.upstream`, `vosu.adapter.gosumemory`,
  `vosu.adapter.streamcompanion`; `vosu.osu`, `vosu.dashboard`,
  `vosu.overlays`, `vosu.tools`. *Recommend: approve;* upstream-brand names
  would need new `scripts/brand-allowlist.txt` entries for every crate path.
- **D11 SDK distribution.** Git tag dependency during alphas; publish
  `vosu-plugin-api` to crates.io at 2.0.0 (the only crate with
  `publish = true`) and the C header as a release asset. *Recommend:
  approve.*
- **D12 Bridge path.** Serve the 2.0 bridge at `/tools/_host/v2/bridge.js`
  and remove the v1 path in M7 (every official tool is rebuilt).
  *Recommend: approve.*
- **D13 New dependencies.** `libloading` (dlopen), `linkme` (bundled
  registry), `hmac` (resource ids), `jsonschema` (dev-only, schema
  validation) are new; `ciborium` and `regex` are already in `Cargo.lock`.
  Each lands with its `Cargo.lock` review. *Recommend: approve the list.*
- **D14 Owner capture session.** One scheduled session covering the M4b
  captures on Windows stable, Windows lazer, and Linux lazer and the
  `/api/ingame` observation, with the clean-room author named beforehand.
  *Recommend: schedule once M4b tooling lands; it gates only M8.*
- **D15 Forge verification.** Owner provides private test repositories and
  read-only tokens on GitHub, GitLab, Codeberg, and a Gitea or Forgejo
  instance for M6b. *Recommend: approve; mocks cover CI, real forges cover
  ADR 0023's verification clause.*
- **D16 vosulay architecture.** Its native host becomes a plugin requesting
  `runtime = "process"` (official, so it could run in-process, but a CEF host
  inside the daemon would let a renderer crash stop every game); the editor
  becomes a tool. *Recommend: approve.*
- **D17 Performance thresholds.** No more than 10% p50 regression against the
  previous milestone for `raw_snapshot_reads` (M1), `publication` (M2), and
  `serializers_cold`/`serialization_cache` (M4a); 2x the prototype for
  brokered reads (M5). *Recommend: approve as the landing bar.*

Risks without a separate decision:

- **Integration drift:** 1.x fixes in code that 2.0 has rewritten (after M4a,
  `src/api/`) must be ported by hand; the weekly merge surfaces them early.
- **Ecosystem schedule:** ADR 0022 ties the release date to four
  repositories; the trigger in section 7 is placed as early as the contracts
  allow, and alphas let each repository integrate continuously.
- **Hidden 1.x quirks:** the adapters rely on M4b to catch behavior that the
  source oracle and `server_compat` do not cover; until then "same as 1.x" is
  the bar.
- **Windows transport:** unmeasured until M5; the spec allows the choice to
  follow the measurement.
- **Linux capability drop:** must run before any thread exists; a
  dependency that spawns threads during static initialization in
  `vosu plugin-host` would break §7.3, so `plugin-host` dispatch happens
  first in `main`, before the Tokio runtime is built (`src/main.rs`).

## Critical files

- Contracts: [plugin ABI](plugin-abi.md), [`include/vosu_plugin.h`](../../include/vosu_plugin.h),
  [state schema](state-schema.md), [`schemas/`](schemas/), [API v1](api-v1.md),
  [catalog v2](catalog-v2.md), [brokered reads](brokered-reads.md), ADRs 0019–0023.
- Structure and checks: `Cargo.toml`, `build.rs`, `src/lib.rs`, `src/main.rs`,
  [`scripts/module-boundaries.txt`](../../scripts/module-boundaries.txt),
  `scripts/check-module-boundaries.py`, `scripts/check-file-length.py`,
  `scripts/file-length-baseline.txt`, `scripts/ci-changes.py`,
  `scripts/prepush.py`, `scripts/land.py`, `scripts/sign-release.py`,
  `.github/workflows/{ci,nightly,release}.yml`.
- Seams: `src/process.rs` (`ProcessMemory`, `ProcessHandle`),
  `src/memory.rs`, `src/memory/lazer_loader.rs`, `src/instance.rs`,
  `src/instance/discovery.rs`, `src/state/worker.rs`, `src/state/bundle/reducer.rs`,
  `src/calculation/state_backend.rs`, `src/api/snapshot.rs`,
  `src/runtime/projection.rs`, `src/server/routes.rs`,
  `src/server/service/routing.rs`, `src/server/v2_subscription.rs`,
  `src/server/files.rs`, `src/tools/mod.rs`, `src/tools/process.rs`,
  `src/updater/release.rs`, `src/net/streaming.rs`, `src/config/schema.rs`,
  `assets/server/tool-bridge.js`.
- Evidence: `tests/state_api_compat.rs`, `tests/fixtures/oracle/`,
  `tools/oracle/`, `tools/live-parity/`, `tests/server_compat.rs`,
  `benches/hot_paths.rs`, `benches/brokered_reads_bench.rs`.

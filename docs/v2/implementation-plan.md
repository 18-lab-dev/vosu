---
type: Plan
title: "vosu 2.0 implementation plan"
description: "Crate layout, milestones, compatibility safety, ecosystem fan-out, build-time goals, and owner decisions for implementing the frozen vosu 2.0 contracts."
---

# vosu 2.0 implementation plan

Status: adopted. The owner accepted this plan subject to an adversarial
review of its decisions; every verdict of that review is applied below, and
section 10 records the adopted decisions. Inputs, all at `v2/plan` `f07cceb`:
[ADR 0019](../decisions/0019-plugin-runtime-and-trust.md),
[ADR 0020](../decisions/0020-game-agnostic-data-model.md),
[ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md),
[ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md),
[ADR 0023](../decisions/0023-authenticated-catalog-sources.md), the frozen
specs [plugin ABI](plugin-abi.md) with [`include/vosu_plugin.h`](../../include/vosu_plugin.h),
[state schema](state-schema.md) with [`schemas/`](schemas/),
[API v1](api-v1.md), [catalog v2](catalog-v2.md), and
[brokered reads](brokered-reads.md), plus the 1.x tree (`src/`, `crates/`,
`tests/`, `scripts/module-boundaries.txt` (per crate under `scripts/module-boundaries/` since M0),
[architecture](../architecture/overview.md), [source map](../architecture/source-map.md)).
`AGENTS.md` is binding for every step below.

## Summary

vosu 2.0 turns the single 1.x daemon crate into a small core plus bundled
plugins (ADR 0019), publishes game state as owned namespaces (ADR 0020),
serves API v1 beside bug-for-bug upstream adapters (ADR 0021), and installs
plugins, tools, and overlays through a capability resolver and catalog v2
(ADR 0022, ADR 0023). The 1.x consent release (1.2.4) and the
behavior-neutral crate split (M0) land on `main`, so 1.x gains the build-time
improvement ADR 0019 measured and the long-lived `v2/main` branch, cut after
M0, never carries a mass relocation. ABI amendments the code needs land before
any plugin code. Then the plugin API and an in-process host, the state bus,
API v1, the adapters rebuilt as bus projections, the out-of-process plugin
host, catalog v2 and the resolver, the dashboard/overlay/tool hosts on the
new model, and finally the 1.x migration and the day-0 release gate. Every
milestone lands with the full `prepush.py` gate green and the 1.x observable
behavior provably unchanged until a milestone deliberately replaces it.
Planning against the 1.x code exposed fifteen contract gaps (section 9); each
is closed by an adopted decision (section 10) or a milestone task.

## 1. Ground rules

- **Branches (D1).** M−1 and all of M0 land on `main` through the normal
  `python scripts/land.py` path; M0 is behavior-neutral by its own exit
  criteria, so 1.x releases may ship from it. `v2/main` is branched from
  `main` after M0 lands and receives M1 onward through
  `python scripts/land.py --base v2/main --topic <milestone>` (M−1 adds
  `--base`). `main` keeps taking 1.x fixes and is merged into `v2/main` at
  least weekly and before every milestone lands; because both branches share
  M0's paths, those merges carry no mass rename. After M4a, 1.x fixes in code
  2.0 has rewritten (`src/api/`) are ported by hand. Every landing needs
  `python scripts/prepush.py` evidence for exactly `HEAD`, including the
  native Windows run (`AGENTS.md:96-105`); `--skip` never reaches `land.py`.
- **Moves are content-identical.** Every file relocation lands as a pure
  `git mv` commit (no edits), followed by a separate edit commit, so history
  and blame follow the move. `land.py` fast-forwards, so both commits survive.
- **Interim bridges are named and dated.** A milestone that must keep a 1.x
  surface alive across a structural change names the bridge, the code that
  implements it, its interim crate edge in `scripts/crate-boundaries.txt`,
  and the milestone that deletes it (B1: M1→M4a; B2: M1→M8). No other
  compatibility shim, alias, or re-export is added (`AGENTS.md:6`).
- **Specs change before code.** Every adopted ABI or spec amendment
  (section 10) lands as a documentation change on `v2/main`, header and
  specs together, before the first milestone that implements it.
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
`scripts/brand-allowlist.txt` entry is needed (D10). `vosu-adapter-support`
and `vosu-osu-model` exist for dependency direction, not crate count: they
let the reader, calculator, and adapters share osu! types without any of them
depending on another (D9). "Allowed
dependencies" lists workspace crates only; it is recorded in a new
`scripts/crate-boundaries.txt` and enforced from `cargo metadata` (M0).

| Crate | Responsibility | Allowed workspace dependencies |
|---|---|---|
| `crates/vosu-plugin-api` | ABI v1 types mirroring `include/vosu_plugin.h` (`repr(C)`, layout-tested), message codecs (flat hot-path layouts including `PUBLISH_BATCH`, CBOR), the Rust SDK (`Plugin` with `&mut self` behind an SDK lock and `ConcurrentPlugin` with `&self`, D22; `HostServices`; `Message`; `export_plugin!` for external builds), manifest validation shared by core and SDK users. Published to crates.io (D11). Since S0 (AF.9) it declares no Cargo features and no workspace dependency (not even a dev-dependency), ships a crate README and license copies, and carries the `vosu-package-check` example (AF.10); its workspace-only header, packaging-schema and `prop_*` tests are the root `tests/sdk.rs`. Versioned by SDK semver whose major equals the ABI major. | none |
| `crates/vosu-plugin-internal` (AF.9) | `publish = false` companion: `PlanMemory` and the SDK-side synchronous-read interception (D5) — a wrapper around the SDK glue's `dispatch` — the bundled-registration glue for the root crate's `BUNDLED` array, the intercepting `cdylib` form for the osu! process fixture, and the opt-in `read_plan_profile` diagnostics, moved out of `vosu-plugin-api` by S0 with every internal caller (D11). Its users (`vosu-osu`, the adapters, `vosu-plugin-testkit`, `vosu-core` for host-side profile spans, the root crate) are all `publish = false`; `scripts/check-crate-boundaries.py` enforces both. | `vosu-plugin-api` |
| `crates/vosu-sys` (core-owned) | Native boundary: `src/platform*`, `src/process*` (Linux/Windows `ProcessMemory` backends, signature region walk, module tables), process-event backends `src/instance/events*` with the eBPF build step. All platform `unsafe`. | `vosu-plugin-api` (M5 R0: implements the platform-neutral `memory` vocabulary — `ProcessMemory`, read requests, `MemoryError`/`ReadStatus`, `ProcessBitness`, signature `Pattern` — instead of defining it) |
| `crates/vosu-net` (core-owned) | Hardened HTTPS streaming and archive extraction (`src/net*`), later credential origin scoping and the per-host private-network exception (ADR 0023). | `vosu-sys`; `vosu-plugin-api` (F S0: the SDK's package-path `portable_component` rule) |
| `crates/vosu-surface` (core-owned) | Rust-only API for **bundled** surface plugins (D3), listed exhaustively in the ADR 0019 amendment: exact route and WebSocket-feed registration on a listener role; state reads that go through the same subscription, demand, and `requires` path as `STATE_SUBSCRIBE`; root-confined file responses against `ROOT_DECLARE` handles; management-route provenance; and, from M7, the A7.1 presentation views core pushes to the two hosts that need them ([A7 freeze](#a7-approved-contract-freeze-2026-11-24)). No `ProcessMemory`, configuration writes, or credentials. Not part of the C ABI. | `vosu-plugin-api` |
| `crates/vosu-core` | Config, logging, report, updater, desktop integration, generic discovery/generations/focus (generalized `src/instance*`), plugin registry and host (in-process dispatch, `vosu plugin-host` supervision and channel), state bus and demand compiler, listeners/origins/auth/API keys/broker/file confinement/WebSocket hub primitives, API v1 (sessions, subscriptions, reads, resources, trees, jobs), catalog v2, sources, credentials, resolver, 1.x migration, lifecycle; from M7 (A7.1) also every privileged dashboard management handler, the tool registry with launch codes, launch sessions and action authority, and the dashboard-off switch. | `vosu-sys`, `vosu-net`, `vosu-plugin-api`, `vosu-surface`, `vosu-plugin-internal` (S0: host-side `read_plan_profile` spans; bundled test fixtures) |
| `crates/vosu-osu-model` | osu! value types and osu!-exact helpers shared by reader, calculator, and adapters: `src/model/{memory,states,enums}*` and the calculation root helpers (`src/calculation/root/{helpers,mods,scores}.rs`: JS-compatible rounding and number writing, mods, grades, legacy hits). | none |
| `crates/vosu-osu-calc` | Live and legacy calculation: `src/calculation/{beatmap,progressive,engine}*` over `crates/vosu-pp` (ADR 0004 contracts unchanged). | `vosu-osu-model`, `vosu-pp` |
| `crates/vosu-osu-reader` | Client decoding: `src/memory*` (stable, lazer, offsets, loader, snapshots), the pointer-width read layer now in `src/process.rs::ProcessHandle`, the embedded `assets/offsets.json` and generator bundle, `src/runtime/offset_generator*`, osu!-specific discovery inputs (client argv parser `StartupArgs`, Wine path resolution `resolve_osu_path`). | `vosu-osu-model`, `vosu-plugin-api`, `vosu-net` (the moved offset-generator's hardened ZIP extraction, `vosu_net::extract_zip_atomic`, DD, and since M5 R1 the atomic offset-cache publish/read convention, `vosu_net::publish`). The interim `vosu-sys` edge (DC) was removed by M5 R1: reads use `PlanMemory`, every scan goes through an injected `ScanBackend` (the host's `SCAN_REQUEST`; no region-walk fallback), the lazer executable root comes from the `SESSION_ATTACHED` `path` fact, and the offset generator's composer injects the gated `vosu_sys::process_spawn` spawn. `vosu-sys` remains a dev-dependency only |
| `crates/vosu-osu-state` | Reducer, capture scheduling, gameplay state: `src/state*` plus the calculation backend (`src/calculation/state_backend*`). | `vosu-osu-reader`, `vosu-osu-calc`, `vosu-osu-model`, `vosu-pp`, `vosu-plugin-api` (the reducer's `MemoryRead`/`ReadStatus`/`MemoryError` from `vosu_plugin_api::memory`; the interim `vosu-sys` edge was removed by M5 R0) |
| `crates/vosu-osu` | The bundled osu! plugin: manifest, matchers, `SESSION_DECLINE` use, session identification, namespace projection (`vosu.osu`, common layer, the paths-only `internal` subtree, D2), roots and resource kinds, settings (D19), the `beatmap.import` action (D8). A `test-harness`-only `cdylib` build with a non-bundled test manifest runs it out of process in M5 (gap 13). | `vosu-plugin-api`, `vosu-plugin-internal` (S0: `PlanMemory`, bundled and process registration), `vosu-osu-state`, `vosu-osu-reader`, `vosu-osu-calc`, `vosu-osu-model`; no `vosu-surface` dependency after M4a |
| `crates/vosu-adapter-support` | Shared upstream-wire machinery: `src/api/{json,fixed,wire_fixtures}.rs`, legacy filter parsing, JS-number JSON writer. | `vosu-osu-model` |
| `crates/vosu-adapter-upstream` | Upstream v2 adapter: `/json/v2`, `/json/v2/precise`, `/websocket/v2`, `/websocket/v2/precise`, `/files/...`, `/Songs/{path}`, `/backgroundImage`, `/api/calculate/pp` (api-v1.md "Route ownership"). `/api/ingame` is a removed route, not an adapter route (section 6). | `vosu-surface`, `vosu-plugin-api`, `vosu-plugin-internal` (S0: bundled registration), `vosu-adapter-support`, `vosu-osu-model`, `vosu-osu-calc` |
| `crates/vosu-adapter-gosumemory` | gosuMemory-compatible `/json`, `/ws`. | `vosu-surface`, `vosu-plugin-api`, `vosu-plugin-internal` (S0: bundled registration), `vosu-adapter-support`, `vosu-osu-model` |
| `crates/vosu-adapter-streamcompanion` | streamCompanion-compatible `/json/sc`, `/tokens`; computes strains, breaks, kiai points, and timing points itself from the chart resource (D2). | gosumemory's list plus `vosu-osu-calc` |
| `crates/vosu-dashboard` | Dashboard page shells, the embedded dashboard-only `assets/server/` files, and the browser presentation of every management view (plugins, tools, overlays, catalog, settings, status, D20 notice including the removed `/api/ingame` route's vosulay install action). It holds no management authority: every view reads and writes the exact core or host routes in the [A7 freeze](#a7-approved-contract-freeze-2026-11-24). Inactive, but still registered, when the dashboard is off (A7.3). | `vosu-surface`, `vosu-plugin-api`, `vosu-plugin-internal` (bundled registration only, as for the adapters) |
| `crates/vosu-overlay-host` | Overlay hosting: counter discovery/download/settings/profiles (`src/counters*`), the community overlay catalog (`src/server/catalog.rs`) and its card presentation, `/websocket/commands` command processing (`src/server/commands*`), overlay static serving. | `vosu-surface`, `vosu-plugin-api`, `vosu-net`, `vosu-plugin-internal` (bundled registration only) |
| `crates/vosu-tool-host` | Web-only tool hosting: tool UI static serving and art, the read-only `_settings` view, the v2 browser bridge and the shared design-system assets. Launch codes, sessions and action authority are core's (A7.1, A7.2). | `vosu-surface`, `vosu-plugin-api`, `vosu-plugin-internal` (bundled registration only) |
| `crates/vosu-proptest` (dev-only) | `vosu_proptest_config()` from `src/proptest_support.rs`, shared by every crate with `prop_*` tests. | none |
| `crates/vosu-plugin-testkit` (test-harness only) | Bundled test plugin with echo/progress/cancel/stream actions and a synthetic game, for API v1, bridge, and plugin-host tests and as the SDK reference example; also `vosu-core`'s own dev-dependency for `PluginHost` session/focus end-to-end tests (`session.rs`'s `SessionPlugin`). | `vosu-plugin-api`, `vosu-plugin-internal` (S0: bundled registration, `PlanMemory`) |
| `.` (`vosu`) | `main.rs`, `vosux` and `plugin-host` argv dispatch, Windows resources, the explicit `BUNDLED` registration array (D13), integration tests, benches, examples. | every crate above |
| `crates/vosu-pp` and members, `crates/vosu-test-harness`, `crates/vosu-release-smoke` | Unchanged roles (`AGENTS.md` "Source ownership"); the harness depends on the specific crates it drives instead of the old monolith. | unchanged |

M4a H0's root provider/cache tests exercise all three adapters through the real
producer and bus. gosuMemory and streamCompanion are root **dev-dependencies**;
R1 already supplies the upstream dependency for its unmounted file providers.
The generic hub consumes only the surface projection contract; production
registration remains I0's atomic cutover, not a second root-owned implementation.

Layout rules:

- Plugins (`vosu-osu*`, adapters, hosts) never depend on `vosu-core`. Bundled
  surface plugins reach core only through `vosu-surface` traits that core
  implements; the game plugin reaches core only through the C ABI via
  `vosu-plugin-api`, except bridge B1's recorded interim edge. This is also the build-time property that matters: a
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
| `src/config*` | 2,565 | `vosu-core` | move; drop removed keys (`src/config/schema.rs` lines 145–175, 257–276), add `API_V1_SUBSCRIPTION_DEBOUNCE` and the dashboard key; the osu!-specific keys (lines 139, 163, 169, 181, 187) become `vosu.osu` settings (D19) | M0, M1, M3, M4, M7, M8 |
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
main:
  M−1  1.2.4 consent gate; land.py --base; prerelease publishing
   └─ M0   crate split, no behavior change           ── v2/main branched here
v2/main:
  A1   ABI and spec amendments (D2–D10, D18, D20, D22), docs only
   └─ M1   plugin API + in-process host + osu! plugin behind 1.x surfaces
       ├─ S1   vosulay supervisor-plugin spike, Linux and Windows
       └─ M2   state bus and vosu.* namespaces
           ├─ M3   API v1, resources, jobs, bridge
           │   └─ M4a  adapters as bus projections (deletes B1)
           │       ├─ M4b  golden fixtures: pilot capture at once, full set later
           │       └─ M5   plugin-host process (needs S1; Linux, then Windows)
           └─ M6a  manifest.toml + catalog-v2.json validation
               └─ M6b  sources, credentials, resolver
  F    ecosystem fan-out: after M3 + M6a and v2.0.0-alpha.1
  M7   dashboard, overlay host, tool host                       (needs M3, M5, M6b)
  M8   migration, release smoke, day-0 gate, release      (needs all, M4b included)
```

Ordering rules and why:

- **M0 on `main`** (D1): no 2.0 code, so no reason for a long-lived branch to
  carry the relocation.
- **A1 before M1**: `SESSION_DECLINE` (D6), `PUBLISH_BATCH` and the
  unknown-flag rule (D4), the per-plan synchronous-read flag (D5), the
  `ConcurrentPlugin` SDK shape (D22), and the matcher hints (D18) change the
  header and its layout tests, so they precede the code that freezes them.
- **S1 before M5**: vosulay is on the day-0 critical path (catalog-v2.md
  "Day-0 release check") and its hosting model is the riskiest integration
  (D16); the spike needs only M1's SDK.
- **M5 after M4a**: bridge B1 passes a typed Rust snapshot that cannot cross
  a process boundary; once M4a deletes it, M5's process-runtime test of the
  osu! plugin runs the real bus path.
- **M4b pilot as soon as its tooling lands** (D14), so an adapter/upstream
  mismatch surfaces months before M8, which the full capture still gates.
- **Ecosystem plugins exercise the process runtime before release**: from M5,
  `--plugin <dir>` loads with `user` trust in alpha builds, so the process
  runtime and approval are what ecosystem testers use (section 7); M8
  requires every official plugin to have passed a process-runtime smoke.
- M3 and M6a proceed in parallel after M2; M4a's JSON/WebSocket projection
  may start in parallel with M3 and needs M3 only for file routes.

Every milestone's exit criteria include the **common gate**: `prepush.py`
green for `HEAD` (formatting, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`,
`cargo test --workspace --lib --bins --tests --examples --all-features --locked`
including doctests, repository checks and `scripts/test_*.py`, Windows MSVC
cross-Clippy, native Windows Clippy/tests/doctests through
`scripts/native-ci.ps1`, and the dashboard smokes when UI paths changed),
then hosted CI through `land.py`. Performance gates follow D17 at every
milestone that touches a measured path. Only milestone-specific commands are
listed below.

### M−1: 1.2.4 consent gate and landing infrastructure (on `main`)

Scope:

- **1.x major-version consent ships as 1.2.4, before M0** (D1). ADR 0022:
  it protects only installations that receive it before 2.0 is published.
  `parse_latest_release` (`src/updater/release.rs:26-56`) returns a new
  `ReleaseDecision::NeedsConsent` when the candidate's major version exceeds
  the running one; automatic updates stop there; the dashboard update UI
  asks; `RunUpdates` accepts an explicit consent flag.
- **Landing base.** `scripts/land.py` gains `--base <branch>` (default
  `main`): `ensure_fast_forward_of_main` (`scripts/land.py:83-93`) and
  `fast_forward_main` (`scripts/land.py:144-145`) take the base; branch
  protection on `v2/main` requires `CI required` when it is cut after M0.
  `ci.yml` needs no trigger change (`ci/**` already runs);
  `nightly.yml` gains a `v2/main` checkout so random proptests and the lazer
  canary run there too; `scripts/test_gate_tooling.py` covers `--base`.
- **Prerelease publishing.** `scripts/sign-release.py --publish`
  (`scripts/sign-release.py:120-122`) publishes drafts with only
  `draft=false`; for a semver prerelease tag it also sets `prerelease=true`
  and `make_latest=false`, so `v2.0.0-alpha.N` never becomes the latest
  release. 1.x already ignores prereleases (`src/updater/release.rs:35-51`).

Exit: `cargo test --locked --test updater_compat`; native
`cargo test --locked --test updater_native -- --nocapture` on Linux and
Windows; unit tests for `NeedsConsent` beside `src/updater/tests_release.rs`;
the dashboard smokes (update UI changed); `scripts/test_gate_tooling.py`
(run by `prepush.py`) covering `--base`; 1.2.4 published.

Risks: users who skip 1.2.4 still update to 2.0 silently (accepted by ADR
0022); 1.2.4 stays a minimal patch release so it ships before M0 starts.

### M0: crate split without behavior change (on `main`)

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
   - `scripts/ci-changes.py`: `UI_PREFIXES` and the `render*` rule
     (`scripts/ci-changes.py:9-21`) cover the relocated paths, and the
     schema-only exemption list (`scripts/ci-changes.py:34`) names the new
     crate paths; otherwise UI smokes and native verification silently stop.
   - `AGENTS.md` "Source ownership", the property-test rule
     (`vosu_proptest::vosu_proptest_config()`), [source map](../architecture/source-map.md),
     and [overview](../architecture/overview.md) "Deployment variants".
   - `scripts/measure-build.py` and its first results, including the D9
     budget items (section 8).

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
`check-asset-hashes.py`; the D17 reader, serializer, and publication benches
against the pre-M0 commit, which becomes the cumulative baseline for every
later milestone; `scripts/measure-build.py` within the D9 budget.

Risks: lost cross-crate inlining in debug and test builds (release keeps thin
LTO and one codegen unit, `Cargo.toml` `[profile.release]`); `pub(crate)`
items and `#[cfg(test)]` helpers used across new crate boundaries (for
example `src/runtime/test_support.rs`) need explicit `pub` or a
`test-harness`-gated module; MSVC link time for one test binary per crate in
the native Windows gate (measured under D9).

Outcome: M0 landed on `main` from `3a91ff9` (the pre-M0 baseline) through
`fda1625`; results are in the [benchmark guide](../testing/benchmarks.md).
Every exit item passed except one D9 probe: a `vosu-osu-state` edit's
`cargo check` measured 1.031 s against the 1.0 s goal. The owner accepted
that miss, because most of the time is re-checking the root `vosu` crate,
which shrinks as `api` and `server` move out in M1–M4. The
native Windows CI regression the risk above predicted (full PDB debuginfo
across the new test binaries) was fixed by building that job with
line-table debuginfo.

### A1: ABI and spec amendments (on `v2/main`, documentation only)

Scope: the adopted amendments in section 10, written into
`include/vosu_plugin.h`, [plugin ABI](plugin-abi.md), [state schema](state-schema.md)
and [`schemas/`](schemas/), [API v1](api-v1.md), [catalog v2](catalog-v2.md),
and ADRs 0019 and 0021: `VOSU_OP_PUBLISH_BATCH` and the unknown-flag rule
(D4); `VOSU_READ_PLAN_SYNC` (D5); `VOSU_OP_SESSION_DECLINE` (D6);
`x-visibility` on `vosu.osu` `/internal` and the per-variant path fields
(D2, D8); the bundled-only HTTP surface, multi-crate source layout, and
adapter-name amendments to ADRs 0019 and 0021 (D3, D9, D10); the bounded
`[[provides]] schema` profile (D7); the matcher `comm_hints` and runtime
overlap rule (D18); the catalog-v2.md wording for user overlays (D20); the
`ConcurrentPlugin` SDK sketch (D22); `docs/v2/naming.md` (gap 8).

Exit: `python scripts/check-doc-links.py`, `python scripts/check-brand.py`;
the owner signs off the amended specs, which are frozen again. Signed off
2026-11-22.

### A1b: ABI and plan amendment from M1-foundations findings (on `v2/main`, documentation only)

Scope: addendum to A1, closing the two findings unit F0 of
`m1-remainder-plan.md` found while breaking the M1 milestone into landable
units (that plan's §9 R1, R2): [plugin ABI](plugin-abi.md) §6.2
`VOSU_OP_SESSION_ATTACHED` gains `name`, `path`, `command_line`, and an
optional Linux-only `argv` (process facts the osu! plugin needs for D6's
argv-based decline and 1.x root resolution; DA, an additive minor revision
under abi:54, no ABI major bump; absent on an older host means unknown,
never the empty string a present-but-empty value would mean); §6.1/§8 gain
a new optional Linux-only `linux_name_contains` matcher field reproducing
1.x's Linux substring name rule that exact `process_names` cannot express,
and the static overlap rule is extended to the new field and to
`comm_hints`-vs-`linux_name_contains` pairs (DB; `comm_hints` itself is
unchanged — still pre-filter only — and Windows is unchanged); §6.4
`VOSU_OP_SCAN_RESULT`'s `addresses` order is pinned to ascending region
then ascending offset, the order core's scanner already produces and
`scan_first` depends on; the `VOSU_OP_SESSION_ATTACHED` comment at
`include/vosu_plugin.h:160` (comment only, no layout change); and, in this
document and `scripts/crate-boundaries.txt`, the crate-dependency table's
re-dated `vosu-osu-reader`/`vosu-osu-state` interim `vosu-sys` edges (now
until M5, DC) and new `vosu-osu-reader -> vosu-net` edge (DD), the M1
scope text's matcher wording, B1 payload list split by direction (plugin →
root vs. root → plugin, DE), and identify timing (DF); the M1 exit
criterion's full `src/instance/tests.rs` coverage, restored after an
over-narrow first edit; and stale `src/instance.rs`/
`src/instance/discovery.rs` references, `docs/v2/api-v1.md:59`, and
`docs/v2/catalog-v2.md:549` this document and its siblings carried from
before the F0–F8 breakdown existed.

Exit: `python scripts/check-doc-links.py`, `python scripts/check-brand.py`;
the owner delegated sign-off on DA–DI (`m1-remainder-plan.md` §4) to the
implementing agent. A1b signed off 2026-11-23 (owner delegated decisions
to the implementing agent).

### A2: state-schema and plugin-ABI amendment from M2 planning findings (on `v2/main`, documentation only)

Scope: closes the spec defects and plan-text gaps `docs/v2/m2-plan.md`'s
planning found while breaking the M2 milestone into landable units (that
plan's §9 R1-R32), bundled per that plan's §4 design decisions (MA-MH):

- [state-schema.md](state-schema.md): adds `vosu.osu.session.state`
  (`$defs/numberName`, `precise`/`always`) beside the existing
  `beatmap.status`, splitting the coarse `state` mapping row (MA, R1); adds
  the `GameState` → `vosu.status.status` table (24 values, nearest-listed
  rule, MB/R12); states the common-layer formulas — `vosu.play.accuracy`/
  `.health` as the 1.x value divided by 100, `vosu.chart.bpm` as the
  rate-1.0 `BpmSummary.common` rounded to 4 decimals, `vosu.chart.length` as
  `beatmap.time.lastObject`, `vosu.chart.keyCount` as `round(cs.converted)`
  for mania (MB, R2-R4); clarifies "during play"/"gameplay" activity as
  `vosu.status.status` in `{play, pause}` (MB/MC, R10/R13); adds the missing
  `client` → `vosu.client.variant`, `server` → `vosu.osu.session.server`,
  and `settings.skin` mapping rows (MD(d,e), R7); notes the new `good` and
  lazer-tourney-client fields and the `vosu.osu` schema bump to `1.2.0`
  (MD(b,c), R5/R6/R31); aligns the sampling-lane prose with every field
  `vosu.play.schema.json` actually annotates (MH); amends the "correctly
  absent" plan-text wording to "`resources` is `{}`, `tree` is `null`" (MH,
  R11); adds `internal.paths.current` documentation, the owner's open
  decision from MD(f) resolved to ADD now (R8).
- [plugin-abi.md](plugin-abi.md): `VOSU_OP_FOCUS_CHANGED` gains an additive
  `"foreground": bool|absent` key, sent only where core has a real
  OS-foreground source (Windows), so `vosu.osu.session.focused` can be
  reproduced without exposing a foreground API (ME, R9); `SESSION_ATTACHED`'s
  process facts (`name`/`path`/`command_line`/`argv`) are omitted entirely
  for a non-owning plugin receiving it through `vosu.sessions` (MG, R17);
  `activity: "gameplay"` now covers `play` or `pause` (MC, R10); the
  publication edge cases abi:199-209 left unspecified — publish-before-
  identify, an unknown/detached/foreign handle, missing intermediates,
  descent into an array/scalar, `NULL` at `/resources`, `RESOURCE_REF`/
  `TREE_REF` before M3, pointer/CBOR limits, NaN and bytes in JSON — are
  now stated (MF, R19); `STATE_SUBSCRIBE`'s unspecified error cases — an
  unexposed/unknown handle, or a non-`vosu.sessions` `session: null`
  selector — answer `send`'s status with `VOSU_ERR_INVALID_ARGUMENT`,
  nothing delivered, and an internal grant's subtree-only granularity is
  stated (MG, R16/R18).
- `include/vosu_plugin.h`: comment only, at `VOSU_OP_FOCUS_CHANGED = 13`
  (ME).
- `schemas/vosu.osu.schema.json`: version `1.1.0` → `1.2.0`, every change
  additive per the schema's own versioning rule — `session.state`/
  `.server`, `result.statistics.good`, leaderboard entry
  `statistics.good`, `$defs/tourneyClientPlay`'s `score`/`accuracy`/
  `combo`/`judgements`, `settings.skin`, and (owner's open decision,
  resolved: ADD now) `internal.paths.current {beatmapFolder, beatmapFile,
  background, audio}` and `internal.paths.skinFolder` — the 1.x
  `folders.beatmap`/`files.*`/`folders.skin`/`directPath.skinFolder`
  sources M4a's upstream adapter must echo,
  consistent with D2/D8 (MD(f), R8).
- `docs/v2/catalog-v2.md`: the `[[provides]]` example's `version` and the
  "additive amendments bump it from there" prose now read `1.2.0` (R31).
- This document: the M2 scope text's "correctly absent" wording and the
  D17 M2 bench definition, both per MH.

Exit: `python scripts/check-doc-links.py`; `python scripts/check-brand.py`;
`jq empty docs/v2/schemas/*.json`; the owner delegated sign-off on every
`[spec]` decision in `docs/v2/m2-plan.md` §4 to the implementing agent,
exactly as A1b's sign-off worked. A2 signed off 2026-11-23 (owner
delegated decisions to the implementing agent).

### A2b: amendment recording M1/M2 implementation decisions (on `v2/main`, documentation only)

Scope: records the approved decisions made while implementing M1 L3 and
M2, in their normative contracts and the M1 outcome below:

- [plugin-abi.md](plugin-abi.md) §6.6: K3 bounds committed state to 4 MiB
  per namespace and 65,536 keys per session; exceeding either returns
  `VOSU_ERR_INVALID_ARGUMENT`, committing nothing.
- Plugin ABI §6.2: K6 cancels an unseen attach+detach pair; more than 4,096
  undelivered lifecycle entries sends `SESSION_DETACHED(lost)` for every
  announced session, then unsubscribes the watcher's lifecycle-session
  subscriptions for non-owned sessions only, with a WARN. Resubscribing
  replays the current list; the owner's final committed state flushes to
  subscribers before its `SESSION_DETACHED`.
- Plugin ABI §4: host lock order is
  `focus -> state_serial -> instances -> sessions -> registry -> subscriptions`.
- Plugin ABI §5: UnknownHandle distinguishes an issued, retired handle
  (`VOSU_ERR_TARGET_GONE`) from a never-issued handle
  (`VOSU_ERR_INVALID_ARGUMENT`).
- Plugin ABI §4: plugin destroy joins lookup workers unconditionally,
  within a 4.5-second budget.
- [state-schema.md](state-schema.md), mapping notes, and
  `schemas/vosu.osu.schema.json`: shared `$defs/statistics.good` is fed
  from `katu`; `rankedPlay.stage` is an integer; `stats.stars.hitWindow`
  is a number. These corrections are already present in the schema.
- State-schema mapping notes and the result schema:
  `result.statistics.sliderBreaks` is always `0`, a known limitation with
  the 1.x v1 precedent at `src/api/v1.rs:692`.
- State-schema mapping notes: unavailable optional stars and hitWindow
  members are omitted, not `null`, as in 1.x (already reflected in the
  schema). All these schema corrections retain unreleased version `1.2.0`.
- State-schema mapping notes: intentional divergence from 1.x's whole
  `/json/v2` HTTP 500 is `vosu.osu.tourney: null` on a tick where any IPC
  client entry has no score.
- State-schema mapping notes: intentional spelling divergence is tourney
  `bestOF` → `bestOf`; the value is unchanged.
- M1 outcome: production `vosu.osu` load/create failure or discovery not
  starting fails daemon startup; there is no half-start.
- M1 outcome: initial discovery logs
  `Initial process discovery completed: N osu! process candidate(s)`.
- M1 outcome: daemon-log forwarding includes WARN/ERROR from all tracing
  targets and INFO from `vosu_*`, limited to 20 lines per callsite per
  second, followed by a suppression summary.
- M1 outcome and state-schema collection contract: 2.0 bus demand enables
  hitOffsets/keyHitErrors reader collection without `ENABLE_*` gates;
  1.x outputs retain the 1.x configuration gates.
- M1 outcome: D17's stable PlanMemory microbench is 2.65x native
  (3.22 vs 1.22 µs per capture, about 71 ns per plan plus 12 ns per op,
  13 plans); the decision remains open pending a live daemon-CPU comparison.

Exit: `python scripts/check-doc-links.py`; `python scripts/check-brand.py`.
The owner delegated spec sign-off; A2b signed off 2026-11-24 under that
authority. D17's live daemon-CPU outcome remains open, not a performance pass.

### A3: contract amendments and frozen M3 interfaces (on `v2/main`, documentation only)

Scope: all M3 plan §4/§10 recommendations, including approval questions 1–7,
are **approved**, signed off 2026-11-24 under the owner's delegated authority.
These are new M3 decisions, not inherited M2 sign-off. Normative homes:

- MJ/R1: [plugin-abi.md](plugin-abi.md) §6.8 adds mandatory full bare
  `ACTION_INVOKE.action`; exact action registry lookup, no last-dot inference.
  F2 owns SDK/wire changes, every caller and two-action fixtures; A3 changes
  no Rust type or C layout.
- MK/ML, R3/R4: [catalog-v2.md](catalog-v2.md#bounded-action-schema-profile-a3)
  freezes the bounded compiled-once action-schema profile, unsupported-keyword
  rejection and budgets. No runtime `jsonschema`; dev-only differential oracle.
  API/ABI validate before job/dispatch and before publishing success, with
  `invalid_arguments`/`invalid_result`; default core deadline is 30 seconds.
- MH/R15: API Id registry and ABI §6.5 require canonical indexed `entry_path`
  proof for aliases/fallback, exclude truncated-away entries, retain confinement.
- MI/R16 (amended 2026-11-24): mutable assets use cold-prepared disk-backed
  metadata identity, not byte snapshots. Unix identity includes dev/ino, size,
  nanosecond mtime/ctime; Windows includes volume serial, file id, size,
  LastWriteTime and FILE_BASIC_INFO.ChangeTime from the opened handle.
  Re-verify immediately before and after disk reads; a mismatch yields 410
  and the next publication mints a new id. No steady file-byte retention in RAM.
  Residual risk is a privileged Windows actor resetting ChangeTime together
  with LastWriteTime; this is not a cryptographic content hash.
- ME/R14/R17/R18: API/ABI freeze transient-free restart identity, multi-owner
  membership, deterministic CBOR/duplicate keys, request-owned stream leases,
  exact cleanup and re-resolution after close. MF's prepared binding transaction
  changes membership only with an accepted atomic state commit.
- MM/R8/R9/R10 and MN/R11/R25: catalog records the transitional v2 tool loader,
  fail-closed manifest preference and installed-provider-only grants. API
  records exact live launch-session ownership, credential ordering, unchanged
  peer/Origin/read policy, private CLI metadata/session routing, no new public
  discovery route and no action API-key scope.
- MO/R12: catalog Beatmap import action freezes local `.osz` `sourcePath`,
  bounded validated source retention, selected-client non-overwriting stable
  queue/lazer submission, cancellation boundary and exact completion results.
- MP/R26/R29/R30: D1 activates root application `2.0.0-alpha.1`, updates its
  lockfile package entry and host-range docs, not independently versioned SDK.
  Executable semver alone selects the channel, never debug mode/sys crate
  version. CLI/daemon share pure resolution before any migration/initialization.
  Every config/data/cache root and runtime/broker root gains `prerelease/`;
  system cache, logs/support, plugins/offsets and writable static/output roots
  follow it. Normal release roots remain unchanged. The first alpha start may
  import 1.x settings read-only once, never run the mutating initializer or
  normal-root directory migration there, and never reread that source later.
  Automatically imported writable paths into normal roots are rebased/excluded.
  A deliberately configured user override may leave isolation, but must be
  explicit in alpha's own config and documented as such; copied paths are not
  overrides. Malformed source remains untouched. R26 amends the M3 exit below.
- R5/R6/R20/R21/R22/R23/R24: API freezes initial no-focus `active` as HTTP 404,
  upgraded active absence/rebind, final fixed-id removals then normal `1000`
  closure, 416 `invalid_range`, 502 `provider_failed`, 504
  `provider_timed_out`, GET-only tree index, DELETE 200/current job and no-store
  action/job responses. Session documents transition coherently; stream hub
  revisions are monotonic across rebinding, independent of state revisions.
  Explicit pointer limits and decoded-token overlap govern.
- R7: M3 `plugin_crashed` proves the production instance-loss seam. M5 proves
  actual subprocess crash/kill and process cancellation grace; in-process
  execution is not process-crash evidence.
- R36: ABI §4 extends A2b's lock order with resources/invocations/jobs;
  reserve, dispatch after unlock, then complete; no plugin callback under locks.
- R33/R35/MQ: [verification](../testing/verification.md#m3-api-resources-and-actions)
  freezes feature-bearing commands, nonzero test inventory and lifecycle/privacy
  counterexamples. [Benchmarks](../testing/benchmarks.md#m3-d17-contract)
  freezes M3 named cases, common-harness comparisons, cold identity costs and
  live idle/subscribed evidence. M1 PlanMemory CPU and M2 performance findings
  remain open until measured, not erased by M3 functional success.

#### Frozen shared interface contracts

The signatures below are Rust interface contracts for the owning units, not
implemented declarations or an extra compatibility layer. Owners may choose
module paths/private storage, but must preserve these types, ownership and
operations; changed shared shapes require an explicit amendment. `Result<T, E>`
uses each domain's structured error, never raw provider paths/credentials.
Opaque ids are typed newtypes, not interchangeable strings.

**S0/S1 — public state (S0 implementation amendment).**
`SessionTarget = Active | Id(String)`;
`ApiPlan::new(&[impl AsRef<str>]) -> Result<Self, ApiError>` preserves caller
order/absence, enforces 1–256 nonoverlapping pointers, 512 bytes and 32 decoded
tokens, and precompiles namespace-relative demand (including future public
namespaces for `""`). `ApiPlan::pointers()` exposes the ordered validated
pointers; `ApiPlan::lanes(&SchemaRegistry)` returns their schema-derived union.

The implemented names replace the proposed `CompiledApiSelection`,
`SelectionError`, `ApiSnapshot`, `SessionSummary` and `ApiStateError` shapes;
there are no compatibility aliases. `ApiError` distinguishes `UnknownSession`,
`Closed`, `Capacity` and `InvalidSelection(String)`. A summary is a `StateValue`
with API's exact core-owned `vosu.client` object shape. `sessions()` is fallible
so a non-owning adapter reports shutdown rather than silently returning an empty
live-session list.

```text
ApiUpdate {
    session: Option<String>,
    revision: u64,
    binding: u64,
    values: Vec<Option<StateValue>>,
    lanes: LaneBits,
    ended: bool,
    error: Option<ApiError>,
}
ApiStateSource::sessions(&self) -> Result<Vec<StateValue>, ApiError>
ApiStateSource::session(&self, target: &SessionTarget) -> Result<StateValue, ApiError>
ApiStateSource::read(&self, target: &SessionTarget, plan: &ApiPlan)
    -> Result<ApiUpdate, ApiError>
ApiStateSource::subscribe(&self, target: SessionTarget, plan: ApiPlan)
    -> Result<ApiSubscription, ApiError>
ApiStateSource::subscribe_sessions(&self, plan: ApiPlan)
    -> Result<ApiSubscription, ApiError>
ApiSubscription::current(&self) -> Arc<ApiUpdate>
ApiSubscription::changed(&mut self)
    -> impl Future<Output = Result<Arc<ApiUpdate>, ApiError>> + Send
ApiSubscription::updates: watch::Receiver<Arc<ApiUpdate>>
```

`values` is a full selected-value snapshot in plan order, not a delta;
`None` means absent, distinct from `Some(StateValue::Null)`. Immutable
`StateValue` trees retain shared subtrees and use the existing API JSON encoder
only at the transport boundary, rather than serializing whole JSON documents on
every core notification. This replaces the proposed `Arc<serde_json::Value>`
document while preserving slow-consumer correctness. The public watch receiver
also permits the transport hub to use native watch operations directly.

`revision` always means the source notification sequence, including on reads;
it is never a session publication revision. It can remain zero without
observers, and a read may see committed state ahead of its notification.
The transport hub owns its separate monotonic wire sequence. `binding` changes
on every active rebind, including when a slow consumer skips focus away and
back to the same session with identical selected values; S1 compares it to
reset the baseline. Session-document bindings are always zero. `ended` is
sticky terminal state: fixed-id detach sends absent selections then terminates;
shutdown and unrecoverable capacity errors also carry `error`. These fields
preserve reset/close reasons that latest-only value snapshots alone cannot.
`lanes` is the current selection's producing-lane union.

`ApiSubscription` owns latest-value reception and a demand drop guard;
`SubscriberId::Api` is distinct from plugin/surface consumers. Drop removes
demand on every exit. Reads use `Audience::Api` with no demand. Upgrade-time
validation calls `session(target)` (404 if absent). Subscription establishment
can subsequently bind `Active` with no focused session: all selected values
are absent, with no demand, until focus returns. Fixed ids must still resolve
at establishment. Session events use one coherent lifecycle document. No client
means no periodic API task, JSON construction or per-publication clock query.

**F0 — retained confinement (vosu-sys).**

```text
SafeRelativePath::parse(decoded: &str) -> Result<Self, SafePathError>
SafeRoot::open(path: &Path) -> io::Result<Self>
SafeRoot::open_child(&self, path: &SafeRelativePath) -> io::Result<SafeChild>
SafeChild::file(&self) -> &File
SafeChild::metadata(&self) -> &Metadata
SafeChild::into_file(self) -> File
```

The parser consumes already-decoded text exactly once. Roots are retained
directory capabilities, child metadata/identity comes from that same open
object, never canonicalize then reopen. Windows uses normalized component
containment; Unix rejects Windows-looking absolute roots. HTTP policy remains
above this single opener; blocking operations run off Tokio workers.

**F4/R1 — identity and prepared membership.** `ReferenceIdentity` contains
plugin id, reference class, kind, original CBOR key, name and CBOR content_id;
`ResourceId` is the authenticated public token. `ReferenceBinding` identifies
session, namespace and pointer, with an owning plugin-instance generation.

```text
ResourceSecret::open_or_create(path: &Path) -> Result<Self, ResourceError>
ResourceRegistry::prepare(&self, changes: &[ReferenceBindingChange])
    -> Result<PreparedReferenceBindings, ResourceError>
PreparedReferenceBindings::references(&self) -> &[ProjectedReference]
ResourceRegistry::commit(&mut self, prepared: PreparedReferenceBindings)
ResourceRegistry::lookup(&self, id: &ResourceId) -> Result<ResourceRecord, ResourceError>
ResourceRegistry::retire_session(&mut self, session: &SessionId)
```

`ReferenceBindingChange` is replace/remove-subtree, not only leaf removal.
Preparation is bounded and fallible; dropped preparation has no visible effect.
Prepare and commit are serialized by `state_serial`; successful preparation
reserves all capacity so commit is infallible and occurs only after final state
validation, atomically with state. Lookup verifies authenticity first and returns
unknown versus unpublished distinctly, with an owned live-owner snapshot.
Initial centralized limits: 4,096 records, 16 MiB retained reference/key metadata,
bounded resolution/index caches and pending operations/streams. Shared ownership
counts one identity; pressure diagnostics and 410 eviction are explicit. Secret
creation is atomic/private; malformed/unreadable existing secret fails startup,
never silently rotates. Composition supplies its channel-aware storage path.

R1 integrates these bindings through `state::ReferenceStore` and
`StateStore::with_resources` (`StateRuntime::with_resources` for host
composition). Production supplies one persisted secret and shared registry;
`StateStore::new` retains process-ephemeral identity only for non-public hosts
and tests. `ReferenceStore::is_persistent` exposes the secret's actual provenance
for the public-serving composition check. The lock order is session commit,
then reference registry; state swap and membership commit precede unlock, and
revision notification follows unlock. Typed reference publication fixes
`ReferenceIdentity::media_type`; ordinary CBOR cannot forge a core reference.

**R0–R3 — lazy requests and leases.** `ResourceOwner` identifies exact plugin
instance, session handle and generation; `ResourceRequest` is resolve/list/
lookup/read with the ABI payload for that kind, and `ResourceReply` is the
corresponding validated ABI variant (lookup success includes `entry_path`).

```text
ResourceRequests::request(&self, owner: ResourceOwner, request: ResourceRequest)
    -> impl Future<Output = Result<ResourceReply, ResourceError>> + Send
StreamLease::read(&self, offset: u64, len: u32)
    -> impl Future<Output = Result<Bytes, ResourceError>> + Send
StreamLease::close(self)
```

Host ids are unique for the instance lifetime; reserve before unlocked dispatch.
The pending facility stores expected reply kind/generation, deadline and
cancellation guard; dropping its future removes the entry. `StreamLease` is
non-cloneable, closes exactly once on explicit close or drop and invalidates its
resolution; no stream survives request ownership or crosses requests. Late
replies cannot leak returned streams. File/index cache owns only successful
validated results; teardown does not invoke callbacks under registry locks.

**F2/F3/J0/J1 — actions, principals, invocations and jobs.**
`ActionSessionMode = Required | Optional | None`;
`ActionDescriptor` contains full bare id, provider instance, version/title,
session mode, cli flag and shared compiled argument/result schemas.
`Principal = Tool(LaunchSessionId) | Broker | Plugin(PluginInstanceId)` carries
no cookie/raw token; composition authenticates and core rechecks authority.
`InvocationPolicy` owns `deadline: Duration` (default 30 s).
`ACTION_CALL` targets another plugin's declared action; reentrancy safety means a callee can call back into its caller during dispatch, not permission for self-calls.

F3 foundation: `vosu_core::action_schema` (since F S0 the SDK's
`vosu_plugin_api::manifest::action_schema`, shared with `vosu-package-check`)
implements the A3 profile
and centralized schema/value/work limits without a runtime schema-engine
dependency. `LoadedPlugin::compiled_action_schemas()` retains compiled assets
by exact manifest path; both bundled and development loading reject malformed
or unsupported schemas before plugin code is entered. Argument and result
validation return pointer-only `ValidationError`s; successful validation
constructs no instance-pointer strings. The root `action_schema` integration
test uses the pinned dev-only `jsonschema` oracle with document-local refs.
Invocation/job integration remains J1's responsibility.

```text
ActionRegistry::get(&self, action: &str) -> Option<Arc<ActionDescriptor>>
CompiledActionSchema::compile(bytes: &[u8]) -> Result<Self, SchemaError>
CompiledActionSchema::validate_arguments(&self, value: &Value) -> Result<(), ValidationError>
CompiledActionSchema::validate_result(&self, value: &Value) -> Result<(), ValidationError>
trait InvocationSink: Send + Sync {
    fn progress(&self, progress: ActionProgress);
    fn finish(&self, result: InvocationResult);
}
InvocationExecutor::invoke(&self, principal: Principal, action: Arc<ActionDescriptor>,
    session: Option<SessionId>, arguments: Value, sink: Arc<dyn InvocationSink>)
    -> Result<InvocationId, InvocationError>
InvocationExecutor::cancel(&self, invocation: InvocationId)
JobRegistry::create(&self, owner: Principal, action: String, session: Option<SessionId>)
    -> Result<Job, JobError>
JobRegistry::get(&self, principal: &Principal, id: &JobId) -> Result<Job, JobError>
JobRegistry::cancel(&self, principal: &Principal, id: &JobId) -> Result<Job, JobError>
JobRegistry::wait(&self, principal: &Principal, id: &JobId, after: u64, wait: Duration)
    -> impl Future<Output = Result<Job, JobError>> + Send
```

`Job` is the exact API object (id/action/fixed session/state/revision/progress/
result/error). A job sink applies progress/terminal events to its registry;
a relay sink remaps caller/callee ids without making an HTTP job. Both enforce
first-terminal-wins and validate results. Waiter guards release on every exit;
caps are 8/job, 256 total; saturation returns current job. Jobs retain at most
1,024 running/terminal entries, terminal retention 5 minutes, oldest terminal
evicted first, never a running job. Background tasks hold `Weak` ownership,
cancel/join safely and never join themselves. All outstanding queues/maps,
including cancelled invocations and relay terminal outboxes, stay bounded;
slot release follows ABI §6.8/§6.12, not receipt of cancellation.

Exit: `python scripts/check-doc-links.py`; `python scripts/check-brand.py`;
`jq empty docs/v2/schemas/*.json`; rustdoc with denied broken intra-doc links.
No VM or production behavior change is part of A3.


### M1: plugin API, in-process host, osu! plugin behind the 1.x surfaces

Scope:

- `vosu-plugin-api`: plugin-abi.md §1–§5 and §9 as amended by A1 and A1b;
  layout
  tests asserting every size and offset in the §3 table with
  `core::mem::offset_of!`, and a C translation unit with `_Static_assert`s
  on the same table compiled from a test, so header and Rust cannot drift;
  CBOR codecs over `ciborium` (D13); flat payload validation (offsets inside
  the buffer, 64 KiB `VOSU_READ_PLAN_MAX_BYTES`, the universal 1 MiB
  `VOSU_FRAME_MAX_BYTES` every opcode shares (F4; `VOSU_PUBLISH_BATCH_MAX_BYTES`
  is an alias of it, not a cap of its own) and the narrower
  `VOSU_STREAM_CHUNK_MAX_BYTES`, alignment, unknown flag bits rejected); `Plugin`
  (`&mut self`, serialized by an SDK lock) and `ConcurrentPlugin` (`&self`,
  `Sync`) with `export_plugin!` producing `vosu_plugin_entry_v1` for
  `cdylib` builds (D22); `PlanMemory` behind `unstable-sync-reads` (D5;
  S0 later moved it to the `publish = false` `vosu-plugin-internal`, AF.9).
- Core plugin host: the explicit `BUNDLED` array in the root crate and
  external `dlopen` (`libloading`, D13); manifest `kind = "plugin"` runtime
  fields (plugin-abi.md §8) for bundled and development plugins
  (`--plugin <dir>`, the analog of 1.x `--tool <dir>`; trusted in-process in
  debug builds only); dispatch thread per instance and the
  `concurrent_dispatch` pool (§4); error model (§5); matching (§6.1) with
  the D18 overlap rule; `SESSION_DECLINE` (D6); session lifecycle,
  identification, the 5-second hidden rule, focus rule and notifications
  (§6.2, api-v1.md "Focus rule"); read-plan executor (§6.3) over `vosu-sys`,
  including `VOSU_READ_PLAN_SYNC` delivery on the submitting thread with its
  deadline (D5); scanning (§6.4) over `src/process/scanner.rs`; settings
  (§6.9); logging with the 200/s limit (§6.10); timers (§6.11).
- Discovery moves into core, parameterized by enabled plugins' matchers.
  The event backends' hard-coded `osu!`/`osulazer.exe` substring filter
  (`src/instance/events.rs:150-155`,
  `src/instance/events/bpf/process_events.bpf.c:76-92`) becomes a bounded
  BPF map and CN_PROC set filled from enabled plugins' `comm_hints` and
  `linux_name_contains` entries (D18, A1b/DB),
  so the measured in-kernel filtering that `AGENTS.md:130` requires is kept.
- `vosu-osu`: matchers `process_names = ["osu!.exe", "osulazer.exe"]`/
  `process_path_patterns`, a pre-filter-only `comm_hints` (unchanged, D18),
  and a new Linux-only userspace matcher
  `linux_name_contains = ["osu!", "osulazer.exe"]` (plugin-abi.md §6.1 as
  amended by A1b/DB); `SESSION_DECLINE` reproduces
  `should_skip` only (`src/instance/discovery.rs:118-123`: `--tournament`
  and `debug-client-id`); the Linux conditional fallback
  (`src/instance/discovery.rs:73-107`, which depends on other processes and
  cannot be a full-match regex, plugin-abi.md:115) is expressed as
  `SESSION_IDENTIFY`'s `variant` instead, never a decline, since 1.x never
  skips those processes. Identify timing (DF): every non-fallback candidate
  (a `name` containing `osu!.exe`/`osulazer.exe`) is identified at once; a
  Linux fallback candidate (a `name` containing `osu!` but neither of
  those) waits one host-minimum one-shot "settle" timer so siblings
  attached in the same discovery pass are seen before its variant is
  fixed, matching 1.x's named-pass-first rule; `version` is the lazer
  `osu!.deps.json` version, or `""` for stable (m1-remainder-plan.md §9
  R10; nothing exposes `vosu.client` before M3). `-spectateclient N`
  processes are identified as
  ordinary sessions with their IPC id in `vosu.osu.session.ipcId`, as 1.x
  attaches them (`src/instance.rs:66-83`, D6); the lazer offset-resolver
  lifecycle moves from `src/instance.rs` into the plugin on
  `SESSION_ATTACHED`; reads through `PlanMemory` with the synchronous flag,
  so the 1.x readers stay unchanged, always from the plugin's own reader
  threads, never a dispatch thread; `ConcurrentPlugin` with the stress test
  that plugin-abi.md §10 requires.
- Settings: the five osu!-specific keys become `vosu.osu` settings declared
  by its `settings_schema` (D19). **Interim bridge B2** (deleted in M8):
  core serves the plugin's settings from the existing `vosu.toml` values
  until the M8 migration moves them into plugin settings storage.
- **Historical M1 bridge B1** (removed by M4a): byte-identical 1.x surfaces needed
  more than `StateBundleSnapshot`
  (DE). Plugin → root: `StateBundleSnapshot`; per-session lifecycle phase
  (dashboard status "resolving offsets"/"starting reader"/"reading",
  `src/runtime/config.rs:368-386`); `InstanceSettings` (ipcId, spectator,
  devserver). Root → plugin (`B1Inputs`, `LegacySink::inputs()`): the four
  non-D19 configuration keys named below (`ENABLE_KEY_OVERLAY`,
  `ENABLE_MANIA_KEY_HIT_ERRORS`, `ENABLE_HIT_OFFSETS`,
  `V2_SUBSCRIPTION_DEBOUNCE`) and the V2 subscription demand
  (`src/runtime/worker.rs:432-447`). The 1.x focus reduction
  (`src/instance.rs:702-721` + `src/runtime/worker.rs:873-896`, which
  differs from the api-v1 Focus rule core implements) stays root-side, in
  `bridge_b1.rs`, computed from the foreground source and the identified
  sessions it already has — it is never handed over by the plugin. All of
  this travels through B1 and dies with it in M4a. The plugin's half goes
  to the existing `SnapshotManager` (`src/api/snapshot.rs`) through a
  `vosu-surface` hook, recorded as the interim `vosu-osu → vosu-surface`
  edge, so every 1.x route keeps serving unchanged bytes.

Exit: `cargo test --locked -p vosu-plugin-api` (layout, codecs, `prop_*`
bounds tests, unknown-flag rejection); `cargo test --locked -p vosu-core` host
tests (trust refusal of `in-process` for an ineligible plugin, matcher
overlap at validation and at runtime, `SESSION_DECLINE` re-offering a PID
generation to the next matching plugin in install order and being refused
  after `SESSION_IDENTIFY`, identification timeout, focus delivery order,
  `focus_priority` held by the first session to set it and ignored (logged
  once, not refused) on a second session's `SESSION_IDENTIFY` while it is
  held, then picked up by the next session to set it once the holder
  detaches, read-plan
validation including a dependent stage whose `ref_op` length differs from
the pointer width, `VOSU_READ_PLAN_SYNC` refused on a dispatch thread and
timing out with `VOSU_ERR_TIMED_OUT`, log rate limiting, timer
cancellation); a new `tests/plugin_host.rs` that builds the testkit as a
`cdylib` and loads it in-process on Linux and Windows; discovery parity:
every case in `src/instance/tests.rs` (`:184-319` hold the classification
cases; `src/instance/discovery.rs` has none)
reproduced through matchers plus decline,
including spectator clients selected by lowest PID
(`src/instance.rs:702-721` + `src/runtime/worker.rs:873-896`); all M0 exit tests unchanged;
`cargo test --locked --test process_memory -- --nocapture` on both OSes; the
Linux process-event examples (`examples/process_event_bench.rs`,
`examples/process_event_lazer_bench.rs`, `Cargo.toml` `[[example]]`
entries) run natively to show the in-kernel filter still drops
non-matching execs; D17 M1 benches.

Risks: the 1.x readers issue many sequential reads, one plan each. D5 keeps
each on the calling thread; if D17's `PlanMemory` reader benches miss the
bar, convert the hottest chains (brokered-reads.md "Plan shapes") to
dependent multi-stage plans before M2.

#### M1 outcome

Production `BUNDLED` now contains `vosu.osu`; generic discovery belongs to core
and osu!-specific classification, resolvers and capture belong to the plugin.
The 1.x `src/instance.rs` and `src/instance/` runtime and tests are deleted.
B1 alone retains the root-side legacy selection/publication contract until
M4a; B2 retains the configuration-to-plugin-settings bridge until M8.

Startup is fail-closed: required `vosu.osu` load/create failure, a missing
required plugin, or absent discovery fails composition. Zero candidates is
valid. The first successful scan logs
`Initial process discovery completed: N osu! process candidate(s)`.
Operational attachment/phase/detach logs remain available, and the daemon
tracing subscriber forwards WARN/ERROR from all targets and INFO from
`vosu_*` targets into `Logger`, limited to 20 lines per callsite per second,
followed by a suppression summary.

For the 2.0 bus, demand enables the hitOffsets/keyHitErrors reader collection
without an `ENABLE_*` gate. The 1.x outputs still honor their 1.x config flags.

A1b was signed off on 2026-11-23 under the owner's delegated authority:
attached process facts (`name`, `path`, `command_line`, optional Linux `argv`),
Linux-only `linux_name_contains` (not a change to pre-filter-only `comm_hints`),
ascending-region/offset scan ordering, and the DA–DI decisions above are the
implemented contract. Linux fallback identification settles once against
same-pass siblings and is fixed for the generation; stable identifies with
version `""`, lazer with its deps version. The plugin owns its runtime.
Reader/state `vosu-sys` edges remain until M5 (DC); reader `vosu-net` owns
hardened generator extraction (DD). Offsets now use
`<data>/plugins/vosu.osu/offsets`; old `<cache>/offsets` is not migrated (DH).
M3 D1 activates the root application at `2.0.0-alpha.1`, independently of the
SDK version. Host ranges intended for this alpha must explicitly include its
prerelease tuple (for example `>=2.0.0-alpha.1, <3.0.0`); `>=2.0.0` and the
old `>=1.2.0, <3.0.0` range do not match it under semver prerelease rules.
M8 still owns the final `2.0.0` release; range enforcement belongs to M6a.

D17 is **not a pass** for M1. On the final measured source `1162fe6`, stable
regular, stable precise (fixture), lazer regular and lazer precise exceed the
10% bar on native Windows. Zero read errors were recorded on every run. The
owner granted a documented exception on 2026-11-24 for every missed row. It
permits the M1 landing on `v2/main` only; D17 remains a hard release gate.
Main owns the M4a X1 attribution and re-measurement of the unattributed
residual and the M5 process-runtime re-check. Each over-10% row must close
there or get a fresh explicit exception. Numbers, attribution and the
exception text are in the [benchmark guide](../testing/benchmarks.md#m1-planmemory-paired-d17).

The following parity map is the replacement evidence cited by C4's
`Assertions-Removed` trailer. Old names are from `src/instance/tests.rs`
(M1 remainder plan §7); replacement names are present at this cutover.
Paths are relative to the repository root. M4a replacements below supersede
the deleted bridge-specific entries; the M1 bridge description above is
historical, not an allowed fallback.

| Deleted 1.x test | Replacement tests at cutover |
|---|---|
| `windows_names_and_bitness_classify_and_btree_orders` | `tests/discovery_parity.rs`: `manifest_matches_both_platform_name_tables`, `windows_names_and_bitness`; host diagnostics remain PID-ordered |
| `targeted_discovery_inserts_only_the_requested_pid` | Core discovery `tests/cadence.rs`: `targeted_discovery_inserts_only_the_requested_pid` |
| `targeted_discovery_replaces_a_dead_same_pid_generation` | Core discovery `tests/mod.rs`: `reused_pid_with_a_new_start_time_is_reported_as_exit_then_a_fresh_appearance`; `tests/discovery_parity.rs`: `explicit_exit_then_appearance_advances_plugin_generation` |
| `linux_named_first_pass_is_stable_and_fallback_is_lazer` | `tests/discovery_parity.rs`: `linux_named_sibling_and_lone_fallback_variants` |
| `command_filters_follow_presence_and_truthiness` | `tests/discovery_parity.rs`: `command_filters_decline_and_suppress_reoffers`; `tests/osu_plugin.rs`: `debug_clients_decline_without_publication` |
| `spectator_and_devserver_settings_are_typed` | `tests/discovery_parity.rs`: `spectator_devserver_settings_reach_bus`; `tests/osu_plugin.rs`: `stable_session_settings_and_exit` |
| `duplicate_discovery_and_removal_are_idempotent_and_close_once` | `tests/discovery_parity.rs`: `duplicate_discovery_and_removal_close_memory_once` |
| `tournament_precedence_matching_and_fallback_selection` | `tests/osu_plugin/tournament.rs`: `stable_manager_priority_aggregate_all_sessions_and_generation_replacement` |
| `foreground_exact_match_and_unavailable_focus_fallback` | `src/server/bus_provider/tests.rs`: `production_focus_generation_prevents_numeric_revision_collision`; permanent core Focus tests |
| `lifecycle_snapshots_are_pid_ordered_and_semantic_exactly_once` | `tests/discovery_parity.rs`: duplicate attach/removal and fresh bus-generation assertions; `src/runtime/tests/context.rs`: host lifecycle/dashboard shape |
| `lazer_discovery_starts_one_resolver_and_shutdown_joins_it` | `crates/vosu-osu/src/plugin/tests.rs`: `lazer_identify_starts_one_resolver_and_detach_joins_it`, `destroy_joins_every_live_sessions_resolver_within_the_grace_period` |
| `discovery_runtime_publishes_current_then_closes_after_joined_shutdown` | Host lifecycle/bus tests above; `src/runtime/tests/lifecycle_owner.rs`: `bundled_osu_discovery_and_real_runtime_shutdown_are_joined` |
| `failed_full_scan_retries_early_and_counts_only_success` | Core discovery `tests/cadence.rs`: `failed_full_scan_retries_early_and_counts_only_success` |
| `failed_targeted_inspection_is_not_counted` | Core discovery `tests/cadence.rs`: `failed_targeted_inspection_is_not_counted` |
| `full_reconcile_replaces_a_dead_same_pid_in_one_pass` | Core discovery `tests/cadence.rs`: `full_reconcile_replaces_a_dead_same_pid_in_one_pass` |
| `live_exit_is_retried_until_the_process_dies_without_another_event` | Core discovery `tests/cadence.rs`: `live_exit_is_retried_until_the_process_dies_without_another_event` |
| `process_event_wakes_are_targeted_and_loss_reconciles_once` | Core discovery `tests/cadence.rs`: `inspect_hint_is_targeted_and_a_loss_flag_reconciles_once_keeping_healthy_cadence` |

Core discovery test paths above are under
`crates/vosu-core/src/plugin/discovery/`. The additional production contract,
spectator clients selected by lowest PID, is covered by B1's tournament/fallback
selection test: manager precedence is retained, then the lowest PID matching
the focused client, with no selection for an absent client. Production manager
identification remains false until the D6 task at M4a or later.


### S1: vosulay supervisor-plugin spike (Linux and Windows, before M5)

Scope (D16): a throwaway official-trust plugin built on M1's SDK, running
in-process, that starts vosulay's existing host executable as a child: on
Windows inside a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
(the `src/tools/process.rs:59-70` pattern), on Linux with
`PR_SET_PDEATHSIG`; it forwards session and focus changes as the child's
input and exposes start/stop actions.

Exit, recorded in [verification](../testing/verification.md): on Linux the
child's `chrome-sandbox` setuid helper works (no `no_new_privs` is
inherited, unlike under `vosu plugin-host`, plugin-abi.md:254;
`vosulay/docs/security.md:58-60`) and the child's `CapEff` is zero although
the daemon holds file capabilities; on Windows the child starts through its
packaged sandbox bootstrap (`vosulay/docs/security.md:265`,
`vosulay/docs/architecture.md:24`); killing the daemon kills the child on
both. A failure reopens D16 before M5 starts.

AF.8 (approved 2026-11-24) amends the forwarding in the scope above for the
production supervisor: the attached child subscribes to API v1 sessions and
the selected session's state itself, as its single data authority; the
supervisor's bounded private IPC carries lifecycle, backend configuration,
the user's chosen-session policy and editor intent, never a second copy of
session or focus state. The spike's stdin writes were never consumed (the
child's stdin reader only drains to EOF) and are not evidence of forwarding.

#### S1 outcome (2026-11-25)

**D16 stands on both operating systems.** M5's S1 prerequisite is closed. The
evidence, artifact hashes, receipts and script defects are in
[verification](../testing/verification.md#s1-vosulay-supervisor-evidence-d16).

- **Windows: PASS (2026-11-24), conditional on a Medium vosulay host.** A
  Medium daemon running the original strict kill-on-close job started real
  packaged CEF: a Medium host, a Low restricted GPU process and an Untrusted
  restricted renderer. Killing only the daemon removed the whole live tree in
  2.156 s. The negative control with assignment disabled survived. The earlier
  High-integrity exit 38 is Chromium's auto-de-elevation code, not a job or
  sandbox refusal.
- **Linux: PASS (2026-11-25).** The daemon held the `vosu --setup` file
  capabilities. The vosulay host had zero `CapInh`, `CapPrm` and `CapEff`, and
  `NoNewPrivs: 0`. CEF zygotes ran in fresh PID, user and net namespaces with
  `NoNewPrivs: 1` and `Seccomp: 2`. After SIGKILL of the daemon alone, none of
  the 13 distinct host and CEF processes survived. The script printed this as
  "0 of 25" because it counted duplicate PIDs.
- **The Linux exit criterion was met differently from its wording.** Chromium
  used its unprivileged user-namespace sandbox, not the setuid
  `chrome-sandbox` named in the exit above. The sandboxed CEF tree was live
  and confined, so D16 holds, but the setuid path is still unexercised (see
  residuals).
- **Evidence classes stay separate.** The 2026-11-23 host-only Linux run, with
  no file capabilities, is death-signal evidence only. The Linux VM daemon was
  a debug build with file capabilities, and no artifact hashes were recorded.
  Its `getcap` output and `/proc` status (`CapPrm`/`CapEff` exactly
  `0xc001080000`, the four setup capabilities, with the child's `CapPrm` and
  `CapEff` at zero) are in `daemon-capability-receipt.txt`, which Main transcribed
  from the same VM session.

Requirements this records:

- **Elevated daemon on Windows.** vosulay's official supervisor must launch
  the host at Medium integrity from an elevated daemon, using a verified
  unelevated token for the same interactive user and session. A missing token
  fails visibly, with no High-integrity fallback, breakaway flag or
  `--no-sandbox`. S1 did not implement or prove this.
- **Session and focus input (AF.8).** S1's stdin forwarding (scope above)
  had no effect, because vosulay's stdin reader only waits for EOF. It is
  superseded by AF.8: the attached child reads session and focus through its
  own API-v1 subscription, which is the only data authority. The supervisor's
  bounded private IPC carries lifecycle, backend configuration, editor intent
  and the chosen-session policy.
- **Ownership.** The production supervisor, the Medium launch and the API-v1
  conversion belong to the vosulay ecosystem owner (F), not M5. M5 must not
  ship shared spawn helpers that contradict this policy.
- **Separate launch policies.** The vosulay child never gets the generic
  plugin-host confinement of plugin-abi.md §7.3.

Residuals, owned by F/vosulay and not M5 blockers:

- setuid-helper evidence on a kernel that forbids unprivileged user
  namespaces (AF.7's separately installed runtime owns that provisioning);
- a capability readback captured in the same evidence directory as the
  CEF/forced-death run, with artifact hashes;
- packaged-release evidence for the daemon.

### M2: state bus and `vosu.*` namespaces

Scope:

- Core state bus: per-session immutable namespace documents published
  latest-only (ADR 0002), typed values (`vosu_value_kind`), one commit per
  `VOSU_OP_PUBLISH_BATCH` validated as a whole (D4), coverage enforcement
  (`covers`, reserved-name `[[resource_kinds]]`, plugin-abi.md §6.6;
  undeclared publication refused), `null` semantics and namespace omission
  (state-schema.md "Namespaces"), `vosu.client` filled by core from attach
  and `SESSION_IDENTIFY` (state-schema.md "`vosu.client`").
- Schema registry: `docs/v2/schemas/*.schema.json` embedded with
  `include_str!` (core namespaces in `vosu-core`, `vosu.osu` in `vosu-osu`)
  as the single source; a hand-written lane walker (no runtime
  `jsonschema`, D7/D13) compiling `x-sampling-lane` demand, generalizing
  `V2SubscriptionHub` demand (`src/server/v2_subscription.rs`,
  state-schema.md "Sampling lanes"); `DEMAND_CHANGED` snapshots (§6.7);
  plugin `STATE_SUBSCRIBE`/`STATE_UPDATE` and `vosu.sessions` events (§6.12,
  §6.2); `vosu-surface` reads through the same subscription, demand, and
  `requires` checks (D3).
- `x-visibility: "plugins"` accepted only on `vosu.osu`'s top-level
  `/internal` key and rejected anywhere else at schema load; one projector
  function strips it from every API-facing document unconditionally, and
  gates `STATE_UPDATE` and `vosu-surface` reads by `requires`/`optional`
  naming `field:vosu.osu.internal` or a descendant of it, never by the
  whole-namespace `field:vosu.osu` (D2).
- `vosu-osu` projects each `StateBundleSnapshot` onto `vosu.status`,
  `vosu.chart`, `vosu.play`, `vosu.skin`, `vosu.osu`, and
  `vosu.osu/internal/paths` (per-variant fields, D8, plus the current
  chart's/active skin's folder and file names, R8), publishing changed
  leaves as one batch per sample, following state-schema.md "1.x-to-2.0
  field mapping", "`keys`", "`skin`", and "Common-layer coverage";
  `vosu.osu.session.ipcId` carries the spectator IPC id (D6) and
  `vosu.osu.tourney` is published exactly where 1.x builds it — `null` on
  the lazer multi-spectating path before the first lobby read, populated
  `clients[]` once that read lands, 1.x's default empty object otherwise —
  with only the stable manager aggregate deferred as a temporary
  pre-release gap (D6, M4a-or-later task below). Resource references arrive in M3; until then the
  manifest declares no `[[resource_kinds]]`, so `vosu.chart.resources` is
  always `{}` and `vosu.skin.tree` is always `null` (R11), never absent.
- Bridge B1 still feeds the 1.x surfaces; reader demand is the union of the
  1.x hub's demand and the bus demand.

Exit: new `tests/state_bus_compat.rs`: for every case in
`tests/fixtures/oracle/inventory.json` (its `required_coverage` list,
`tests/state_api_compat.rs:46-74`), the published documents validate against
their schemas (`jsonschema`, dev-only, `default-features = false`) and every
mapped value equals the 1.x `/json/v2` or precise value at its 1.x path —
except `stable-tournament`'s `tourney` members (`v1`, `v2`, `v2-precise`),
exempted until the stable manager aggregate lands (D6, M4a-or-later task,
recorded as a `temporary` divergence in
`tests/fixtures/upstream/DIVERGENCES.md`, G1); the
`internal` subtree is absent from every API-facing projection — whole
document, whole namespace, `""` and `/vosu.osu` subscriptions and their
encoding caches, session events, and debug logs — and `STATE_UPDATE`/`vosu-surface` reads
are gated by `requires`/`optional` naming `field:vosu.osu.internal` or a
descendant, never granted by `field:vosu.osu`; a concurrent regular/precise
publisher test proving no snapshot mixes two batches.
`cargo test --locked --test state_api_compat` and `--test server_compat`
unchanged; `--test performance_invariants`; the D17 M2 benches below.

Risks: whole-array republication of growing hit-error arrays (§6.6 publishes
arrays only as whole CBOR subtrees) may cost more than 1.x's shared vectors;
D17's end-to-end bench decides whether an additive append form is needed.

#### M2 outcome

The state bus and production osu! projection now publish side by side with
B1. Core owns atomic bounded commits, client facts, schema-lane demand,
audience projection and ABI/bundled subscriptions; `vosu-osu` owns
projection, changed-value batches and the union of bus/B1 reader demand.
This does not claim M3's public transport or resources have landed:
`vosu.chart.resources` remains `{}`, `vosu.skin.tree` remains `null`, and
B1 continues serving the 1.x APIs until M4a.

The implemented contract includes the approved [A2](#a2-state-schema-and-plugin-abi-amendment-from-m2-planning-findings-on-v2main-documentation-only)
and [A2b](#a2b-amendment-recording-m1m2-implementation-decisions-on-v2main-documentation-only)
decisions:

- A2 separates precise `session.state` from beatmap status, defines all
  `GameState` common statuses and play/pause activity, and fixes common-layer
  formulas: accuracy/health divided by 100, rate-1 BPM rounded to four
  decimals, chart length from the last object, mania key count from rounded
  converted CS. It adds client/server/settings-skin mappings, `good`,
  lazer spectator score fields and private current-file/skin paths in
  unreleased `vosu.osu` schema `1.2.0`.
- A2 adds Windows foreground facts, strips non-owner attach process facts,
  specifies publication/subscription errors and limits, and requires explicit
  internal-subtree grants rather than a whole-namespace grant.
- A2b records the 4 MiB/namespace and 65,536 keys/session commit bounds,
  unseen attach/detach cancellation, the 4,096-entry lifecycle-loss backstop,
  final-state-before-detach delivery, host lock order, retired-versus-unknown
  handle errors and unconditional lookup-worker joins within 4.5 seconds.
- A2b keeps `statistics.good` sourced from `katu`, integer `rankedPlay.stage`,
  numeric stars `hitWindow`, omission of unavailable optional stars/windows,
  and result `sliderBreaks: 0`. It intentionally maps `bestOF` to `bestOf`
  and publishes `tourney: null` on missing IPC scores instead of failing the
  whole response.
- A2b makes overlapping hit-offset/key-error selectors opt into reader
  collection without `ENABLE_*` gates; the 1.x serializers retain those gates.
  Its M1 startup/logging decisions and open PlanMemory CPU comparison remain
  recorded in the M1 outcome and A2b above.

The authoritative parity map is
[state-schema.md: 1.x-to-2.0 field mapping](state-schema.md#1x-to-20-field-mapping),
with executable coverage in `tests/state_bus_compat.rs` over the oracle
inventory and dev-only offline JSON Schema validation. The exact
[divergence ledger](../../tests/fixtures/upstream/DIVERGENCES.md) lists the
permanent contract differences and the temporary stable tournament-manager
aggregate gap (D6); the latter still requires the M4a-or-later closing task.
`state_bus` covers host/surface atomicity, privacy and lifecycle;
`osu_projection_privacy` covers actual osu! path projection. Commands are in
the [verification guide](../testing/verification.md#m2-state-bus-contracts).
Functional coverage is not D17 performance evidence: M2 measurements and
the regression decision are maintained in the [benchmark guide](../testing/benchmarks.md).

### M3: API v1, resources, jobs, bridge

Scope:

- Routes exactly per [API v1](api-v1.md): "Sessions" with "Focus rule" and
  "Visibility"; "Session events"; "Field subscriptions" (generalizing
  `src/server/v2_subscription.rs`, a separate hub with
  `API_V1_SUBSCRIPTION_DEBOUNCE`); "HTTP state reads"; "Status codes and
  error bodies"; "Resources and trees" with "Id registry" (HMAC from the
  TLS stack already in the tree, D13, under a persisted core secret, bounded
  registry) and "Headers and type allowlist"; the `307` convenience routes;
  "Actions" ("Create a job", "Job object", `/api/v1/jobs/{id}`,
  "Authorization"); "Listener placement, CORS, and limits" (overlay listener
  only).
- Plugin side: `ROOT_DECLARE`, `RESOURCE_RESOLVE`, `TREE_LIST`,
  `TREE_LOOKUP`, `STREAM_*` (§6.5) over ADR 0005 confinement in
  `src/server/files.rs`; `ACTION_*` (§6.8) including relay and id remapping;
  timeouts producing `timed_out`. The osu! plugin declares `songs`, `skins`,
  and lazer file-store roots and the `file`, `audio`, `background`, `set`,
  and `skin` kinds, moves lazer skin lookup from `src/server/files/skin.rs`
  into its `TREE_LOOKUP`, and provides `vosu.osu.beatmap.import`
  (catalog-v2.md:244-249 is the template) so writers never need a writable
  path (D8).
- Bridge: `assets/server/tool-bridge.js` gains `vosu.invoke` and
  `vosu.start` exactly as api-v1.md "Authorization" defines, and an API v1
  `vosu.subscribe` (pointers such as `/vosu.play/combo`, `session` option);
  served at `/tools/_host/v2/bridge.js` (D12). The 1.x tool host and its
  launch sessions remain the source of `X-Vosu-Tool-Session` until M7.
- CLI: `vosux <plugin-id> <action> [json-args]` with `--detach` and
  `--session` through the broker credential. 1.x tool ids cannot contain a
  dot (`src/tools/manifest.rs:82-89`) and plugin ids always do, so both forms
  coexist until M7 removes the tool form.
- Alpha data isolation (D21): prerelease builds use the prerelease
  subdirectory of each vosu root.

Exit: new `tests/api_v1_compat.rs` (every route, status, error body, cache
header, `sandbox`/`nosniff`, type allowlist, range handling, CORS refusal of
`X-Vosu-Tool-Session` and `Authorization`, dashboard-listener refusal,
subscription frames, removed-index absence, 1 MiB limits, close `1011` on a
full pong queue, job states including `plugin_crashed`/`timed_out`, no
`internal` pointer reachable by any route); `cargo test --workspace --lib --all-features --locked prop_`
with new `prop_*` tests for API v1 pointer parsing, resource-id parsing, and
tree path validation; `cargo test --locked --test server_compat` unchanged;
`cargo test --locked --features test-harness --test daemon_smoke` extended to
read `/api/v1/sessions` and fetch a resource; a real-browser test driving
`vosu.invoke`, progress, and `AbortSignal` cancellation against the testkit, run
by `python tests/tool_host_smoke.py --binary "$CARGO_TARGET_DIR/debug/vosu"
--playwright-module "$VOSU_PLAYWRIGHT_MODULE"`; a real `2.0.0-alpha.1` startup/
restart test proving no writes to normal 1.x roots, only a one-time read-only
settings import and no subsequent source reads, with writable-path rebasing;
D17 M3 benches. M3 `plugin_crashed` evidence exercises the production
instance-loss lifecycle seam; real subprocess crash/kill proof remains M5.

Risks: restart-stable ids require honest content identities and stable immutable
snapshots; no old id may silently serve changed bytes. Registry and caches stay
bounded under map-selection churn. Functional coverage is not a D17 pass.

#### M3 outcome

M3 is functionally integrated on `m3/actions` and merged with `v2/main` (M1
L3 plugin reader cutover and M2 state bus). The overlay listener serves every
API v1 route in the [final route inventory](../testing/verification.md#final-api-v1-route-inventory):
sessions and session events, state reads and field subscriptions on their
own hub, signed resources and indexed trees with ranges, validators and 307
convenience routes, and authorized actions and jobs. The osu! plugin declares
its roots and the `file`, `audio`, `background`, `set` and `skin` kinds, owns
the lazer skin lookup, and provides `vosu.osu.beatmap.import`. The v2 bridge
(`/tools/_host/v2/bridge.js`), the dotted `vosux <plugin-id> <action>` form
and alpha data isolation (D21) are in place. Every 1.x route remains served
through B1 until M4a.

The contract-to-test mapping is the [M3 parity inventory](m3-parity.md). At
the exit:

- Linux: the full workspace gate passed (2,517 nextest tests), with workspace
  and Windows cross-Clippy, every repository check, and the tool-host browser
  and dashboard smokes against a `test-harness` build.
- Native Windows (VM): `api_v1_compat` 33/33, `server_compat`, `vosu-core`
  574/574 and `vosu-osu` 196 passed (1 ignored). `vosu-sys` passed 61/61 at High
  integrity. Its two file-symlink tests need `SeCreateSymbolicLinkPrivilege`,
  which `vm-test.py`'s LIMITED task does not hold. `daemon_smoke` is Linux-only,
  so the live campaign below stands in for native daemon evidence.
- Live Windows stable and lazer, using the VM's own test installations and
  an isolated alpha program folder: stable passed 51/51 resource, skin-alias,
  redirect, retirement and legacy checks. Its import was `queued`: one new
  byte-identical archive in the selected Songs folder, with all 436 existing
  files unchanged, and the game imported it on restart. Lazer passed 33 of 38
  checks. All five misses come from the built-in intro map and the built-in
  argon skin, which publish no background and no indexed skin files. The
  legacy routes agree. Its import was `submitted` to the selected executable,
  and the handoff child was reaped. The game did not import the archive
  within 30 s; this is still open.
- Concurrent clients kept fixed ids and independent state. One owner's detach
  left the other session's resources served. A selected-destination import on
  a just-identified stable session returned `target_gone` before that session
  had published its chart facts, so this case is unproven.
- Linux lazer and stable/Wine live runs are pending. Isolated installations
  exist only in the Linux VM, which cannot run while the Windows VM does.

**D17 for M3 is pending** the official pinned benchmark run; the provisional
X1 numbers in the [benchmark guide](../testing/benchmarks.md#m3-d17-contract)
are not a pass. Boundaries: native subprocess crash/kill proof and process
cancellation grace remain M5; the 1.x tool host and its launch sessions,
which still authorize the v2 bridge, are replaced in M7; the D20 notices,
removed-key migration and day-0 gate are M8.

### M4a: upstream adapters as projections

Scope:

- Create the three adapter crates and `vosu-adapter-support`; move the
  builders (`src/api/{v1,sc,v2}*`, `src/api/precise.rs`,
  `src/model/api/*`), the HTTP providers (`src/server/service/providers.rs`,
  including ADR 0004's `LegacyCalculateProvider`), the stable
  `/files/skin/{path}` behavior, and the WebSocket encoders from
  `src/runtime/projection.rs`. Builders take a typed view assembled from
  `vosu-surface` coherent reads of explicitly granted common namespaces,
  `vosu.client`, `vosu.osu`, and `vosu.osu.internal.paths` (D2).
  The streamCompanion adapter computes `mapStrains`, breaks, kiai points, and
  timing points (`src/model/api/sc.rs:132-141`) from `vosu-osu-calc` and the
  chart resource. Core's hub keeps latest-only publication and per-snapshot
  encoding caches (`tests/performance_invariants.rs:509-701`).
- File routes (`/Songs/{path}`, `/files/...`, `/backgroundImage`,
  api-v1.md:507) resolve against the osu! plugin's `ROOT_DECLARE` handles
  (plugin-abi.md:167) through `vosu-surface`, never against a path string
  read from the bus, so ADR 0005 confinement never takes its root from
  published data (D2). The path strings are only echoed in upstream JSON
  fields such as `folders` and `directPath`.
- Remove what ADR 0021 removes: `pressHitErrors`, `releaseHitErrors`,
  `keyHitErrors`, `keys.all`, `catcher`, `hitOffsets`, the `/files/skin/`
  index, `/websocket/v2/subscribe`; lazer `/files/skin/{path}` returns 404;
  the removed configuration keys (`ENABLE_KEY_OVERLAY`,
  `ENABLE_MANIA_KEY_HIT_ERRORS`, `ENABLE_HIT_OFFSETS`,
  `V2_SUBSCRIPTION_DEBOUNCE`) leave the schema and enter the M8 migration
  list. Requests to a removed route are attributed to the requesting overlay
  for the D20 dashboard notice. Delete bridge B1, its interim crate edge,
  and `src/api/snapshot.rs`.
  **M8 migration inventory:** delete exactly the four uppercase TOML/environment
  bindings above, idempotently; preserve `API_V1_SUBSCRIPTION_DEBOUNCE`,
  unrelated unknown keys/tables, overlays, profiles and settings. No new
  replacement flag is written: authorized subscription selectors own demand.
  Removed-route attribution retains at most 128 validated installed overlays
  (LRU, 24-hour inactivity expiry) and one unknown bucket per fixed feature.
  D20 consumes only identity, feature, first/last seen and saturating count;
  unknown requests never acquire a guessed overlay name. M8 day-0 execution
  and M7's migration-guide notice UI remain separate work.
  First-party consumer migration inputs include
  `assets/server/tool-bridge-v1.js` (selected-v2 URL and renderer pointer map),
  the overlay quickstart's selected-v2 client, and consumers of legacy skin
  metadata/lazer files. Their owners move to API v1 namespace subscriptions
  and skin trees/resources at M8; preserving those consumers does not restore
  removed endpoints. Keep user overlays installed and enabled.
- **Stable tournament-manager aggregate (D6, mandatory M4a exit).** The osu!
  plugin attaches the `--tournament` process
  as an identified `stable` session, setting `focus_priority: true` on its
  `SESSION_IDENTIFY` (`docs/v2/plugin-abi.md` §6.2), instead of declining
  it — `--tournament` leaves plugin-abi.md:140's decline list once this task
  lands, and stays declined under it until then. The attached manager
  session is visible in `/api/v1/sessions` and, while it exists, is always
  the focused session regardless of OS foreground or recency (api-v1.md
  "Focus rule"'s `focus_priority` clause), matching 1.x's `select()`,
  which prefers the manager unconditionally (`src/instance.rs:702-721`,
  `src/runtime/worker.rs:873-896`) — a deliberate addition to the plain
  rule, not something the
  plain rule already produces. It builds `vosu.osu.tourney` from the
  matched spectators' sessions exactly as 1.x's stable branch does
  (`src/api/v2/tourney.rs`'s `build_tourney` manager path) and publishes
  the resulting aggregate to **every** currently attached stable session's
  `vosu.osu.tourney` — not only the manager's own — exactly as 1.x's
  `build_tourney` is invoked per client and looks the manager up across
  all instances each time (`src/api/v2/tourney.rs:36-43`). Landing this
  task removes the `stable-tournament` divergence below and the M2/M4a
  exit exemptions for it.
- The divergence file next to the fixtures (`tests/fixtures/upstream/DIVERGENCES.md`)
  lists every divergence with its reason and a per-entry class (G1):
  `security` (sandbox header, path confinement, peer and Origin policy,
  request limits — permanent), `contract` (the ADR 0004 legacy calculation
  endpoint's separate contract — permanent), `licensing` (`/api/ingame`'s
  removal, ADR 0021's 2026-11-25 amendment — permanent, since vosu cannot
  ship upstream's LGPL page and 2.0 has no replacement), and `temporary` (a
  pre-release gap with a task that closes it — must be gone by day-0). The
  one `temporary` entry at this freeze is `stable-tournament`'s `tourney`
  members (`v1`, `v2`, `v2-precise`), until the stable manager aggregate
  task above lands; the M8 day-0 gate (below) fails closed when a
  `temporary` entry remains, or when any entry's class is missing or
  unrecognized, never merely for the file being non-empty.

Exit: `cargo test --locked --test state_api_compat`, rewritten to drive the
adapters from bus input and compared byte for byte with the committed source
oracle, including the stable manager aggregate. The temporary tournament
exemption must be closed with T0 evidence before completed M4a; it is not
removed merely by this A4 contract freeze. Delete `strip_vosu_extensions`
and ad hoc regular `keys` normalization because encoders no longer emit them;
`cargo test --locked --test server_compat` updated in the same change for each
intentional removal, plus traversal attempts proving file routes stay inside
declared roots; `--test performance_invariants` ported to the adapters;
`--test daemon_smoke`; D17 M4a benches, including the end-to-end
snapshot → bus → `/json/v2` bytes and API v1 frames bench against the 1.x
serializers.

Risks: per-frame projection from the bus may cost more than 1.x encoding
from typed snapshots (the end-to-end bench decides).

#### A4 approved contract freeze (2026-11-24)

The owner-delegated approvals A4.1–A4.6 are normative:

1. Declare consumed common field namespaces and chart resource authority
   explicitly; no duplication into private state or loopback HTTP.
2. Publish missing accurate public facts, including independent physical
   key slots, with the owning schema version/partition/lanes and producers.
   Numeric inversions require oracle and boundary/property equivalence.
3. Distinguish pending tournament data from missing-score failure; only
   adapters implement upstream omission/500, never API v1.
4. Legacy calculation paths are relative to the current captured session's
   declared songs root or absolute spellings confined within its declared
   roots; all others are rejected as the named
   `legacy-calculate-declared-roots` security divergence. Keep the engine,
   query, mods and result contract unchanged.
5. Attribute removed requests only to validated installed overlay identity;
   unknown is separate. Retain at most 128 known overlays, deduplicated with
   24-hour inactivity TTL, plus fixed unknown feature buckets; no path/referrer
   logging. Notice data and exact lease/cache/worker bounds are frozen below.
6. Remove extensions endpoint by endpoint, including direct/filtered writers
   and nested tournament copies. Regular v2 has no `keys`; upstream precise
   slots and legitimate all-hit histories remain.

P0/R0 implement [permanent bundled surface contracts](adapter-surfaces.md).
P1/T0 and V0/G0/C0 implement the
[per-field fidelity and removal inventory](../../tests/fixtures/upstream/ADAPTER_CONTRACT.md).
Approved public additions ship in osu! schema 1.3.0 together with producers;
A4 alone does not claim that schema implementation or parity evidence.
State-schema differences do not exempt adapter wire output. M8 retires
exactly the four keys listed above, preserving API-v1 debounce/unknown keys.
M4b retains capture/golden tooling ownership; M8 day-0 execution is not
completed by M4a.

The approved D17 case names, common baseline harness and evidence requirements
are [frozen in the benchmark guide](../testing/benchmarks.md#m4a-d17-contract).
No measurement or D17 pass is claimed by the contract freeze.

#### M4a outcome

M4a is integrated on `m4a/projections` and merged with `v2/main` at `49e95cf`
(M1 L3, M2, the M3 exit and M6a SDK validation). Every 1.x state route is now
a projection of the state bus:

- **Registration.** `src/plugins.rs` lists the required bundled set
  explicitly: `vosu.osu`, `vosu.adapter.upstream`, `vosu.adapter.gosumemory`,
  `vosu.adapter.streamcompanion`. If any of the four fails to load or create,
  composition fails (`src/runtime/dev_plugins.rs`).
- **Reads.** The adapters read only through `vosu-surface`: one coherent,
  manifest-granted capture of the focused session per view. Private path
  echoes need an explicit `vosu.osu.internal` subtree grant and are data only.
  The production adapter crates depend on `vosu-surface`,
  `vosu-adapter-support`, `vosu-osu-model`, `vosu-plugin-api` and (upstream,
  streamCompanion) `vosu-osu-calc`. Their `vosu-core`/`vosu-osu`/
  `vosu-osu-state` edges are test ingress only (`scripts/crate-boundaries.txt`).
- **Serving.** One `BusStateProvider` per schema serves HTTP and WebSocket from
  shared latest-only encodings. HTTP reads create no demand; each WebSocket
  connection owns one RAII selector lease. Game files and legacy calculation
  use the session's declared roots (ADR 0005, A4.4); streamCompanion chart
  facts come from chart-resource bytes and `vosu-osu-calc`.
- **B1 is deleted in both directions.** The snapshot/event flow and the root
  control inputs are gone, as are `src/api/snapshot.rs`, the root adapter
  models and the interim `vosu-osu -> vosu-surface` edge. The crate checker
  rejects reviving that edge.
- **Removals.** ADR 0021 removals hold at every nesting level. The four
  retired configuration keys are ignored and never saved. Removed-route
  requests are attributed for the future D20 notice.
- **Tournaments.** D6's stable tournament-manager aggregate landed, and the
  `stable-tournament` temporary exemption is closed with distinct
  manager/spectator fixtures.

The [divergence ledger](../../tests/fixtures/upstream/DIVERGENCES.md) has 18
rows: 6 `security`, 12 `contract`, 0 `licensing` and 0 `temporary`. The four
`vosu.osu`-scoped contract rows authorize no adapter wire difference.

Exit evidence (2026-11-24, on the merged head):

- **Linux.** Formatting, workspace Clippy, Windows cross-Clippy, all nine
  `check-*.py` scripts and the 234 `scripts/test_*.py` tests passed. So did
  the tool-host smoke (curl and Chromium phases) and the dashboard smoke
  against a `test-harness` build. The full workspace nextest run
  (`--lib --bins --tests --examples --all-features`) passed 2,514 tests
  (4 skipped). Listed targets:
  - `cargo test --locked`: `state_api_compat` 7, `server_compat` 27,
    `state_bus_compat` 5, `config_compat` 9, `performance_invariants` 20
    (22 with all features), `runtime_wakeups` 3 (1 ignored).
  - `--features test-harness`: `state_bus` 6, `osu_plugin` 10,
    `api_v1_compat` 33, `daemon_smoke` 2.
  - `cargo test --workspace --lib --all-features --locked prop_`: 39.
- **Native Windows (`vm-test.py`, which always passes `--all-features`).**
  `server_compat` 27/27, `state_api_compat` 7/7 (including the stable
  tournament aggregate, the extension-absence proofs and the byte-for-byte
  oracle replay), `vosu-core` lib `state_subscribe` 17/17 and
  `api_v1_compat` 33/33 passed. A per-package sweep (`34691a0` plus the
  allocation fix; `vosu` rerun on `c1a8596`) passed every package except the
  symlink cases below: `vosu` 556 across all 28 test binaries
  (1 ignored; `daemon_smoke` is Linux-only and runs 0), `vosu-osu` 211,
  `vosu-osu-reader` 188, `vosu-plugin-api` 147, `vosu-osu-state` 104,
  `vosu-rosu-pp-api` 207, `vosu-rosu-map` 83, `vosu-pp` 72, `vosu-osu-calc` 38,
  `vosu-net` 28, `vosu-osu-model` 24, the three adapters 12/19/22 and
  `vosu-adapter-support` 9.
  - Four tests create file symlinks or reparse points, which need
    `SeCreateSymbolicLinkPrivilege`; `vm-test.py`'s LIMITED task lacks it
    (error 1314): `vosu-core`'s `surface_calculation_rejects_canonical_non_osu_alias`,
    `vosu-sys`'s two file-symlink cases and `vosu-adapter-upstream`'s
    `windows_reparse_files_and_directories_cannot_escape_declared_routes`.
    The `prepush.py` native phase runs them at High integrity.
  - Native runs exposed and the exit fixed four test defects. A
    tool-context expectation mixed path separators and expected the `\\?\`
    prefix of `std::fs::canonicalize`. `runtime_wakeups`' positive control
    expected over 10 cadence wakes in 100 ms, but the 1 ms interval fires at
    the ~15.6 ms Windows timer resolution (15 wakes in 250 ms natively, 116 on
    Linux); instrumented idle phases saw 0 wakes. The adapter cost
    invariants raced the osu! resource worker's first asynchronous
    references commit through the publisher mailbox (measured: 42
    allocations per steady commit, 54 when one absorbed it, 14 when the
    worker held the mailbox). `Publication` now records its applied epoch,
    and the bench fixture waits for that commit before measuring. The live
    adapter-mode example test expected one commit to show thread CPU, but
    Windows `GetThreadTimes` charges whole ~15.6 ms ticks, so it now commits
    until a tick is attributed (bounded). A 16,384-entry surface directory
    page opens every child through the retained root (R0 confinement): 73 ms
    on Linux, 2.3 s on native Windows alone, over the test's 5 s operation
    bound under the parallel native suite. The test now has a 60 s hang
    bound. The per-entry Windows open cost (~140 µs, so a 131,072-entry
    legacy listing takes ~18 s) is an open performance finding, not fixed here.
  - Stress (`vm-stress.py`): `performance_invariants` adapter cases 0/15
    failures and `runtime_wakeups` 0/15 after the fixes;
    `owner_teardown_and_destroy_all_retire_pending_and_identified_sessions`
    0/12 alone and 0/10 in parallel `plugin::` runs. Its single earlier 5 s
    timeout did not reproduce, so its bound is unchanged.
    `destroy_cancels_every_timer_of_an_instance_and_none_fire_again`, which
    predates M4a, failed once in an earlier 12-iteration `plugin::` run.
- **Hub freshness fix.** Stressing the X1 filter-isolation invariant on two
  CPUs (1–3% failures) exposed two latest-only defects in the compatibility
  hub's publication pass, both sending a client that became writable mid-pass
  an older image than the latest committed one (an extra encode and ack round
  trip each):
  - a pass that captured before the client's acknowledgement still sent that
    older capture. An acknowledgement now records the generation it observed
    before the client becomes writable; a pass whose capture is older
    recaptures instead of encoding or sending it (an integer compare per
    writable client, no lock or allocation, idle path unchanged);
  - shared unfiltered bytes were encoded only when a plain client was already
    writable at the start of the pass, so a plain client acknowledging during
    the pass received bytes kept from an earlier generation. They are now
    encoded lazily for the pass's own generation.

  Gated regression tests (`src/server/websocket/tests/stale_capture.rs`) fail
  on the old loop (`unfiltered:2` instead of `:3`, `unfiltered:1` instead of
  `:2`). After the fix the X1 test, unchanged and exact, passed 250
  consecutive two-CPU runs.

Not part of this exit:

- **D17 for M4a is pending.** It needs Main's official pinned Criterion run
  and native `live_reader_bench --adapter` runs on Windows stable/lazer and
  Linux lazer. It is not a pass. The M1 rows stay open under their owner
  exception ([benchmark guide](../testing/benchmarks.md#m4a-d17-status-at-the-exit)).
- Live parity is an optional diagnostic and did not run. M4b owns the
  pilot/full owner captures and `adapter_golden`; no capture exists, and
  none is claimed.
- M5 process execution and the M8 day-0 gate and migration remain separate
  work. The clean-room `/api/ingame` page this list also named at the M4a
  exit was withdrawn on 2026-11-25 (section 6).

**M8 migration inventory.** Delete exactly `ENABLE_KEY_OVERLAY`,
`ENABLE_MANIA_KEY_HIT_ERRORS`, `ENABLE_HIT_OFFSETS` and
`V2_SUBSCRIPTION_DEBOUNCE` from `vosu.toml` and the dotenv migration inputs,
idempotently. Preserve `API_V1_SUBSCRIPTION_DEBOUNCE` and every unrelated
unknown key or table. Write no replacement flag: subscription selectors own
collection.

**First-party consumers of removed surfaces.** These still request routes that
M4a removed, so those requests now fail. Their owners move them to API v1
namespace subscriptions and skin trees/resources; the routes do not return:

| Consumer | Removed surface it uses | Owner |
|---|---|---|
| `assets/server/tool-bridge-v1.js` `subscribe()` | `/websocket/v2/subscribe` (now 404) | M7 tool-host replacement; v2 bridge already uses API v1 |
| vosu-tools `packages/client-skin` | `GET /files/skin/` index, lazer `/files/skin/{path}`, selected-v2 pointers | vosu-tools, before M8 |
| vosu-tools `packages/tool-web` bridge typings and the tools calling `vosu.subscribe` (downloader, map-analysis, map-preview, practice, replay-renderer, server-pp, skin-studio) | `/websocket/v2/subscribe` through the v1 bridge | vosu-tools, before M8 |
| vosu-overlays `ratings-timeline` | `/websocket/v2/subscribe` | vosu-overlays, before M8 |
| vosu-overlay-quickstart (`app.js`, README, TUTORIAL) | `/websocket/v2/subscribe` | quickstart owner, before M8 |

M8's D20 notice flags user overlays that still request removed routes; they
stay installed and enabled.

### M4b: golden fixtures from live captures (owner-authorized)

Scope: extend `tools/live-parity` with (1) a route-probe matrix recording
status, headers, and error bodies, including near-miss routes; (2) a
vosu-side bus dump per sample through a `test-harness`-only endpoint so
captures can be replayed; (3) a `golden` command that normalizes volatile
values and writes fixtures under `tests/fixtures/upstream/<pinned revision>/`.
A new `tests/adapter_golden.rs` replays the bus dumps into the adapters and
compares every route and frame with the upstream capture, minus the
machine-readable divergence rules.

Captures (D14), against the pinned upstream revision (`Cargo.toml`
`source-commit`), traversing menu → play → paused → result:

1. **Pilot**, as soon as the tooling lands: one pair (Windows lazer, the pair
   that exercises the most adapter surface), in an owner-authorized session.
   Mismatches become M4a follow-up fixes immediately.
2. **Full set** before M8: every pair in `SUPPORTED_MATRIX`
   (`tools/live-parity/live_parity.py`: Windows stable, Windows lazer, Linux
   lazer), in one owner-authorized session.
3. **Recapture** of all three if the pin moves before M8.

Exit: `cargo test --locked --features test-harness --test adapter_golden`
(A4b.9) with nonzero case counts by platform and family, on the pilot, then on
the full set; the fixture inventory names its revision and is covered by exact
per-file `scripts/brand-allowlist.txt` entries in the `parity`/`fixture`
categories where fixtures spell the upstream name.

Blocking: M8's release gate blocks on the full set. Nothing else does.

#### A4b approved golden contract freeze (2026-11-24)

The owner decided A4b.1 and A4b.4; main adopted every other A4b item exactly
as recommended under delegated spec authority. The normative text is the
[behavioral golden contract](../../tests/fixtures/upstream/GOLDEN_CONTRACT.md);
in summary:

1. **Capture authority (A4b.1).** Captures are owner-authorized per session
   and agent-operated on isolated VM test installs. A written authorization
   names the vosu commit and pin, platform/client version/install/profile,
   scenario, account/data/media/network policy, private evidence path, GUI
   operator scope and fixture review route. VM lifecycle pre-authorization is
   not capture authorization. The agent stops on any login, purchase, legal,
   private-account or unexpected data prompt. The Windows and Linux VMs never
   run together.
2. **Capture build (A4b.2).** An attested debug
   `cargo build --locked --features test-harness` vosu (Windows without
   default features; Linux keeps `linux-process-events`). No release-capable
   harness; release live parity stays an optional diagnostic, and debug
   cadence is not performance evidence.
3. **Endpoint (A4b.3).** Exactly
   `GET /__vosu_test/v1/adapter-capture?schema=<v1|sc|v2|v2-precise>`:
   harness-only, mounted only with a per-launch capability, loopback-only,
   ordinary 404 for absent or invalid authorization, exact query, GET only,
   `no-store`, 8 MiB bound, no demand or history. It returns the
   provider-retained capture and the adapter bytes from that same snapshot.
4. **Data and licensing (A4b.4).** Offline or test accounts, no chat.
   Committed fixtures use generated vosu-owned test maps, skins and media
   with a recorded license; never the owner's library. Raw captures stay
   private.
5. **Sandwiches (A4b.5).** Stable case IDs; upstream request twice, vosu
   dump → route → dump; whole-case retries with every attempt recorded; no
   timestamp joins. Exact payloads only in stable windows; active play is
   diagnostic.
6. **Normalization (A4b.6).** A closed list of exact-scope rules (`date`,
   file `last-modified`/`etag` and their request validators, capability
   redaction in raw evidence, dump identities, synthetic roots, and
   individually listed client timestamps, none initially). Never status,
   route, method, content-type, security, cache, range or upgrade semantics,
   numbers, null versus absence, order, quirks or coherent-sample
   relationships. A new rule needs review, a counterexample test and a
   manifest version bump.
7. **Headers (A4b.7).** Status, content type/length, cache, validators,
   range, location, CSP, nosniff, `access-control-*` and WebSocket
   upgrade/accept/protocol fields are compared; others are recorded
   privately.
8. **Publication (A4b.8).** `golden` writes only a candidate directory; main
   reviews the diff, brand inventory, privacy and asset checks and replay
   before commit. Nothing is uploaded or published.
9. **Exit command (A4b.9).** As above, with
   `[[test]] required-features = ["test-harness"]`; a featureless or zero-case
   run is not evidence.
10. **Pin policy (A4b.10).** The test reads only the Cargo pin's directory;
    a pin bump is one owner-approved change that also deletes the previous
    behavioral directory, and before M8 it triggers a full recapture.
11. **Divergence rules (A4b.11).** `divergence-rules.json` rules reference
    existing non-`temporary` ledger IDs with exact case and record-part
    scope; unused rules fail; whole-route subtraction needs whole-route
    ledger scope.
12. **`/api/ingame` (A4b.12).** Not captured, not a golden. The clean-room
    observation this item kept separate was withdrawn with the endpoint on
    2026-11-25 (section 6).
13. **Size (A4b.13).** Per platform at most 8 MiB of JSON/CBOR envelopes and
    4 MiB of approved binary assets; text stays UTF-8, never compressed.
14. **Brand (A4b.14).** Exact per-file allowlist entries (`parity` for
    upstream wire/provenance, `fixture` for synthetic replay content), added
    only in the reviewed fixture commit; no revision-directory wildcard.

The freeze is not evidence that any tool, capture or fixture exists.

### M5: plugin-host process, capability drop, brokered reads

Starts after M4a (no bridge B1 exists) and S1.

Scope:

- `vosu plugin-host` argv dispatch in `src/main.rs` beside `vosux`, before
  the Tokio runtime is built; one process per plugin instance; the
  shared-memory rings, framing, and 2-second backpressure window (§7.2);
  crash detection and 1/2/4/8/16-second restart backoff with the
  five-in-five-minutes `failed` state; read plans relayed byte for byte; a
  dedicated ring-reader thread that hands `VOSU_READ_PLAN_SYNC` results to
  the blocked submitter, which dispatches them on its own thread (D5);
  bounded streams for generated resources.
- Linux: the §7.3 sequence before `dlopen` on the single initial thread, as
  amended by A5.1(b) (below). `vosu --setup` grants file capabilities
  (`crates/vosu-sys/src/platform/linux/capabilities.rs`). Core's launcher
  sets `no_new_privs` before it execs `vosu plugin-host`, so the exec grants
  none of them; the child still runs and verifies the full drop
  (plugin-abi.md §7.3 "Launch"). Process plugins need Linux 5.4 or later.
  Setup does not change.
- Windows: `CREATE_SUSPENDED`, job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` before resume (§7.4), reusing
  `src/tools/process.rs:59-70`. Then the measurement brokered-reads.md "Open
  questions" requires: port `benches/brokered_reads_bench.rs` to a Windows
  helper (unnamed section + event versus named pipe), run it pinned on the
  native Windows guest, record results in [brokered reads](brokered-reads.md)
  and the [benchmark guide](../testing/benchmarks.md), and apply A5.6's frozen
  tie rule and Windows brokered-read bar (D17).
- `--plugin <dir>` becomes available in prerelease release builds, loading
  the plugin with `user` trust: `runtime = "process"` after the approval
  prompt (catalog-v2.md "Trust"), so ecosystem testers exercise the process
  runtime and approval from here on.
- The osu! plugin's `test-harness`-only `cdylib` build with a non-bundled
  test manifest (bundled manifests must not request `process`,
  plugin-abi.md:36) runs the real bus path out of process (gap 13).

Exit: new `tests/plugin_host_native.rs` run with `-- --nocapture` on Linux and
Windows: the testkit and the osu! plugin `cdylib` run under
`runtime = "process"` and produce the same `/json/v2` bytes and API v1 frames
as in-process for the oracle inputs; read plans against another session's
handle return `VOSU_ERR_PERMISSION_DENIED`; `VOSU_READ_PLAN_SYNC` works and
times out identically in both runtimes; killing the plugin host detaches its
sessions with `reason: "lost"` and restarts with backoff; a stalled ring
trips the 2-second window while an idle plugin survives beyond it. On Linux an
`#[ignore]` test, run natively after `vosu --setup` on a packaged build,
asserts that the plugin host's `/proc/<pid>/status` shows `CapInh`, `CapPrm`,
`CapEff` and `CapAmb` all zero, `NoNewPrivs: 1`, `Seccomp: 2`, a failing
user-namespace probe and no UID 0, both before the constructor and after
plugin threads exist. `CapBnd` is recorded but not asserted (A5.1). On
Windows, terminating core terminates the plugin host and its descendants.
D17 M5 benches: `brokered_reads_bench` with the
production transport, and the osu! plugin's reader benches and per-tick CPU
under the process runtime (D5) recorded as data, not gated, since the
bundled plugin runs in-process; `cargo test --locked --test process_memory -- --nocapture`
on both OSes.

Risks: shared-memory setup and teardown robustness against a crashed host
(brokered-reads.md "Recommendation" keeps a socket fallback acceptable);
measured stage counts of the real regular capture path versus the bench
estimate (brokered-reads.md "Open questions").

#### A5 approved contract freeze (2026-11-24)

The owner decided A5.1 as option (b). Main adopted A5.2–A5.8 as recommended,
under the owner's delegated spec authority. These decisions are normative, and
no implementation or measurement is claimed by them. The full text is in:

- [plugin ABI](plugin-abi.md) §5, §6.8, §7.1–§7.4 and §7.6;
- [catalog v2](catalog-v2.md#process-plugin-approval-and-management-a55-approved-2026-11-24);
- [API v1](api-v1.md) jobs;
- [ADR 0019](../decisions/0019-plugin-runtime-and-trust.md)'s 2026-11-24
  amendment;
- the [test and fixture conventions](../testing/verification.md#m5-test-and-fixture-conventions-a5);
- the [D17 contract](../testing/benchmarks.md#m5-d17-contract).

| ID | Decision |
| --- | --- |
| A5.1 | Before any plugin code runs, a Linux plugin host reaches `CapPrm = CapEff = CapInh = CapAmb = 0` and `NoNewPrivs = 1`, with seccomp denying creation or entry of a user namespace. It reads this back and fails closed. `CapBnd = 0` is not required. Setup and the daemon gain no privilege, and `CAP_SETPCAP` is not granted. Without setup, process plugins run under the same postcondition. An environment that cannot install the filter fails with `security_unavailable` and is not retried. |
| A5.2 | Transport-private frames are those whose opcode has the high bit set; ABI v1 gains no opcode. Every send whose status only core can decide is acknowledged. Plugins see the in-process statuses: there is no fabricated success and no phantom later result. Each instance has 64 shared status and `SYNC` waiters, and overflow returns `VOSU_ERR_INVALID_ARGUMENT`. Backstops are 2 s, or the `SYNC` budget plus 2 s, and reaching one is a transport loss. Rings are 2 MiB per direction. |
| A5.3 | Every unexpected exit (including code 0) and every pressure or transport loss counts once. The fifth event in a rolling 300 s window latches `failed`. Backoff is 1/2/4/8/16 s, saturating, and resets only when the window is empty. Controlled stop, shutdown, revocation and deterministic startup refusals are never crashes. A manual restart resets the latch and starts a new generation. |
| A5.4 | `DELETE` stays advisory. The 30 s deadline makes a job `timed_out`, then core cancels and allows 5 s of grace per invocation. Core kills at the earliest unanswered grace, and that kill counts as one loss. Collateral jobs fail `plugin_crashed`, and terminal or timed-out jobs are unchanged. Destroy has a 5 s grace. There is no manifest timeout. |
| A5.5 | Approval binds the id, the runtime, the manifest and library SHA-256 and the provenance. It is stored in `plugins/approvals.json`, and dashboard routes handle it under `/api/plugins` with CSRF. Debug builds keep only the in-process `--plugin` exception. Prerelease release builds load `--plugin` as `user`/`process` after approval. There is no noninteractive or CLI approval. Stable release builds still refuse `--plugin`. |
| A5.6 | Unnamed section and events remain the Windows production transport. The pipe is measurement-only unless it wins under the frozen tie rule and a separate secure-handoff amendment is approved. Linux `production_sync` p50 must be at most 2× the same-run prototype `shm` p50. The Windows bar uses the same form against the selected transport. |
| A5.7 | D16 stands with the Medium-host requirement. S1 is recorded (above). Session and focus use the child's API v1 (AF.8). The production vosulay launch belongs to the ecosystem owner. |
| A5.8 | The child re-arms `PR_SET_PDEATHSIG` after exec, checks its parent PID, and is spawned only from a long-lived launcher thread. Startup and destroy deadlines are 5 s. Failed security, bootstrap or configuration is a load refusal, not a crash loop. |

**Execution backend and load metadata (H0).** Core keeps exactly one
authoritative validation path: the M6a SDK `PackageManifest` plus validation
context. From the validated package and its trust, core decides the granted
runtime once (plugin-abi.md §7.1).

- An instance's backend is either `InProcess`, a bundled vtable or a debug
  developer library, or `Process(ProcessLaunch)`.
- `ProcessLaunch` holds:
  - the plugin id;
  - the next generation (a `u64` that is never reused while the daemon runs);
  - the canonical absolute library path for the running host target;
  - `library_sha256` and `manifest_sha256`;
  - provenance: `bundled`, `development {path}` or
    `installed {source, version, package_sha256}`;
  - the trust tier and a reference to the approval record;
  - `concurrent_dispatch`;
  - effective settings, `data_dir` and the `SYNC` budget.
- A process grant never resolves, loads or calls the library in core.
- A bundled manifest that requests `process` stays invalid.
- State, actions, resources, sessions, focus, timers and logging remain the
  existing host services behind one dispatch, destroy and loss seam per
  backend. There is no duplicate host implementation.

**Waiter and queue budgets (per process instance).** These bounds are frozen:

| Item | Bound |
| --- | --- |
| Ring storage | 2 MiB in each direction. A legal frame parks for at most 2 s, and an illegal oversize frame is refused at once. |
| Status and `SYNC` waiters | 64, shared. A `SYNC` result buffer holds at most 64 KiB, so the total is at most 4 MiB, allocated lazily and reused. |
| Asynchronous plans, scans, plugin `ACTION_CALL`s and timers | 64 each, as before. Local admission happens before write, and core stays authoritative. |
| `STATE_UPDATE` | One in-flight split sequence plus one complete merged update per session. |
| Latest-only notifications | One entry per key: the handle for demand and focus, the timer id, one entry for settings, and the invocation for progress. |
| Lifecycle backlog | 4,096 entries, unchanged. |
| Restart state | Five timestamps and one cancellable one-shot timer. |

Admitted work and terminal results use their existing reserved outboxes. The
generic dispatch queue never refuses them, and private replies never enter it.

**Counterexamples ruled out by this text:**

- core loading a process library to validate it (§7.6 startup);
- the unchanged `--setup` being assumed able to empty the bounding set
  (§7.3: not required, not attempted);
- a silent ring treated as an idle crash (§7.2 pressure and loss);
- a remote permission failure turned into a fabricated success (§7.6
  acknowledged sends);
- `EPERM` ignored while a constructor still runs (§7.3 fail closed);
- a `PR_SET_PDEATHSIG` that only the pre-exec process armed (§7.3 step 1);
- generic plugin-host confinement applied to vosulay (§7.3 scope, D16);
- generic job membership offered as proof (§7.4);
- a changed library reusing an approval (A5.5 record, §7.6 digest check);
- a 16 s retry after the fifth crash (A5.3);
- `timed_out` overwritten by `plugin_crashed` (A5.4).

**M4a and A4 compatibility.** The bundled osu! plugin and adapters stay
in-process, and no external or process plugin gets `vosu-surface` authority
(§7.5). The process osu! fixture follows the real producer → ABI → bus →
M4a adapter path and compares actual `/json/v2` bytes and encoded API-v1
frames against the committed oracle inputs, without fixture regeneration or a
DTO publisher. A4.1–A4.6, the adapter surface contracts and M4a's D17 cases
are unchanged. The process runtime adds no B1 bridge, snapshot channel or
`vosu-osu → vosu-surface` edge.

**M6a and Q compatibility.** Core validates through the SDK only:

- Q3 provenance is part of the registration context;
- Q4's strict and duplicate policy is unchanged;
- Q6 hint validation and Q7 game classification run before any launch.

The plugin host never parses `manifest.toml` and adds no second validator;
core sends it only already-validated fields.

**M6b and M7 interfaces.** None of these decisions reopens an M6b or F
decision. M6b depends on two M5 deliverables:

- the per-plugin controlled teardown and unregistration operation, for
  uninstall, untrust and revocation (A6b.7);
- the shared process-plugin load path, which M6b X0 needs to load, restart
  and approve real installed process plugins.

M7 consumes the `/api/plugins` routes and does not rebuild them. Both
deliverables are in [catalog v2](catalog-v2.md#process-plugin-approval-and-management-a55-approved-2026-11-24).

**Unit file ownership.** The integrator owns:

- workspace and crate `Cargo.toml` files and the lockfile;
- public and core exports;
- `src/main.rs` and runtime composition;
- test-root declarations, `scripts/crate-boundaries.txt` and the module
  boundary lists.

`host/instances.rs` changes in the order H0 → B0 → C0, through the integrator.
No unit makes a length exception. Every file stays at or below 1,000 lines.

| Unit | Owns |
| --- | --- |
| S0, A5 | `docs/testing/{verification,benchmarks}.md` contract sections; `docs/v2/{implementation-plan,plugin-abi,catalog-v2,api-v1}.md`; ADR 0019 |
| H0 | `crates/vosu-core/src/plugin/{registry,instance,error,host}.rs`, new `plugin/registry/` and `plugin/execution/` modules, and the shared pure descriptor validator |
| T0 | `crates/vosu-core/src/plugin/process/{frame,ring,channel}.rs`; `crates/vosu-sys/src/plugin_process/` mapping primitives |
| L0 | `crates/vosu-sys/src/plugin_process/linux/{spawn,capabilities,channel}.rs`; Linux spawn integration in core. Setup capabilities stay unchanged. |
| W0 | `crates/vosu-sys/src/plugin_process/windows/{spawn,job,channel}.rs`; Windows endpoint integration; a shared tool-process helper only with owner agreement |
| R0, R1 | Pure memory vocabulary in `vosu-plugin-api` (R0); migration of the reader, state and `vosu-osu` callers, and deletion of their `vosu-sys` edges (R1) |
| E0 | `crates/vosu-core/src/plugin/process/{entry,bootstrap,control}.rs`; the child loader |
| T1 | `crates/vosu-core/src/plugin/process/{relay,waiters,delivery,admission}.rs`, reusing `plugin/{outbox,dispatch}` |
| B0 | `crates/vosu-core/src/plugin/host/{read_plans,scans}.rs` process routing |
| C0 | `crates/vosu-core/src/plugin/process/{supervisor,restart,shutdown}.rs`; focused hooks in `host/{instances,lifecycle,actions,action_delivery,resources,resource_requests}.rs` |
| U0 | `src/runtime/dev_plugins.rs`, CLI parsing and prerelease policy, the approval store and `/api/plugins` handlers |
| P0 | `crates/vosu-plugin-testkit/examples/testkit_process_plugin.rs`, `crates/vosu-osu/examples/osu_process_plugin.rs`, `tests/fixtures/plugin_host/`, `tests/plugin_host_native/support/` |
| I0, X0, N0 | Their `tests/plugin_host_native/` child modules, listed in verification |
| P1, X1 | `benches/brokered_reads_bench.rs` and its support, `examples/brokered_reads_target.rs`, the process cases and modes in the reader benches, and `docs/v2/brokered-reads.md` |
| X2 | Final evidence, plus updates to security, setup and architecture documents |

The [D17 contract](../testing/benchmarks.md#m5-d17-contract) records the
D17 names, metrics and bars.

#### M5 landing decisions and open obligations (2026-11-25)

Main accepted these E0 and P1 results on 2026-11-25:

- **FAILED before CREATE.** A child that fails §7.3 before it has read
  `CREATE` reports `FAILED` with generation 0, which no real generation uses
  (plugin-abi.md §7.6).
- **Build identity.** It is `vosu/<package version>/plugin-host-1`. On
  Linux, core execs `/proc/self/exe`, so the child is always the running
  image. On Windows, a replacement image with the *same* version is not
  detected. This residual is accepted because the updater always installs a
  different version, so an in-place update is always caught as
  `executable_updated`.
- **Private exit statuses 74–76.** These join the frozen 70–73
  (plugin-abi.md §7.6).
- **P1.** The Windows transport stays the unnamed section and events, the
  pipe stays measurement-only, and the Windows D17 bar and P1's method
  deviations are adopted ([brokered reads](brokered-reads.md#decision-and-windows-bar-approved-2026-11-25)).

E0 landed the child lifecycle and a test-installed launcher. These pieces
are open obligations with named owners, not completed work:

| Owner | Open obligation |
| --- | --- |
| T1, B0 | The child refuses a `VOSU_READ_PLAN_SYNC` submission with `VOSU_ERR_UNSUPPORTED` until the `REPLY_NEXT` path exists. The 64-per-instance local admission caps are not yet applied before the write; core still enforces them. Child dispatch is one serial worker even under `concurrent_dispatch`. The child's delivery queue is a bounded FIFO that blocks the ring reader when full, so a full queue ends as a 2-second stall loss rather than T1's coalescing queue. Core's ring reader delivers each plugin → host message to the handler on its own thread. |
| U0 | The launcher computes `library_sha256` from the library at launch; U0 passes the approved digest instead. Production composition installs `ProcessHostLauncher` only after the approval flow (A5.5). |
| C0 | `HostError::ProcessStartupFailed` carries the formatted `stage … (code)` text. C0 consumes the structured startup failure (`StartupFailure`, `StartupStage::is_crash_class`) to decide restarts and the `failed` latch. |

**Dev-profile measure (P0, 2026-11-25).** The workspace builds `sha2` at
`opt-level = 3` in the dev profile (`[profile.dev.package.sha2]`). Core and
the plugin host each hash a process library before loading it, the child
inside the 5-second startup deadline. Unoptimized, hashing the 241 MB debug
osu! fixture library took 4.9 s; optimized, it took 0.11 s. Release builds
were already optimized, so production behavior is unchanged.

### M6a: `manifest.toml` and `catalog-v2.json` validation

Scope: the unified manifest (catalog-v2.md "Package manifest v2", "Shared
fields", the three kinds, "Capability fields", "Capability coverage",
"Namespace and kind parsing", "Game capabilities"); `metadata.txt` as
read-only legacy overlay input; `catalog-v2.json` parsing and validation;
the bounded `[[provides]] schema` profile and the catalog's mirrored schema
version and lane table (D7); the `comm_hints` matcher field (D18);
committed JSON Schemas for both documents under `docs/v2/schemas/` so
ecosystem packaging scripts can validate without vosu. Validation lives in
`vosu-plugin-api` so plugin authors and core use one implementation. The
integrated implementation extends M2 K0's capability grammar and F2's action
declarations, extracts K1's pure D7 compiler below core, and replaces T0's
independent v2 document/requirement rules. Core retains schema registration,
grants and demand; the tool host retains exact live launch-session authority.

M6a's Q1–Q7 contract completion was approved on 2026-11-24:
the [shared validation contract](catalog-v2.md#m6a-validation-contract-approved-2026-11-24)
records strict collection/shape policy, resolution `1..=16384`, game/matcher
equivalence and authoritative field revisions. [Sampling lanes](state-schema.md#sampling-lanes)
freezes wildcard/ref-sibling/full-document/provenance/work-bound semantics;
[process matching](plugin-abi.md#61-process-matching) freezes portable hint
validation.
M6b consumes the SDK's pure requirement/candidate-conjunction predicates;
it must not add a parallel capability/version parser.

Exit: `cargo test --locked -p vosu-plugin-api manifest` and a corpus test
`tests/catalog_v2_compat.rs` with valid and invalid manifests for every rule
in catalog-v2.md, including reserved names, id prefix rules,
`provides`/`[[actions]]` pairing, and D7's size, depth, `$ref`, and
namespace limits.

#### M6a outcome (2026-11-24)

V0–V6 are integrated at `7ea4958`. The exit corpus contains **266 paired
rejection controls** and **38 literal semver truth rows**. The SDK `manifest`
filter passed 62 tests; `catalog_v2_compat` contains 22 tests, or 23 with
`test-harness`/all features. The integrated Linux workspace run passed
2,556 tests with 6 skipped. Exact commands, attribution, the migration ledger
and remaining evidence are in [verification](../testing/verification.md#m6a-outcome-and-evidence).

All plugin, v2 tool and v2 overlay production loaders consume SDK validation
with explicit running-version and registration context. The former K0 subset
deferral and T0 minimal v2 parser are no longer implementation boundaries.
Offline Draft 2020-12 schemas cover document structure, not source trust,
semver resolution or asset-backed equality; use the SDK with schema bytes for
complete mirror comparison.

**Not a release claim:** native Windows passed `manifest` (59) and
`catalog_v2_compat` (23) in the agent VM (see verification). No new
compiler/load-time performance measurements were taken, and the
functional results do not establish a performance pass or supersede inherited
M1/M2/M3 findings. M6b still owns fetching, credentials, trust, source/provider
selection, graph orchestration and installation; it must reuse the SDK
predicates and verify package mirrors before install commit. M7 still owns
final legacy tool-host removal and first-start migration. `metadata.txt`
remains supported read-only overlay input, and `tool.toml` remains only the
explicit M3→M7 exception. No release or push is authorized here.

### M6b: sources, credentials, resolver, private sources

Scope: catalog-v2.md "Sources and credentials" ("Source forms", "Forge
detection", "Per-forge private asset retrieval", "Credentials", "Origin
scoping on redirects", "Private networks", "Trust") on top of the manual
redirect loop in `src/net/streaming.rs`; "Resolver algorithm" (built-in
providers, provider selection, version solving, install plan with
`needs_approval`, in-process plan errors, and the D8 `internal.paths`
disclosure field, dependency-only autoremove, game detection); "API
placement" (dashboard listener, CSRF); plugin trust tiers complete §7.1 and
the approval flow. `AGENTS.md`'s private-destination rule is amended with
ADR 0023's per-host exception in the same change.

Exit: `tests/catalog_v2_compat.rs` resolver scenarios;
`cargo test --locked -p vosu-test-harness --test network_integration`
extended with credential stripping on every cross-origin redirect, the
GitHub API asset redirect to presigned storage, and a local Forgejo or Gitea
container behind an internal certificate authority installed into the test
trust store, proving the private-network exception stays limited to the
exact source host (ADR 0023:52-60, D15);
`cargo test --workspace --lib --all-features --locked prop_` (ZIP preflight
unchanged); `python tests/tool_catalog_smoke.py` and
`python tests/catalog_art_smoke.py` on catalog v2; credentials never
returned by any route (server_compat-style test).
**Owner-dependent (D15):** a one-time check against GitHub, GitLab (nested
group), Codeberg, and one Gitea or Forgejo instance with read-only tokens
that are never stored in the repository; results recorded in
[verification](../testing/verification.md).

#### M6b frozen service interfaces

**A6b contract landing, approved 2026-11-24.** A6b.1–A6b.6 and A6b.8
are adopted as recommended; A6b.7 adopts pending user restart, **not**
automatic restart. Normative details are in
[Sources and credentials](catalog-v2.md#sources-and-credentials),
[Resolver algorithm](catalog-v2.md#resolver-algorithm) and
[ADR 0023](../decisions/0023-authenticated-catalog-sources.md).
This freezes implementation inputs/outputs, not a claim those services or
their native evidence already exist at baseline `2c349eb`.

The following names describe the shared logical records/operations; Rust
ownership wrappers may borrow/share immutable data without changing their
fields or authority. Integrator owns public exports/config/root wiring.
W1 implementations must use these same records, not competing representations.

| Owner / destination | Frozen input → output / invariant |
| --- | --- |
| S0 `vosu-core::catalog::sources` | `SourceIdentity` is the untouched configured string; equality, credential keys and officialness are exact-string. `ParsedSource` retains identity plus `Static { url }`, `GitHub { owner, repo, tag }` or `Repository { origin, project_path }`; parsing never grants trust. `Forge` is GitHub/GitLab/Gitea-compatible/Static. `DetectedSource` adds forge, configured host and derived credential origin. Successful/failure cache state is keyed by identity and source generation, not URL aliases. |
| N0 `vosu-net` request policy | `Origin` is parsed HTTPS scheme/host/effective port. `RequestPurpose` distinguishes Probe, SourceMetadata, CatalogAsset, Package, Artwork, Updater and Counter. Source policy contains exact configured host, private permission and credential origin; it cannot authorize package/art/updater/counter. `RequestCredential` is sensitive Bearer/token/Basic material, not a public HeaderMap. Only metadata/asset requests may carry it, only on approved initial origin. Policy is immutable for one chain; cross-origin stripping is irreversible. |
| N0 bounded request operation | URL + purpose/policy + optional scoped credential + body/deadline/redirect limits + cancellation → bounded bytes/status or redacted typed error. Probe is always credential-free with 5-second total deadline. Validate initial/hop URL and DNS, pin addresses, no proxy/auto-redirect/cookie ambient state. Existing public streaming/download methods stay credential-free/public-only. Package streaming accepts pinned exact size/hash/cap and never accepts source policy. Production TLS uses OS roots without losing public roots. |
| C0 `vosu-core::catalog::credentials` | Inject channel-aware config root, do not rediscover release roots. `CredentialInput` is Token or Basic; secret values have no general Debug/Serialize. `set(identity, input)`, `delete(identity)` and explicit validated repair serialize durable publication, returning generation or redacted error. A private core-only credential lookup returns an immutable secret lease plus generation for a fetch; no secret getter is exported to dashboard/surface/ABI. `presence(identity)` returns boolean plus availability status only. |
| S0 source publication | `SourceRecord` contains identity, enabled, trusted, private_network and order; officialness is derived from fixed identity allowlist, never loaded from list JSON. `replace` accepts ordered identity/enabled pairs only (32 maximum); `set_trust`/`set_private_network` are separate commands. Source removal and credential purge share one operation/recovery authority. C0 never publishes a newly created unknown source. |
| S0 generation gate | Snapshot source/order/trust/private/credential generations before I/O; publish only if all still match and source is enabled/present. Disable/remove/reorder/flag/credential changes invalidate dependent fetches/plans. Hold no state mutex across network/await/filesystem/plugin callback. Explicit refresh retries failed detection; no detector timer or stale-last-good candidate fallback. |
| S0 → F0 | Immutable `DetectedSource`, generation stamp, private policy and optional private credential lease → bounded fetched catalog bytes or typed source error. F0 derives exact forge endpoints/asset URL; N0 alone implements connection/redirect/TLS rules. Probe identity cannot be established across origins. F0 does not write source state. |
| S0 → R0/P0/G0 | `SourceSnapshot` holds source generation/status and ordered validated candidates. Each candidate retains exact source, source ordinal, kind, catalog-array index, immutable SDK entry, host/target support and exact package/hash/size. Retain lower-ranked alternatives. Failed/invalid/disabled sources have status but no candidates. No secret or authenticated header is stored in snapshot, detection cache, plan or public view. |

**One M6a seam, already present:** reuse SDK `catalog::{Catalog,
CatalogEntry, EntrySupport}`, `Catalog::parse`, `validate_context`, `support`,
and `manifest::{PackageManifest, ValidationContext, Registration,
SchemaProvenance, Requirement, candidate_satisfies}`. Core supplies running
version, all enabled successful-source registrations, installed and actual
bundled registrations, and exact contextual provenance; SDK does not infer
officialness from ids/URLs. Construct the whole registration context before
contextual ingestion; do not first-wins merge candidates into a lossy map.
Host/target/size support is distinct from document validity.

I0 loads the **actual confined package schema bytes**, compiles through the
SDK schema compiler and calls `catalog::compare_package(kind, entry,
manifest, compiled_schemas)` before publication. Catalog-reconstructed lane
metadata is not asset verification. Preserve Q1–Q7: wildcard/ref/full-document
work bounds, trusted provenance, strict duplicates/fields, resolution
`1..=16384`, portable matcher hints and game/provide equivalence.
M4a's actual compiled manifests/current core schemas supply built-ins,
including approved common/chart/internal grants and accurate schema additions;
no copied M3 inventory, duplicate namespace walker or B1 restoration.

**Graph/plan/install seam:** `ResolutionInput` is immutable validated
sources + installed consumers/manifests + actual built-ins + app/target/cap +
explicit requested/update intent. `Resolution` returns one selected candidate
per plugin, resolved required/satisfied-optional edges (requirer, capability,
range, selected provider), dependency-first adds or typed errors with empty
adds. R0 owns centralized finite candidate/search-work bounds; no network
or mutation. U0 reconstructs this graph, not a persisted reference counter.

`PinnedPlan` adds an opaque random ID, creation/deadline, full public add
fields, exact entries/hash/size/runtime approvals and all generation/fingerprint
prerequisites. P0 owns 64 pending/10-minute TTL/one-running-operation admission
and single-use confirmation; no expiry poller. I0 receives only a confirmed
pinned plan and cancellation, not an arbitrary item ID to resolve again.
Use the same checked `MAX_PACKAGE_MIB` byte cap in R0/P0/I0/N0.

`InstalledRecord` contains kind/id/version, exact source and artifact hash,
runtime/approval binding, requested/dependency origin and current manifest
identity; no credential, frozen trust label as authority or refcount.
`InstallTransaction` owns retained prepared objects, same-filesystem staging,
before/after record/directory identities and a durable versioned recovery
journal. Preparation does not expose packages. Journal each publication
boundary; rollback an unpublished batch; recover before registry discovery.
Only a durable graph commit publishes installed generation, then loaders
recheck current trust/approval. U0 reuses that transaction for recursive
removal and preserves data/settings. M8 may reuse recovery/import operations
and no-overwrite replacement intents; it owns the one-time migration marker,
not source scanning or credential reads.

**M5 dependency, explicitly not implemented by M6b:** T0 hands validated
package location/manifest, target library, exact source/hash/runtime approval
binding and current-policy generation to the registry execution-backend seam.
Process grant must produce a real M5 endpoint without parent `dlopen` or
in-process fallback. Missing runtime/security prerequisite is an honest load
refusal; it cannot count as native user-plugin exit. Stop/revoke invalidates
instance generations and uses M5 cleanup; running in-process revocation
remains pending user restart. A5.1's owner-pending Linux privilege policy and
M5/S1 native gates remain external blockers, not permission to weaken §7.3.

W2's pure `catalog::trust::CurrentPolicy` consumes a fresh S0 generation and
records at plan/confirm/load. `catalog::approvals::{Artifact,Approval}` bind
id, exact source, version, hash and runtime; neither a saved tier nor a matching
hash grants trust. `InstalledPolicy::source_changed` clears prior approvals on
trusted-to-user transitions (including source removal), returning `StopProcess`
or `PendingRestart` for running code. W4 persists the cleared approval through
I0 before another load/restart, applies M5 stop effects and retains the
in-process disabled marker for the daemon lifetime, even after re-trust/rescan.
`authorize_load` is a current-policy check, not a durable load permit; the
shared operation gate must cover its final check and execution admission.
H0 owns dashboard-listener/provenance/CSRF checks and shows `TRUST_WARNING`
before every false-to-true grant. `TrustChange` accepts only `trusted`;
`warning_shown` is rejected, not an authorization credential.

**M7 seam:** core owns source/credential/trust/plan/install/remove/job
handlers and bounded redacted views. Current root server handlers become thin
listener/provenance/CSRF/DTO facades to those services; current web hosts retain
static serving, installed web rescan and legacy installed-tool loading until
M7's coherent deletion. M6b removes only superseded catalog-v1 fetch/install
paths at full production cutover. H0 delivers working v2 UI now; M7 later moves
assets/rendering to the bundled dashboard and consumes the same HTTP services.
Neither `vosu-surface` nor ABI gains credential lookup, generic config/core
handle or install authority. M7 A7.1's redacted rendering contract is a
separate dependency; no dashboard → core crate edge is introduced here.

Frozen dashboard-listener route inventory (no prefix/wrong-method fallback):

| Method / exact path | Request / successful result |
| --- | --- |
| `GET /api/catalog` | Current redacted catalog/candidate/source status view; no implicit install. |
| `GET /api/catalog/sources` | Ordered nonsecret source records/status and `has_credential`, never secret previews. |
| `POST /api/catalog/sources` | Ordered source/enabled list replacement; no trust/private injection; durable success 204. |
| `POST /api/catalog/sources/refresh` | Explicit refresh of enabled sources/detection failures; redacted status, no implicit install. |
| `POST /api/catalog/sources/<source>/credential` | Token or Basic body from catalog spec; 204 with no echo. |
| `DELETE /api/catalog/sources/<source>/credential` | 204, including absent credential on existing source. |
| `POST /api/catalog/sources/<source>/trust` | `{"trusted": bool}`; 204; fixed official identity cannot be untrusted. |
| `POST /api/catalog/sources/<source>/private-network` | `{"allowed": bool}`; 204; user-added source only. |
| `POST /api/catalog/install` | `{"id","kind","source"}` → complete immutable plan, never immediate job. |
| `POST /api/catalog/plans/<plan_id>/confirm` | `{"approve":[ids...]}` → `{"job_id"}` only after exact recheck/consume. |
| `GET /api/catalog/jobs/<job_id>` | Existing catalog-job polling object; not `/api/v1/jobs`, never action authority. |
| `POST /api/catalog/uninstall` | Exact installed `id/kind/source` plus explicit `remove_data` choice → catalog job; refuse named required dependents, preserve dependency-only data. |
| `GET /api/catalog/art?url=...` | Existing listed-art proxy confinement/CSP/nosniff; credential-free/public-only even for private source. |

Each `<source>` is one fully percent-encoded exact identity, decoded once.
All POST/DELETE routes require dashboard provenance + CSRF **before** effects;
credential mutations are no-store. Unknown sources/jobs/plans are 404,
malformed inputs/approval sets are 400, stale/consumed/busy is 409; unauthorized
provenance/CSRF is 403. Authentication guidance never confirms private-repo
existence from a 404. Refresh publishes through the same generation gate.
Trust UI shows the exact catalog warning immediately before **each** grant;
backend accepts no `warning_shown` authority. Route corpus must cover encoded
slashes, double decoding, wrong listener/method/near miss and token confusion.

#### M6b baseline-to-destination ledger

Baseline is `2c349eb` (M1–M3 + M6a); destinations below are required work,
not claims of completed extraction. Preserve in-flight M4a/M5 changes.

| Baseline authority | Destination / deletion boundary |
| --- | --- |
| `src/tools/catalog_sources.rs` Source/resolve/load_sources/fetch_sources; `catalog-sources.json` and v2/v3 migration markers | S0 core source parser/store/detection/snapshot; preserve exact strings/order/markers for M8, remove legacy resolver/first-wins fetch at H0 cutover. |
| `src/tools/catalog.rs` v1 Catalog validation/order/catalog_view/begin_catalog/install_catalog | SDK validation + R0/P0/core I0; remove v1 fetch/immediate-install/duplicate solver only once v2 production UI/routes compose. Keep existing catalog-job envelope. |
| `src/tools/catalog_sources.rs` overlay install/origin machinery; counter install/download facade | I0 shared prepared-package/transaction records and U0 graph; root web rescan facade until M7. No copied ZIP extractor. |
| `crates/vosu-net/src/streaming.rs` manual redirect/DNS/pinning/ZIP | N0 focused immutable request policy/TLS modules, reused streaming/ZIP primitives; public defaults unchanged. |
| `src/server/api_keys.rs` and broker atomic writers/readers | C0 follows existing tempfile/atomic_replace/read_published convention; do not rewrite API-key verifier or add independent secret backend. |
| SDK `manifest/`, `catalog/` and core schema registration | M6a authority retained; S0 contextual ingestion, R0 predicates, I0 confined asset mirror check; no second validator. |
| Core `plugin/registry.rs` and actual bundled manifests | T0 current source/hash approval decision + M5 real backend seam; registry wiring serialized with M5 owner. |
| Root server route table/service facades and dashboard catalog assets | H0 core-backed exact catalog routes + UI now; M7 moves presentation/assets/final web hosts, no duplicate service. |
| Installed origin/data/settings and old source migration inputs | I0/U0 durable graph/journal preserve data, M8 owns one-time import/no-overwrite replacement/day-0; no opportunistic migration on reads. |

Required counterexamples and evidence workflow are frozen in
[M6b verification](../testing/verification.md#m6b-contract-and-evidence-obligations).
Functional, native Windows, real OS-CA, browser, M5 and owner-forge results
remain separately attributable; contract approval is none of those passes.


### M7: dashboard, overlay host, tool host on the new model

Scope:

- `vosu-dashboard`, `vosu-overlay-host`, `vosu-tool-host` become bundled
  surface plugins (ids `vosu.dashboard`, `vosu.overlayhost`, `vosu.toolhost`,
  D10); core's router is registration-based and still exact (unknown and
  near-miss routes stay 404, `AGENTS.md`).
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
  "Add support for `<name>`" from game detection, install plans with the D8
  disclosure, the D20 notice for overlays that use removed 1.x APIs, the
  D20 notice for the removed `/api/ingame` route with its vosulay install
  action (A7.5), `VOSU_DASHBOARD=off` and its configuration key (ADR 0019).
- `/api/ingame` is not served in 2.0 (section 6); it is a D20 removed route.
- Core lifecycle absorbs the rest of `src/runtime/`; the root package is
  `main.rs` plus the explicit `BUNDLED` array, and startup asserts that the
  registry contains exactly that list (D13).

Exit: rewritten `python tests/tool_host_smoke.py --binary target/debug/vosu`
(launch, bridge, invoke, progress, cancel, 401/403 rules) and
`python tests/dashboard_smoke.py --binary target/debug/vosu --playwright-module <playwright index.mjs>`
against a `cargo build --locked --features test-harness` binary, including
the D20 notice for a fixture overlay that subscribes to
`/websocket/v2/subscribe`, and the `ingame-page` notice with its vosulay
install action after a request to `/api/ingame`;
`python tests/catalog_art_smoke.py`;
`cargo test --locked --test server_compat` (origins, CSRF, API keys,
counters, and `/api/ingame` as the ordinary removed-route 404 on every
method); `cargo test --locked --test config_compat`;
`cargo test --locked --test daemon_smoke` including `VOSU_DASHBOARD=off`.

#### A7 approved contract freeze (2026-11-24)

Main adopted A7.1–A7.5 on 2026-11-24, exactly as recommended and with the
proposed values, under the owner's delegated spec authority. The A7.5
install target is the AF.2 identity, which AF.2 (approved the same day) made
the agreement A7.5 waited for. These decisions are normative. They freeze
M7's inputs and outputs. They do not claim that any route, crate, test,
native run, browser run or measurement exists or passes; every inventory below
states the baseline (`v2/main` `33f957de`) and the required destination.

On 2026-11-25 the owner dropped `/api/ingame` from 2.0
([ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md)'s
amendment). A7.5 below is rewritten as removed-route notice semantics. Its
adopted limits (one warning per process lifetime, 24 hours of inactivity)
and its install target are unchanged.

Inherited contracts are unchanged and are not reopened: A4.1–A4.6 and the
[adapter surface contracts](adapter-surfaces.md), A5.1–A5.8 including M5
U0's `/api/plugins` approval and management routes, A6b.1–A6b.8 including
M6b H0's `/api/catalog/*` inventory, AF.1–AF.13, ADR 0024's console-less
graphical startup, tray **Show logs** and `vosu logs`, and the rule that no
network log endpoint exists.

The full text is here and in:

- [plugin ABI §7.5](plugin-abi.md#75-bundled-only-http-surfaces-d3):
  the A7.1 presentation contract (page context, presentation views, no ABI
  opcode);
- [API v1 "Tool launch sessions (A7.2)"](api-v1.md#tool-launch-sessions-a72):
  the session exchange and its scope;
- [tool protocol "2.0 transition inventory (A7.2)"](../tools/protocol.md#20-transition-inventory-a72):
  retained and deleted tool routes and the frozen v2 bridge type surface;
- [catalog v2 "Bundled host manifests (A7)"](catalog-v2.md#bundled-host-manifests-a7)
  and the M7 additions to [API placement](catalog-v2.md#api-placement);
- [adapter surfaces](adapter-surfaces.md#a74-d20-notice-presentation): A7.4
  notice presentation and the A7.5 `/api/ingame` removed-route notice;
- the 2026-11-24 A7 amendments to ADR [0016](../decisions/0016-separate-dashboard-origin.md),
  [0017](../decisions/0017-tool-host.md), [0019](../decisions/0019-plugin-runtime-and-trust.md)
  and [0021](../decisions/0021-api-v1-and-upstream-adapters.md);
- [M7 test and fixture conventions](../testing/verification.md#m7-test-and-fixture-conventions-a7).

| ID | Decision |
| --- | --- |
| A7.1 | Privileged management handlers stay in core and are registered on the dashboard listener with the existing dashboard provenance and `X-Vosu-CSRF`. Bundled hosts receive only bounded, redacted presentation data, through three enumerated channels: core-owned exact JSON routes the browser calls, core's page-context injection, and two core-pushed presentation views. No host receives a generic core handle, configuration writer, credential reader, App or runtime handle, and the ABI gains no opcode. |
| A7.2 | Retained under the `/tools` prefix, each by exact method and path: the session exchange, the v2 bridge, the theme and design-system assets, each tool's static UI and art, and its password-filtered `_settings`. Deleted: the four named host endpoints (`self`, `settings`, `context`, `overlays`), the v1 bridge, the CLI host routes, every `/tools/<id>/api/*` proxy route and the bridge's `fetch`. No prefix wildcard decides survival or deletion. |
| A7.3 | Boolean `ENABLE_DASHBOARD`, default `true`. `VOSU_DASHBOARD=off` forces the dashboard off; any other value of that variable is a startup error. Off binds no dashboard listener, opens no browser and registers no dashboard route, but `vosu.dashboard` stays in the compiled and initialized bundled registry, inactive. No dashboard control or API changes the key. Tray **Show logs**, `vosu logs` and network-Apply recovery are preserved. |
| A7.4 | N0's accessor and limits are reused unchanged: 128 known overlays with LRU eviction, 24-hour inactivity expiry, one unknown bucket per fixed feature, saturating counts. The dashboard renders elapsed age, feature, count, the escaped installed identity and a fixed migration-guide link. Unknown traffic is a generic advisory with no overlay action. The notice refreshes only with existing status reads; there is no notice timer, persistence or dismissal protocol. |
| A7.5 (rewritten 2026-11-25) | `/api/ingame` is a D20 removed route: the ordinary exact 404 on every method, recorded by the unchanged A4.5 recorder under the fixed feature `ingame-page`. One WARN per process lifetime, on the first request. The recorder's 24-hour inactivity expiry replaces the separate last-use marker; nothing records a URL, IP or referrer. The dashboard shows the D20 notice for `ingame-page`, including its unknown bucket. Its install button starts the normal M6b plan flow for the `kind = "tool"` entry `vosulay-editor` from `18-lab-dev/vosulay` (AF.2; the plan adds `vosu.vosulay`), with every required approval before confirmation. Already installed shows launch and manage. A missing entry or offline catalog shows truthful retry guidance. |
| A7.1-OR (2026-11-25) | `vosu.overlayhost` receives the overlay root as its `static_folder_path` ABI setting, seeded by core from `STATIC_FOLDER_PATH` until M8 migrates the key (not bridge B2). It writes only through `vosu-net`'s hardened ZIP and atomic publish primitives after SDK path validation, and serves only through a core-minted read-only `RootLease`. |

##### Ownership and dependency direction (A7.1)

| Owner | Owns in M7 | Never owns |
| --- | --- | --- |
| `vosu-core` | Both listeners, the exact router, peer/Origin/Host/CORS policy, API keys and the native broker, CSRF and the management-provenance witness, overlay connection accounting, the D20 recorder (including the `/api/ingame` tombstone and its one-per-process warning), the styled 404 page, page-context injection. Configuration and `/api/settingsSave`, the network-Apply journal, the updater, report generation (including the `report.html` template), desktop integration (tray, browser and folder opening), plugin registry, settings storage and approvals, the catalog, and the **tool registry**: per-tool enabled and trust state, settings values, legacy detection, launch codes, launch sessions and action authority. | Presentation HTML, CSS or JS other than the styled 404 page and the generated report. |
| `vosu.dashboard` | The page shells for `/`, `/settings` (including `?overlay=` and `?tool=`), `/available` and `/tools`; the dashboard-only assets; every browser view of management data. | Any management authority, state route, configuration value or overlay-listener route. |
| `vosu.overlayhost` | Counter discovery, the community catalog and its card HTML, download, update, delete, names and aliases, details, catalog update annotations, settings and profile storage, `/websocket/commands` command processing, overlay static serving, alias redirects, `COUNTER_PATH` injection and the preview colour scheme. | Key verification, CSRF, folder opening, configuration, credentials. |
| `vosu.toolhost` | Tool static UI and art, `_settings`, the v2 bridge, the shared design-system assets on both listener roles. | Launch codes, sessions, action authority, tool state or settings writes. |
| Adapters | A4's routes unchanged; no adapter registers `/api/ingame` (section 6). | Any dashboard route. |

Dependency direction:

- A host depends only on `vosu-surface`, `vosu-plugin-api` and, for its
  bundled registration (`export_plugin!(bundled …)`, AF.9), `vosu-plugin-internal`;
  the overlay host also depends on `vosu-net`. A host never depends on
  `vosu-core`, `vosu-sys`, another host, an adapter crate, or any osu!
  reader, state, model or calculation crate. `scripts/crate-boundaries.txt`
  records exactly these edges (I0).
- Core never depends on a host crate. The root's `BUNDLED` array is the only
  code naming the hosts; core receives their descriptors (D13, I0).
- Core reaches a host only through route and feed invocation
  (`SurfaceRequest`, with the `ManagementProvenance` witness on dashboard
  requests), read-only `RootLease`s, and the two presentation views below.
  A host reaches core only through route registration, its responses, and
  the ordinary ABI (logging, settings).
- The browser reaches core and hosts only through the exact routes inventoried
  below. The dashboard listener never serves or proxies API v1.

**Presentation channels (exhaustive).** These are the only ways management
data reaches a host or a page:

1. **Core JSON routes.** The browser calls core's exact dashboard-listener
   routes (the inventory below). The dashboard host renders their responses
   client-side.
2. **Page context.** Core replaces the first `<!--vosu-context-->` marker in
   an HTML `ResponseBody::Bytes` from a `vosu.dashboard` dashboard-role `GET`
   route with `<script id="vosu-context" type="application/json">` carrying
   `overlayOrigin`, `csrf` and `rollbackOrigin`, exactly today's
   `Origins::page_context`. The host never receives the CSRF secret. A
   response without the marker gets no context. Core still applies the
   dashboard CSP, `frame-ancestors 'none'` and `nosniff`.
3. **Presentation views.** Core pushes two immutable, latest-only views,
   each bound to one bundled requester:
   - `ToolHostView` to `vosu.toolhost`: a generation, the dashboard origin
     (or `null` when the dashboard is off) for the bridge's `dashboardUrl`,
     plus per installed tool its id, name, version, a servable flag, its ui
     `entry`, its declared `icon` and `banner` relative paths, its effective
     non-password settings values, and a read-only `RootLease` for its
     package directory. It holds no path, session, code, credential or status
     reason.
   - `OverlayRootView` to `vosu.overlayhost`: a generation and a read-only
     `RootLease` for the effective overlay root, used for confined static
     serving (ADR 0005).

   The overlay host's **write** authority over that root is not a view and
   never goes through a lease: see "Overlay-root storage (A7.1-OR)" below. A
   view carries no pathname, `PathBuf`, OS handle or `vosu-sys` type
   (adapter surfaces R0).

**Redacted DTOs.** Owners serialize these exact shapes; additions need a later
approval, and no field carries a secret, credential, absolute game path or
`internal.paths` value.

| DTO | Owner and route | Fields |
| --- | --- | --- |
| `DashboardStatus` | core, `GET /api/dashboard/status` | Existing `available`, `instances[{client,pid,version,state,pollIntervalMs,precisePollIntervalMs}]` and `connections`, unchanged; plus `removed_routes` (`RemovedRouteNotice[]`) and `update` (`null` or `{"version","major","release_url"}`, today's update notice). No `ingame` field: `/api/ingame` use is a `removed_routes` notice (A7.5). |
| `RemovedRouteNotice` | core, inside `DashboardStatus` | `overlay`: `null` (unknown bucket) or `{"id","name"}` for a currently installed overlay revalidated at this read; `feature`: `"selected-v2-feed"`, `"legacy-skin-index"` or `"ingame-page"`; `attempts` (saturating); `first_seen_seconds_ago`, `last_seen_seconds_ago` (whole seconds from monotonic instants). At most 387 entries (128 overlays × 3 features + 3 unknown buckets). |
| `SettingsView` | core, `GET /api/settings` (new) | `values` for exactly the 17 keys today's Settings page renders (`ENABLE_AUTOUPDATE`, `OPEN_DASHBOARD_ON_STARTUP`, `START_AT_LOGIN`, `DEBUG_LOG`, `CALCULATE_PP`, `READ_MANIA_SCROLL_SPEED`, `POLL_RATE`, `PRECISE_DATA_POLL_RATE`, `API_V1_SUBSCRIPTION_DEBOUNCE`, `SHOW_MP_COMMANDS`, `SERVER_IP`, `SERVER_PORT`, `DASHBOARD_IP`, `DASHBOARD_PORT`, `ALLOWED_IPS`, `MAX_PACKAGE_MIB`, `STATIC_FOLDER_PATH`) and `updates_externally_managed`. Never `ENABLE_DASHBOARD`. |
| `ToolView` (2.0) | core, `GET /api/tools[/<id>]` | `id`, `name`, `version`, `description`, `author`, `homepage`, `icon_url`, `banner_url`, `source` (`dev`, `official`, `user`), `update_available`, `trusted`, `enabled`, `status` (`running`, `blocked`, `untrusted`, `disabled`, `invalid`), `status_detail` (`needs-2.0-update` for a legacy `tool.toml`-only package), `ui_url`, `requires` and `optional` as `[{"id","version","satisfied","provider"}]` (`provider` is the resolved plugin id or `null`), `has_settings`. The service-era fields `kind`, `startup`, `service_role`, `interfaces`, `autostart`, `starts_with_vosu`, `cli`, `api`, `permissions`, `data`, `process`, `required_by` and `path` are removed. T0 serializes `update_available` as `false`: core has no installed-package update source (the catalog UI compares a listing's version with the installed one in the browser), and T0 adds no per-view network fetch. M7 D1 owns feeding it from catalog v2 read-only update information. |
| `ToolSettings` | core, `GET`/`POST /api/tools/<id>/settings` | Unchanged 1.x shape: `{"settings","values","secrets"}` with every `password` value `""`; POST keeps the stored secret for `""` or an omitted field and clears it for `null`. |
| `PluginSettings` | core, `GET`/`POST /api/plugins/<id>/settings` (new, M7 D1) | GET `{"schema","values","secrets"}` from the validated `settings_schema` with defaults applied and every `password` value `""`; POST takes `[{"uniqueID","value"}]` with the same password rule, validates the whole document before storing, and on success delivers the complete effective object as one `SETTINGS_CHANGED` (plugin-abi.md §6.9). An invalid POST changes nothing. |
| `PluginView`, catalog views | core, `/api/plugins`, `/api/catalog/*` | M5 U0 and M6b H0 shapes, unchanged. |

##### Route, method, listener and provider inventory

The baseline router is `src/server/routes.rs::classify`, the
`catalog_api` table, `plugin_management_action`, `src/tools/http.rs`, the
API-v1 router and `src/server/service/surface.rs`. "O" is the overlay
listener, "D" the dashboard listener. Every M7 registration is an exact
method and path, or a validated template or `Children` matcher where the
row says so. A known path with an unlisted method is 405 only where the
owner's existing contract says so, otherwise 404. Every route not listed is
404 on both listeners, without upgrade or static fallthrough.

| Method and path | Listener today | Provider today | M7 owner and disposition |
| --- | --- | --- | --- |
| `GET /`, `/settings`, `/available`, `/tools` | D page; O 302 to D (path and query kept) | `RuntimePages` (`PageDataProvider::render`) | Dashboard host page shells on D; core keeps the O→D 302 for exactly these four paths. Both are absent when the dashboard is off (O 404). |
| `GET /favicon.ico` | O, D | embedded `ServerAssets` | Tool host, both roles (design-system set). |
| `GET /assets/<name>` | O, D, every file in `assets/server/` | embedded `ServerAssets` | Exact per-name registrations. Design-system set (tool host, both roles): `tokens.css`, `system.css`, `theme-init.js`, the eight `.woff2` fonts, `fonts-OFL.txt`. Dashboard set (dashboard host, D only): `homepage.css`, `homepage-controls.css`, `homepage.js`, `homepage-settings.js`, `dashboard-catalog.css`, `dashboard-catalog.js`, `dashboard-catalog-browse.js`, `dashboard-catalog-flow.js`, `dashboard-catalog-sources.js`, `dashboard-notices.css`, `dashboard-notices.js`, `dashboard-plugins.css`, `dashboard-plugins.js`, `dashboard-tools.js`, `dashboard-update.js`, `update-notice.css`. Removed (404): `homepage.html`, `report.html`, `design.html`, `tool-bridge.js`, `tool-bridge-v1.js` under `/assets/`, and every dashboard-set name on O. |
| `GET /api/dashboard` | O, D | core transport | Core, unchanged `{"origin"}`. 404 when the dashboard is off. |
| `GET /api/dashboard/status` | D | `ReportInputProvider::dashboard_status` + connections | Core, `DashboardStatus`. |
| `GET /api/settings` | — | — | New core D route, `SettingsView`. |
| `POST /api/settingsSave` | D (management) | `ConfigUpdateService` | Core, unchanged, except that a body naming `ENABLE_DASHBOARD` is 400 and publishes nothing. |
| `POST /api/runUpdates` | D (management) | `UpdaterHandoff` | Core, unchanged. |
| `POST /api/generateReport` | D (management) | `generate_report_document` | Core, unchanged; ~~the report lists `vosu.dashboard` as `inactive` when off~~. **Main note (2026-11-25):** the report is generated only through the dashboard, so it cannot show the dashboard off. The intent is met by the core plugin status surface that already exists: the roster behind `GET /api/plugins` lists `vosu.dashboard` as `inactive` with reason `dashboard_disabled` (`src/runtime/plugin_management.rs`; asserted by the `dashboard_off` lifecycle test), and startup logs the same line (`Dashboard is off: vosu.dashboard is inactive (dashboard_disabled)`; asserted by `daemon_smoke`). Neither `vosux` nor API v1 has a plugin list, so neither gains one here. |
| `GET`, `POST /api/keys` | D (management) | `ApiKeys` | Core, unchanged. |
| `GET /api/counters/search/<query>?source=installed\|available` | O, D | `PageDataProvider::search_counters` (HTML fragment) | Overlay host, D only; the HTML contract is unchanged, except that card URLs are origin-relative and the dashboard prefixes `overlayOrigin` from page context. |
| `GET /api/counters/details/<folder>`, `GET /api/counters/updates`, `POST /api/counters/names/<folder>`, `POST /api/counters/download/<url>?name=&update=`, `POST /api/counters/delete/<folder>` | D (POSTs management) | `ProductionRouteDelegate` counter handlers | Overlay host, D only; mutations require the core witness. |
| `POST /api/counters/open-root`, `POST /api/counters/open/<folder>` | D (management) | `CounterService::open*` | Core (desktop integration), D only, confined to one normal component of the effective overlay root. |
| `GET`, `POST /api/counters/settings/<folder>[?profile=&update=yes]`, `GET`, `POST /api/counters/profiles` | O, D; POST by dashboard, `settings:write`/`profiles:manage` key or broker | counter settings handlers | Overlay host, both roles. Core evaluates the unchanged key, broker and dashboard rules before dispatch; `update=yes` stays dashboard-only. The provider sees no credential. |
| `GET /websocket/commands` (upgrade) | O; key/broker scope `settings:write` | `CommandProcessor` | Overlay host feed, O only. Core keeps admission, key grants and revocation close. |
| `GET`, `HEAD` overlay static `/<folder>/…`, alias redirects | O | `CounterStatic` + `alias_redirect` | Overlay host, O only, through `OverlayRootView`'s `RootLease`; preview requests stay excluded from connection counts. |
| `GET /json`, `/json/sc`, `/json/v2`, `/json/v2/precise`, `/api/calculate/pp`, `GET /backgroundImage`, `GET /Songs/…`, `GET`/`HEAD /files/beatmap/…`, `GET`/`HEAD /files/skin/…`, WS `/ws`, `/tokens`, `/websocket/v2`, `/websocket/v2/precise` | O (A4 adapter registrations) | adapters through `adapter_routes` | Unchanged A4 owners, O only. |
| `/websocket/v2/subscribe`, `GET /files/skin/` | O (removed, recorded) | core `vosu_core::removed_routes::RemovedRoutes` | Unchanged exact 404 tombstones; core keeps the recorder, one per process. On D they are unknown-route 404s and unrecorded. |
| `/api/ingame`, every method | — (404) | — | Core, O only: an exact 404 tombstone recorded as `ingame-page` (A7.5); never registered by an adapter or host. On D it stays an unknown-route 404. |
| `/api/v1/*` | O; D 404 | API-v1 router | Core, unchanged ([route inventory](../testing/verification.md#final-api-v1-route-inventory)); `X-Vosu-Tool-Session` authority moves to core's tool registry. |
| `GET /api/plugins`, `POST`/`DELETE /api/plugins/<id>/approval`, `POST /api/plugins/<id>/{restart,stop}` | D | `runtime::plugin_management` | Core, unchanged (M5 U0). |
| `GET`, `POST /api/plugins/<id>/settings` | — | — | New core D routes (`PluginSettings`). |
| `/api/catalog/*` (H0's 20 method/path pairs) | D | `catalog_api` | Core, unchanged (M6b H0). |
| `GET /api/tools`, `GET /api/tools/<id>` | D | `ToolHost::dashboard` | Core, `ToolView` 2.0. |
| `POST /api/tools/<id>/{enable,disable,trust}` | D (management) | `ToolHost::dashboard` | Core; `enable` of a legacy package is 400 `needs-2.0-update`. |
| `POST /api/tools/<id>/launch` `{"theme"}` | D (management) | `ToolHost::dashboard` | Core, unchanged response `{"url"}` (A7.2 limits). |
| `GET`, `POST /api/tools/<id>/settings` | D (POST management) | `ToolHost::dashboard` | Core, `ToolSettings`. |
| `POST /api/tools/<id>/{start,stop,restart,open-data,clear-cache}`, `GET /api/tools/<id>/logs`, `PUT /api/tools/<id>/autostart` | D | `ToolHost::dashboard` | **Deleted** (404). |
| `GET /tools` on O | O 302 | core | Core 302 to D; 404 when off. |
| `POST /tools/_host/v1/session` | O | `ToolHost::host_api` | Core (A7.2), O only. |
| `GET /tools/_host/v2/bridge.js`, `GET /tools/_host/v1/theme.css`, `GET /tools/_host/v1/design` | O | `ToolHost::overlay`/`host_api` | Tool host, O only. |
| `GET`, `HEAD /tools/<id>/`, `/tools/<id>/<path>`, `/tools/<id>/_icon`, `/tools/<id>/_banner`; `GET /tools/<id>` 302 to `/tools/<id>/` | O | `ToolHost::overlay` | Tool host, O only, through `ToolHostView`; 503 when not servable, art in every state. |
| `GET /tools/<id>/_settings` | O | `ToolHost::overlay` | Tool host, O only, password values omitted. |
| `GET /tools/_host/v1/{self,settings,context,overlays}` | O (tool token) | `ToolHost::host_api` | **Deleted** (404). |
| `GET /tools/_host/v1/bridge.js` | O | `ToolHost::host_api` | **Deleted** (404). |
| `GET /tools/_host/v1/cli`, `POST /tools/_host/v1/cli/<id>`, `POST /tools/_host/v1/cli/<id>/open`, `PUT /tools/_host/v1/cli/<id>/autostart`, `DELETE /tools/_host/v1/cli/session/<session>` | O (broker) | `ToolHost::cli_api` | **Deleted** (404). |
| every method on `/tools/<id>/api/<rest>`, including upgrades | O | `ToolHost::proxy` | **Deleted** (404, no static fallthrough). |
| test-harness adapter capture endpoint | O, loopback capability | `adapter_capture` | Unchanged, `test-harness` only. |

Core's baseline route reservations (`reserve_core_routes`) hold whole
prefixes on both roles: `/assets`, `/api/dashboard`, `/api/counters`,
`/tools`, `/api/tools` and `/api/v1` with every method. M7 replaces each with
the exact registrations above. No prefix reservation survives except
`/api/v1` on the overlay role, which core's own API-v1 router serves.

##### Privileged management operations

Each is a dashboard-listener `POST`, `PUT` or `DELETE` that requires dashboard
provenance and `X-Vosu-CSRF` **before** any effect, except where a row names a
key scope. API keys never authorize any other row.

| Operation | Owner | Authorization |
| --- | --- | --- |
| Configuration save, network Apply and confirm | core | dashboard + CSRF |
| Update installation (`runUpdates`, major-consent) | core | dashboard + CSRF |
| Report generation | core | dashboard + CSRF |
| API-key list, create, revoke | core | dashboard + CSRF (also for GET) |
| Plugin approve, revoke, restart, stop, settings write | core | dashboard + CSRF |
| Catalog sources, credentials, trust, private network, refresh, install, confirm, uninstall, promote, offer accept | core | dashboard + CSRF |
| Tool enable, disable, trust, launch, settings write | core | dashboard + CSRF |
| Open overlay root or folder | core | dashboard + CSRF |
| Overlay download, delete, names, developer settings (`update=yes`) | overlay host | dashboard + CSRF (core witness) |
| Overlay settings write, profile management | overlay host | dashboard + CSRF, or `settings:write` / `profiles:manage` key, or native broker (unchanged) |
| `/websocket/commands` settings writes | overlay host | `settings:write` key or broker on upgrade (unchanged) |

##### State consumers and pointers

The baseline has no `SnapshotManager`, `RegularState` or B1 (M4a removed
them). The first-party state consumers and their M7 destinations are:

| Consumer (baseline) | Reads today | M7 destination |
| --- | --- | --- |
| Dashboard status card (`homepage.js` every 2 s) | `GET /api/dashboard/status` host diagnostics | Unchanged core diagnostics, plus the browser's `GET /api/v1/sessions` from the overlay origin (session summaries, which create no demand) for game-agnostic labels. No API-v1 subscription and no new precise demand. Poll intervals are shown only for sessions whose `vosu.client.game` is `osu`. |
| Tool context (`src/runtime/tool_context.rs`, `crates/vosu-adapter-upstream/src/files/tool_context.rs`, `PluginHost::display_path` for it) | selected session's absolute songs, game, skin and beatmap paths | **Deleted** with `/tools/_host/v1/context`. Tools read `/vosu.client`, `/vosu.chart`, `/vosu.skin` and granted `vosu.osu` fields and resources through API v1. |
| Tool overlay listing (`/tools/_host/v1/overlays`) | absolute overlay folders | **Deleted**. |
| v1 bridge `subscribe` (`tool-bridge-v1.js`) | the removed `/websocket/v2/subscribe` | **Deleted** with the v1 bridge. |
| v2 bridge (`tool-bridge.js`) | `WS /api/v1/sessions/{session}/subscribe` with caller pointers; action and job routes | Unchanged, plus the A7.2 removals. |
| Report generation (`RuntimeReportInput::capture`) | host diagnostic sessions, counters, log directory | Core, unchanged; counter facts come from the overlay root through core's own discovery read, not from the overlay host. |
| Bundled adapters | A4 captures and subscriptions | Unchanged. |

No first-party consumer reads `internal.paths`, and none places an absolute
game path in HTML, bootstrap context, JSON, a cache or a browser log.

##### Asset inventory

All 32 files of `assets/server/` keep their paths, bytes and
`assets/SHA256SUMS` entries unless a unit changes them through the reviewed
digest workflow. Each is embedded exactly once:

| Owner | Files |
| --- | --- |
| Tool host (design-system set and bridge) | `tokens.css`, `system.css`, `design.html`, `theme-init.js`, `favicon.ico`, eight `.woff2` fonts, `fonts-OFL.txt`, `tool-bridge.js` |
| Dashboard host | `homepage.html`, `homepage.css`, `homepage-controls.css`, `homepage.js`, `homepage-settings.js`, `dashboard-catalog*.{js,css}` (five files), `dashboard-notices.{js,css}`, `dashboard-plugins.{js,css}`, `dashboard-tools.js`, `dashboard-update.js`, `update-notice.css` |
| Core | `report.html` (report generation) |
| Deleted | `tool-bridge-v1.js` |

`theme.css` stays the concatenation `tokens.css` + `\n` + `system.css`,
served from those same embedded bytes.

##### Legacy deletion inventory

T0, T1 and C0 delete these, migrating every in-repo caller and test first.
Each test-assertion removal needs the `Assertions-Removed:` trailer and a
replacement security assertion (X0).

- `src/tools/{process,cli,http}.rs` service, proxy, host-API and CLI
  paths; `src/tools/manifest.rs` (`tool.toml` loading and its fallback);
  `src/tools/capabilities.rs` service permissions; the old
  `RouteExtension` in `src/server/service/routing.rs`; service tokens,
  readiness, ports, health probes, backoff, autostart and logs.
- `docs/tools/tool.schema.json` and the `[service]`, `[cli]`, `[api]`,
  `[permissions]` and `[[requires]]`-by-tool-id manifest contract.
- The vosux dotless tool form and the options `--list`, `--json`, `--open`
  and `--autostart`. `vosux <plugin-id> <action> [json] [--detach]
  [--session <id>]` and `vosu x` stay; `--help` describes only the action
  form. A dotless first argument exits 2 (usage).
- `assets/server/tool-bridge-v1.js`, `vosu.fetch`, and the
  `/assets/`-served copies listed above.
- `src/runtime/tool_context.rs`, the upstream adapter's `tool_context`
  view, and `PluginHost::display_path` once nothing else calls it.
- The broad `reserve_core_routes` prefixes, `Endpoint::{ToolOverlay,
  ToolManagement}` and every root route delegate that a host or core route
  replaces.

The plugin-host supervisor, `vosu_sys::process_spawn`, M5's reused process,
job and backoff helpers, `--plugin`, `--tool <dir>` (now loading only a
`kind = "tool"` `manifest.toml`), and plugin-action vosux stay. Legacy
installed tools keep their files, settings and data untouched and are
reported, not executed (`needs-2.0-update`). M8 owns their one-time
migration.

##### Dashboard off (A7.3)

- **Inputs.** `ENABLE_DASHBOARD` in `vosu.toml`, boolean, default `true`,
  validated like every other boolean key. Environment `VOSU_DASHBOARD`:
  unset leaves the key in charge; exactly `off` forces off; any other value,
  including empty, `on` or `OFF`, fails startup with a configuration error
  naming the variable, before any listener binds. The effective value is
  read once at startup; a live edit of the key is logged as needing a
  restart and is not applied. `/api/settingsSave` rejects the key, and
  `SettingsView` never contains it.
- **Effects when off.** No dashboard listener binds, and
  `OPEN_DASHBOARD_ON_STARTUP`, the second-launch handoff and the tray open no
  dashboard. The second-launch dashboard probe is skipped, so the existing
  single-instance and bind refusals apply. The tray keeps **Show logs** and
  **Quit** and omits **Open dashboard**. `GET /api/dashboard` and the O→D
  redirects are 404. Core registers no dashboard-listener route, including
  its own management routes.
- **Registry.** `vosu.dashboard` is validated and registered, so the
  registry still equals `BUNDLED` exactly, but no instance is created: no
  `create`, routes, settings delivery or state demand. Diagnostics list it as
  `inactive` with reason `dashboard_disabled`. Overlay, adapter, API-v1 and
  tool hosting are unaffected. Process plugins needing approval stay
  `approval_required` with A5.5's WARN.
- **Recovery.** The network-Apply journal is still recovered by the owning
  runtime before listeners start. Editing the key in `vosu.toml` and
  restarting re-enables the dashboard; nothing in the dashboard can disable
  itself (ADR 0019).

##### `/api/ingame` removed (2026-11-25)

The owner dropped `/api/ingame` from 2.0 on 2026-11-25. Landing **G** and its
units B0 and G0 are withdrawn. No behavioral document, eligible author or
clean-room implementation is needed, and M8 does not wait on any of them.
`/api/ingame` is a D20 removed route (A7.5). N1 adds its `ingame-page`
tombstone, warning and notice to the recorder it moves into core. The
notice's vosulay action uses the install-plan affordance that D1 delivers.

##### Unit file ownership

| Unit | Owns |
| --- | --- |
| A7 | This section and the documents it links |
| S0 | Dashboard status state reads, `homepage.js` status card |
| D0 | `crates/vosu-dashboard/`, dashboard page shells and dashboard-set assets, `src/server/pages.rs` presentation |
| O0 | `crates/vosu-overlay-host/`, `src/counters*`, `src/server/{catalog.rs,commands*}` |
| T0 | `crates/vosu-tool-host/`, `src/tools/` deletion, core tool registry and launch authority |
| T1 | `assets/server/{tool-bridge.js,tool-bridge-v1.js,tokens.css,system.css,design.html}`, `docs/tools/{protocol,design}.md`, bridge browser tests |
| D1 | Dashboard management views, `/api/plugins/<id>/settings` and `/api/settings` handlers |
| N1 | The D20 notice view and its `DashboardStatus` field, including the `/api/ingame` tombstone, warning and vosulay install action (A7.5) |
| C0 | Remaining `src/runtime/` and generic `src/server/` moves into core |
| I0 | Workspace and crate manifests, lockfile, `BUNDLED`, route table, digests, boundary inventories, test roots |
| X0, X1 | Rewritten smokes and fixtures; evidence and documentation closure |

**Counterexamples ruled out by this text:** a generic core handle disguised as
a surface view; a removed host endpoint surviving because its prefix also
serves the session exchange; off-mode dropping a compiled plugin from the
exact registry list; a tool host minting or checking a session; a dashboard
page reading configuration in-process; a CSRF token reaching a provider;
`/assets/report.html` or the v1 bridge still served; a prefix reservation
serving a deleted route; the dashboard listener serving API v1 or a state
route.

##### Overlay-root storage (A7.1-OR, approved 2026-11-25)

Main approved option (a) on 2026-11-25. The overlay host must write inside
the effective overlay root (community downloads and updates, delete, names
and aliases, settings and profile files), while no path may cross
`vosu-surface`, surface root operations are read-only, and bridge B2 must not
gain a second plugin id. Frozen:

1. **Setting.** `vosu.overlayhost` declares `static_folder_path` in its
   `settings_schema` and receives it through ABI §6.9 (`init.settings`, then
   complete `SETTINGS_CHANGED` objects). Core's M7 per-plugin settings
   storage holds it. Until M8 migrates the key, core seeds and keeps it from
   the existing `STATIC_FOLDER_PATH`, resolved to an absolute path by the
   unchanged rule (`./static` maps to the data root; another relative path
   resolves from the launch directory). The Settings page still edits
   `STATIC_FOLDER_PATH` through `/api/settingsSave`, and each effective change
   reaches the host as one `SETTINGS_CHANGED`. It is **not** added to bridge
   B2. M8's migration list gains this key.
2. **Writes.** The host performs every write under the root itself, using only
   `vosu-net`'s existing hardened primitives: `extract_zip_atomic` (path,
   symlink, entry-count and expanded-size checks) and the atomic
   publish/replace helpers. It validates every relative name with the shared
   SDK path rules before use. It makes no ad-hoc `std::fs` write outside those
   primitives; a missing primitive is added to `vosu-net`, never inlined.
   Settings, profile and name files are published same-directory and
   atomically, and their readers use the matching retrying read
   (`AGENTS.md`).
3. **Serving.** Static serving still uses `OverlayRootView`'s core-minted
   read-only `RootLease`. A write never goes through a lease.
4. **Tests (O0).** Traversal and absolute names, symlinked archive or folder
   entries, the root changing while a download is in flight, and a write
   racing a served read.

Core's `open-root` and `open/<folder>` handlers read the same effective
value from the storage core owns.

### M8: 1.x migration and day-0 release gate

Scope: first-start migration exactly per catalog-v2.md "First-start migration
from 1.x" (sources verbatim with `trusted: false` except official identities;
official tools queued through `replaces`; user tools disabled with
`needs-2.0-update`; user overlays left enabled, flagged by D20 when they use
removed APIs) plus removed configuration keys, the D19 move of the
osu!-specific keys into `vosu.osu` settings (deleting bridge B2) and the
A7.1-OR move of `STATIC_FOLDER_PATH` into `vosu.overlayhost`'s
`static_folder_path` setting (ending core's M7 seeding), idempotent
like `src/config/migration.rs`; the day-0 check (catalog-v2.md "Day-0
release check") as `scripts/check-day0.py` wired into `release.yml` and
covered by `scripts/test_gate_tooling.py`, extended to fail closed when
`tests/fixtures/upstream/DIVERGENCES.md` contains any `temporary`-class
entry, or any entry whose class is missing or is none of the four defined
classes (D6's `stable-tournament` exemption must be gone by day-0, which the
stable manager aggregate task removes; the file's permanent `security`,
`contract`, and `licensing` entries, correctly recorded, never fail the gate);
README, quickstart, CHANGELOG,
architecture, and ADR statuses; packaging (section 7). Release requires
every official plugin (vosu-tools plugins and the vosulay supervisor) to
have passed a process-runtime smoke in its own CI against the latest alpha.

Exit: new `tests/migration_compat.rs` over 1.x config/data fixtures for Linux
and Windows path layouts, run twice to prove idempotence and once after a
simulated crash mid-migration; `cargo test --locked --test config_compat`;
`VOSU_RELEASE_SMOKE_BINARY=<packaged-binary> cargo test --locked -p vosu-release-smoke --test release_smoke -- --ignored --nocapture`
extended to start `vosu plugin-host` from the packaged binary, read
`/api/v1/sessions`, confirm the bundled set, and assert that `GET /api/ingame`
on the packaged binary's overlay listener is the ordinary removed-route 404
with no upgrade and no `Deprecation` or `Link` header (ADR 0021's
2026-11-25 amendment; no `ingame_compat` target exists); native updater tests;
`python scripts/check-day0.py` passing against the four repositories; M4b's
full set green; an owner-run live session on Windows stable, Windows lazer,
and Linux lazer through menu → play → paused → result with one official
tool (its plugin under `runtime = "process"`), one overlay, and vosulay
installed through the resolver.

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
| `tests/config_compat.rs` | config format, migration, reload | M3 (on `main` until M0 lands) | M4a for removed keys; M8 for the D19 move and migration |
| `tests/daemon_smoke.rs` | end-to-end startup and serving | every milestone; extended in M3, M7 | — |
| `tests/updater_compat.rs`, `updater_native.rs` | transactional update | M−1 adds consent (1.2.4) | — |
| `tests/tool_host_smoke.py`, `tests/fixtures/tool-host/` | 1.x tool services | M6 (M3 adds a bridge case) | M7 rewrite for web-only tools |
| `tests/dashboard_smoke.py`, `catalog_art_smoke.py`, `tool_catalog_smoke.py` | dashboard and catalogs | M5 (M−1 adds the update-consent prompt) | M6b (catalog v2), M7 |

Golden fixtures come from two sources with different owners:

1. **Projection goldens need no live session.** `tests/fixtures/oracle/`
   already holds per-scenario inputs and the pinned upstream builders' outputs
   (`v1.json`, `v2.json`, `v2-precise.json`, `sc.json`), generated by
   `tools/oracle/generate.ts` at the revisions in `tools/oracle/oracle-lock.json`
   and checked by `tests/state_api_compat.rs` (`EXPECTED_REVISIONS`, lines
   39–45). M4a reuses them unchanged as the adapters' payload goldens.
2. **Behavioral goldens need a live session (owner-authorized, M4b).** Status
   codes, error bodies, headers, file routes, `/api/calculate/pp`, WebSocket
   framing and cadence, and near-miss routes are observable only from a
   running pinned upstream with a live osu! client. ADR 0021 requires these
   captures to be committed and checked in CI. The tooling extension lands
   first in M4b without the owner; each capture session needs the owner's
   written authorization and runs on isolated VM test installs, operated by
   an agent within that authorization (A4b.1, D14). A one-pair **pilot** runs
   as soon as the tooling lands, so mismatches surface early; **M8 (the
   release) blocks on the full set; no earlier milestone does.** Each later
   pin bump repeats the capture (ADR 0021). These behavioral goldens never
   replace the projection goldens above; their contract is
   [GOLDEN_CONTRACT.md](../../tests/fixtures/upstream/GOLDEN_CONTRACT.md).

Between M4a and M4b the adapters are checked by the source oracle plus the
1.x behavioral tests in `server_compat`, which already encode the upstream
behavior 1.x reproduced; M4b turns that from "same as 1.x" into "same as the
pinned upstream".

## 6. `/api/ingame` (withdrawn)

Until 2026-11-25 this section required a clean-room `/api/ingame`. An
eligible author who had never read upstream's LGPL page assets would write
a behavioral specification, and a second clean contributor would implement
it. The owner withdrew that requirement on 2026-11-25
([ADR 0021](../decisions/0021-api-v1-and-upstream-adapters.md)'s
amendment). vosu 2.0 serves no `/api/ingame`. vosulay is the supported
in-game overlay path, through the official `vosulay-editor` tool and its
`vosu.vosulay` supervisor.

- No `docs/v2/ingame-behavior.md`, eligible author, observation session,
  second implementer or `tests/ingame_compat.rs` exists or is planned.
- `/api/ingame` is a D20 removed route
  ([adapter surfaces A7.5](adapter-surfaces.md#a75-apiingame-removed-route-notice)).
  Every method gets the ordinary exact 404, and requests are attributed
  under `ingame-page`. Core logs one WARN per process, and the dashboard
  notice offers the vosulay install plan.
- M4b still captures no `/api/ingame` record. No fixture or document
  contains upstream page HTML, JavaScript or CSS (A4b.12).

## 7. Ecosystem fan-out

ADR 0022 starts ecosystem work once the contracts are frozen (they are,
amended by A1) and the SDK and web bridge exist. **Trigger: M3 and M6a landed
on `v2/main` and `v2.0.0-alpha.1` published as a GitHub prerelease (M−1
publishing fix).** At that point the SDK can build and load an external
plugin (M1), plugins can publish namespaces (M2), declare resources and
actions, and tools can call them through `vosu.invoke`/`vosu.start` and
subscribe through API v1 (M3), and every package kind has a validator and
JSON Schema (M6a).

- **SDK:** `vosu-plugin-api` `1.0.0-alpha.N` prereleases on crates.io, so a
  plugin build never clones this repository (D11).
- **Testing data:** alpha builds keep their state in the prerelease
  subdirectory of each vosu root and never rewrite 1.x configuration, so
  testers can return to 1.x by simply running it (D21).
- **Runtime:** until M5, ecosystem plugins load in-process through
  `--plugin <dir>` in debug builds only; from M5, `--plugin <dir>` in alpha
  builds loads them with `user` trust under `runtime = "process"` after
  approval, and from M6b they install from their own sources. Official
  plugins may request `in-process` for release, but each must pass a
  process-runtime smoke in its own CI before M8 (section 4).
- **Plugin ids:** the bundled hosts are `vosu.toolhost` and
  `vosu.overlayhost` (D10), so `vosu.tools.<name>` and
  `vosu.overlays.<name>` stay available for official plugins
  (catalog-v2.md:120-131 rejects ids that extend a registered id).

One owner agent per repository; each repository pins an alpha and moves to
each newer alpha deliberately.

| Repository | Changes (ADR 0022, specs, adopted decisions) |
|---|---|
| vosu-tools | Each `[service]` becomes a plugin crate on `vosu-plugin-api` (replacing `crates/vosu-tool-sdk`) with id `vosu.tools.<tool>`: `[api].write` routes become `[[actions]]` with `args_schema`/`result_schema`; read routes that return state become a plugin namespace whose schema `[[provides]]` declares (D7); file-returning routes become resources or trees; `[cli]` becomes actions with `cli = true` (`vosux <plugin-id> <action>`). `[permissions].host = ["context"]` becomes `requires` on `vosu.osu` fields and, where a raw path is truly needed, `vosu.osu/internal/paths` (D8) — replacing the `songs_dir` reinterpretation in `tools/replay-renderer/src/main.rs:628-636`; `songs:write` writers (the downloader, practice) call `vosu.osu.beatmap.import` instead of writing paths (D8). Each `tool.toml` becomes `manifest.toml` `kind = "tool"` with `[ui]` and `requires`/`optional`; plugin manifests carry `runtime`, `[targets.*]`, `provides`. `packages/tool-web` (`src/bridge.d.ts`) adopts the 2.0 bridge; every `/json/v2` and `/websocket/v2/subscribe` consumer moves to API v1 pointers using state-schema.md's mapping table. `scripts/package.py` emits `catalog-v2.json` beside `catalog.json` (catalog-v2.md "1.x coexistence"). CI runs each plugin under `runtime = "process"` against the latest alpha. |
| vosulay | A small external official plugin, `vosu.vosulay` (never bundled), supervises vosulay's separately installed host executable as a child process (D16, validated by S1; AF.7): a job object on Windows, with the host launched at Medium integrity even from an elevated daemon; `PR_SET_PDEATHSIG` on Linux, without the generic plugin-host `no_new_privs` or user-namespace denial; bounded private lifecycle, configuration and editor-intent IPC; start/stop/restart/status actions. Per AF.8, the attached child reads session and focus from API v1 itself; the supervisor does not forward them over stdin. The host keeps its CEF sandbox (`vosulay/docs/security.md:58-60`, `:265`) and its standalone mode against upstream-compatible backends. The editor becomes the `kind = "tool"` UI `vosulay-editor`; consumers of `/websocket/v2/subscribe` and `keys.all` (`src/backend.rs`, `src/runtime/editor/rulesets.rs`) move to API v1 when attached to vosu. It is the `/api/ingame` successor named in the `Link` header. Publishes `catalog-v2.json`; its winget/Scoop/AUR packaging is unchanged. |
| vosu-overlays | `metadata.txt` becomes `manifest.toml` `kind = "overlay"` with `requires`/`optional` field ids (common-layer fields where possible so overlays work across games); `overlays/*/overlay.js` move from `/websocket/v2/subscribe` to `WS /api/v1/sessions/active/subscribe`; `scripts/package.py` publishes both catalogs. |
| vosu-overlay-quickstart | `app.js` (its `/websocket/v2/subscribe` at `app.js:449`) uses API v1 (api-v1.md "Worked examples"), ships a `manifest.toml` template, `TUTORIAL.md` rewritten with a section on porting overlays built from the 1.x template (D20); a release tagged `v2.x` (required by the day-0 check). |
| packaging (`packaging/` here) | `nfpm.yaml` and AUR install hooks keep running `vosu --setup`; the plugin host is the same executable, so no new file capabilities; the `vosux` symlink stays; the release adds `include/vosu_plugin.h` as an asset for C plugin authors; `scripts/package-release.py` notes describe 2.0. Starts after M5 (plugin host exists). |
| docs | README routes, configuration, and CLI; quickstart; a plugin author guide (SDK, manifest, trust, runtime); an overlay author guide (API v1, resources, common layer); the 2.0 tool protocol; a 1.x→2.0 migration guide including overlays that used removed APIs (D20) and the alpha data location (D21); each ecosystem repository's README. Public docs name only osu! as a supported game. Runs continuously from the trigger; final at M8. |

The day-0 gate (M8) passes only when vosu-tools, vosulay, and vosu-overlays
each have a published release with `catalog-v2.json` and the quickstart's
latest release is major version 2 (catalog-v2.md "Day-0 release check").

### AF: ecosystem contract freeze (approved 2026-11-24)

The F plan's AF decisions are adopted. The owner decided AF.1, AF.4, AF.5,
AF.7 and AF.11; Main adopted AF.2, AF.3, AF.6, AF.8, AF.9, AF.10, AF.12
and AF.13 exactly as recommended, under delegated spec and ecosystem
authority. The [ecosystem release matrix](ecosystem-release-matrix.md) is
the normative record of identities, versions, the settings and data map,
pins, frozen 1.x sources, platforms, publication owners and receipts;
the specs carry the contract changes:

- **AF.1 release actions.** Every push, tag, upload, signature and
  publication (host alphas, SDK alphas, ecosystem releases) is a separate
  owner approval naming its exact version and target. `v2.0.0-alpha.1` is a
  GitHub prerelease with `make_latest=false`. A version bump never implies
  a crates.io upload.
- **AF.2 identities.** Seven `vosu.tools.<name>` plugins, the web tools
  `<name>-v2` (six companions plus `map-preview-v2`), `vosu.vosulay` with
  `vosulay-editor`, and `ratings-timeline-v2`; same-kind `replaces` only.
  Service data and native or secret settings move through the matrix's closed
  map, executed once by M8 ([catalog v2](catalog-v2.md#first-start-migration-from-1x)).
- **AF.3 long work** and **AF.6 uploads, exposure and timeline**:
  [API v1](api-v1.md#plugin-owned-operations-af3) and the matrix's exposure
  ledger.
- **AF.4 coexistence** and **AF.5 visibility**: [catalog v2](catalog-v2.md#1x-coexistence)
  and [ADR 0022](../decisions/0022-install-resolver-and-2-0-transition.md#amendments).
- **AF.7, AF.8 vosulay**: D16 amendment below.
- **AF.9 SDK surface** and **AF.10 package checker**: D11 amendment below
  and [catalog v2](catalog-v2.md#package-checker-af10).
- **AF.11 live verification**: isolated VM test installs only, under Main's
  VM ownership.
- **AF.12 artifacts**: [plugin ABI §6.13](plugin-abi.md#613-invocation-owned-artifacts-af12)
  and [API v1](api-v1.md#job-artifacts-af12); implemented by B0 in a later
  host and SDK alpha, not promised by alpha.1.
- **AF.13 private transports**: [plugin ABI §7.5](plugin-abi.md#75-bundled-only-http-surfaces-d3)
  and the ADR 0019/0021 amendments.

Recorded facts at the freeze (read-only checks on 2026-11-24): M3 and M6a are
on `v2/main` at `49e95cf`; `v2.0.0-alpha.1` is not published, so the trigger
above is not yet met; crates.io has no `vosu-plugin-api`; the frozen 1.x
catalog sources are vosu-tools `v0.4.3`, vosulay `v1.5.3` and vosu-overlays
`v0.4.0`; the quickstart repository is private with no release. The 18
overlays in vosu-overlays `v0.4.0` beyond ratings-timeline have no v2
source on that repository's `main`; their port or retirement is an open
overlays-owner decision recorded in the matrix.

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

Adopted goals: leaf-crate edit `cargo check` ≤ 1.0 s, `cargo build`
≤ 2.5 s, crate library test build ≤ 3.0 s; `vosu-osu-reader` edit
`cargo check` ≤ 2.0 s; a `vosu-core` edit no slower than today's whole-crate
rebuild. Incremental edits are not the whole cost (D9), so the same script
also records, with a no-regression budget against the pre-M0 commit: the
full `python scripts/prepush.py` wall-clock time; the native Windows Clippy,
test, and doctest run (`scripts/native-ci.ps1`), where one test binary per
crate adds MSVC link time; clean local and hosted CI build times; and the
rebuild caused by alternating `cargo test -p <crate>` with workspace
`--all-features` builds (feature unification of the per-crate
`test-harness` features).

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

Planning against the 1.x code, and the adversarial review of the first
draft, exposed these gaps. Each is closed by an adopted decision in section
10 or by a milestone task; every spec amendment lands in A1.

1. **Paths the adapters must echo.** Upstream JSON carries raw folder and
   file paths (`src/model/api/v2.rs` `Folders`, `DirectPath`;
   `src/model/api/v1.rs` `Folders`, `BeatmapPaths`) that `vosu.osu` omits,
   and state-schema.md:292 says state never carries a raw path. Only the
   paths need hiding: the upstream-only derived values
   (`src/model/api/sc.rs:132-141`) are computed by the streamCompanion
   adapter itself. → D2.
2. **No HTTP in the ABI.** The ABI has no HTTP opcode and ADR 0021:61 says
   plugins never serve HTTP, yet the dashboard, hosts, and adapters are
   bundled plugins whose job is serving routes. → D3.
3. **No atomic multi-field publication.** `VOSU_OP_PUBLISH_FIELD` carries one
   leaf and no batch boundary, so a multi-field change cannot be committed
   atomically (ADR 0002:38, `docs/architecture/overview.md:238`); a
   continuation flag is unsafe with concurrent publishers whose frames
   interleave in one ring (plugin-abi.md §7.2). → D4.
4. **Synchronous readers versus asynchronous plans.** Read plans are
   asynchronous even in-process (plugin-abi.md §3, §5;
   `include/vosu_plugin.h:409-411`), while the 21,756-line 1.x reader stack
   reads synchronously. → D5.
5. **Discovery parity.** 1.x skips `--tournament` and `debug-client-id`
   processes (`src/instance/discovery.rs:118-123`), an argv-based rule
   full-match `regex` matchers (plugin-abi.md:115) cannot express as a
   matcher — the process itself must still be examined to decide whether to
   expose it. Separately, 1.x runs Linux's broad lazer fallback only when
   the named stable pass is empty (`src/instance/discovery.rs:73-107`); this
   is not a skip at all (1.x inserts every matching candidate either way,
   `src/instance/discovery.rs:74-111`), only a classification choice between
   variants, so it needs no matcher-level gap closed, only somewhere to
   express the choice (`SESSION_IDENTIFY`'s `variant`). Tournament spectator
   clients are ordinary 1.x instances (`src/instance.rs:66-83`) selected by
   lowest PID (`src/instance.rs:702-721` + `src/runtime/worker.rs:873-896`);
   the tournament-manager aggregate
   is set only by tests (`src/instance.rs:136`, every instance starts
   `false` at `src/instance.rs:509`). → D6.
6. **Third-party namespace schemas.** External plugins have no way to hand
   core their namespace schema, so core cannot compile `x-sampling-lane`
   demand for third-party namespaces. → D7.
7. **Install paths for plugins.** 1.x services received paths through
   `GET /tools/_host/v1/context`, whose `songs_dir` is the lazer file store on
   lazer (`vosu-tools/tools/replay-renderer/src/main.rs:628-636`); 2.0 has no
   equivalent, and writers need an import path rather than a writable
   folder. → D8.
8. **Naming contract.** The shared naming contract that state-schema.md,
   catalog-v2.md, and the schemas cite is not a committed document. → A1
   commits `docs/v2/naming.md`.
9. **Crate granularity.** ADR 0019:111-114 says one crate per bundled
   plugin; the build-time goal needs the osu! plugin as five crates and core
   as four. → D9.
10. **Linux matcher semantics and the event filter.** 1.x matches the
    substrings `osu!` and `osulazer.exe` in the 16-byte kernel `comm`
    (`src/instance/events.rs:150-155`,
    `src/instance/events/bpf/process_events.bpf.c:76-92`); `process_names`
    are exact names (plugin-abi.md:108), so the osu! plugin needs path
    patterns, which would widen the filter to every exec and lose the
    benefit `AGENTS.md:130` preserves. "Overlapping" regex matchers
    (plugin-abi.md:118) are undecidable in general. → D18.
11. **osu!-specific configuration keys.** `CALCULATE_PP`, `POLL_RATE`,
    `PRECISE_DATA_POLL_RATE`, `SHOW_MP_COMMANDS`, and
    `READ_MANIA_SCROLL_SPEED` (`src/config/schema.rs:139,163,169,181,187`)
    have no owner in 2.0, and the M8 migration listed only removed keys.
    → D19.
12. **1.x user overlays that use removed APIs.** Overlays built from the 1.x
    quickstart subscribe to `/websocket/v2/subscribe`
    (`vosu-overlay-quickstart/app.js:449`), which M4a removes, while
    catalog-v2.md:903-905 calls user overlays "unaffected". → D20.
13. **Bundled plugins cannot run out of process.** A bundled manifest must
    not request `runtime = "process"` (plugin-abi.md:36), yet M5 must prove
    the osu! plugin behaves identically there. → M5 task: a
    `test-harness`-only `cdylib` build with a non-bundled test manifest.
14. **Alpha builds share state with 1.x.** Alphas would read and rewrite the
    1.x roots, and 1.x's canonical rewrite drops unknown keys
    (`src/config/tests.rs:359-395`), so going back to 1.x is undefined. → D21.
15. **The SDK signature.** `fn on_message(&mut self, …)` (plugin-abi.md:315)
    cannot accept the concurrent calls `concurrent_dispatch = true` permits,
    which the osu! plugin needs. → D22.

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
- A regular and a precise reader publishing concurrently never produce a
  snapshot that mixes their batches; an invalid entry rejects its whole
  `PUBLISH_BATCH` and commits nothing.
- A declined process goes to the next matching plugin in install order and
  is dropped for its PID generation only once none remains; a tournament
  spectator client is a visible session with its IPC id.
- `VOSU_READ_PLAN_SYNC` submitted from a dispatch thread is refused in both
  runtimes.

## 10. Adopted decisions and risks

The owner accepted the first draft's recommendations subject to an
adversarial review; every verdict of that review is adopted. Each entry
states what was adopted and why. Spec amendments land in A1.

- **D1 Branches and the 1.x consent release.** M−1 and all of M0 land on
  `main`; `v2/main` is branched after M0 and receives M1 onward through
  `land.py --base v2/main`, with `main` merged in weekly. The 1.x consent gate
  ships as **1.2.4, before M0**. Why: M0 is behavior-neutral by its exit
  criteria, so it carries no 2.0 code; landing it on `main` removes the mass
  rename from every weekly merge (a split file follows only one rename
  destination) and gives 1.x the build-time gain at once, while the consent
  release protects only installations that receive it before 2.0 ships (ADR
  0022).
- **D2 Plugin-only paths.** `x-visibility: "plugins"` is allowed only on
  `vosu.osu`'s top-level `/internal` key and rejected anywhere else at schema
  load; `/internal` carries **paths only** (per-variant fields, D8, plus
  the current chart's/active skin's folder and file names, R8); one
  projector function strips it from every API-facing document
  unconditionally — whole document, whole namespace, every subscription
  and encoding cache, session events, and debug logs — and gates
  `STATE_UPDATE` and `vosu-surface` reads by `requires`/`optional` naming
  `field:vosu.osu.internal` or a descendant of it, never by the
  whole-namespace `field:vosu.osu`. The
  streamCompanion adapter computes strains, breaks, kiai points, and timing
  points itself from `vosu-osu-calc` and the chart resource. Adapter file
  routes resolve against the osu! plugin's `ROOT_DECLARE` handles
  (plugin-abi.md:167), never against a path string from the bus. A1 amends
  state-schema.md (including the line-292 statement), api-v1.md,
  catalog-v2.md, ADR 0021, and ADR 0020's "published schema" rule
  (ADR 0020:20-21) with the carve-out, and bumps `vosu.osu` to 1.1.0. Why:
  the adapters must echo upstream path fields while API v1 must never serve
  them; the subtree is disclosure control, not confinement, because
  out-of-process plugins already have the user's filesystem access (ADR
  0019:105-107) and vosu-tools finds the folders itself
  (`vosu-tools/tools/lazer-library/src/main.rs:52-58`); hiding non-sensitive
  derived values there only mixed two purposes.
- **D3 Bundled-only HTTP surfaces.** Bundled plugins register through the C
  ABI for lifecycle, manifests, settings, and actions, and additionally use
  `vosu-surface` for routes, WebSocket feeds, and reads. A1 writes the ADR
  0019 and 0021 amendments: HTTP surfaces are bundled-only in 2.0; a future
  external HTTP opcode would be an additive minor ABI revision; the ADR lists
  the trait surface exhaustively, with no `ProcessMemory`, configuration
  writes, or credentials. `vosu-surface` reads go through the same
  subscription, demand, and `requires` path as `STATE_SUBSCRIBE`. Bridge B1's
  `vosu-osu → vosu-surface` edge is recorded as interim until M4a, and M5
  starts after M4a. Why: ADR 0019:51-53 promises one plugin interface and ADR
  0021:61 says plugins never serve HTTP, so the exception must be explicit;
  reads outside the demand path would leave `/websocket/v2/precise` without
  precise sampling; B1 cannot cross a process boundary.
- **D4 `VOSU_OP_PUBLISH_BATCH`.** A new flat opcode carries one handle, a
  count, and an array of `vosu_field_publish_header` entries plus trailing
  data, capped by `VOSU_PUBLISH_BATCH_MAX_BYTES` — an alias of the
  universal `VOSU_FRAME_MAX_BYTES` (1 MiB, F4, G6), not the narrower
  64 KiB `VOSU_READ_PLAN_MAX_BYTES`, since a batch's whole-value CBOR
  entries are not bounded the small fixed-width way a read plan's ops are —
  validated as a whole: all or
  nothing, one status, one commit per message, no pending host state.
  Receivers reject unknown `vosu_message.flags` bits with
  `VOSU_ERR_INVALID_ARGUMENT` (today only "0 unless the spec says otherwise",
  `include/vosu_plugin.h:167`). Why: a continuation flag lets a precise
  reader thread commit half of a regular batch (`src/state/worker.rs:216-285`
  runs both concurrently, and process-runtime frames interleave in one
  ring), leaves partial failure undefined since every `send` is accepted on
  its own (plugin-abi.md §5), and keeps unbounded pending state
  (`AGENTS.md:112`); one batch message also cuts per-leaf `send` overhead.
- **D5 Opt-in synchronous reads.** A `VOSU_READ_PLAN_SYNC` flag on
  `READ_PLAN_SUBMIT` guarantees, in both runtimes, that the result is
  dispatched on the submitting thread before `send` returns (in-process
  directly; in `vosu plugin-host` a dedicated ring-reader thread hands it to
  the blocked submitter), under a deadline that returns `VOSU_ERR_TIMED_OUT`.
  The SDK routes flagged results to a per-plan slot before user code sees
  them; `PlanMemory` refuses to run on a dispatch thread and ships only
  in the `publish = false` `vosu-plugin-internal` crate (AF.9, S0; see D11),
  whose dispatch glue performs that routing. The osu! plugin is measured
  under the process runtime in M5. Why: an optional "host may" behavior is
  one plugins cannot rely on, deadlocks a plugin holding a lock across `send`,
  contradicts the header's "arrives later" contract
  (`include/vosu_plugin.h:409-411`), and differs between runtimes; one plan
  per read is the pattern brokered-reads.md measured 16–25x slower
  (brokered-reads.md:103,107) and must not spread to third parties.
- **D6 Discovery parity.** `VOSU_OP_SESSION_DECLINE` (plugin → host, CBOR
  `{"handle"}`) is adopted in A1, before M1: a plugin declines a matched
  process it never wants to expose as a session at all, valid only before
  its own `SESSION_IDENTIFY` for the handle; core detaches it from that
  plugin and, if another enabled plugin's matcher also accepted the
  process, offers it to that plugin next in install order before dropping
  it for that PID generation. The osu! plugin declines `debug-client-id`
  processes always, and `--tournament` processes until the stable
  tournament-manager aggregate task (below) lands (an argv-based skip);
  1.x's Linux conditional fallback (the broad lazer pass that only runs when the named
  stable pass found nothing) is not a decline — 1.x never skips those
  processes, only chooses which variant classifies them
  (`src/instance/discovery.rs:74-111` inserts every matching candidate) —
  so the osu! plugin always identifies a matched Linux process and
  expresses the fallback choice as `SESSION_IDENTIFY`'s `variant`. Tournament
  spectator clients are identified as normal sessions, with their IPC id in
  `vosu.osu.session.ipcId`, as 1.x attaches them. `vosu.osu.tourney` is
  published exactly where 1.x builds it (`src/api/v2/tourney.rs`): `null`
  on the lazer multi-spectating path (`ipcState == 11`) before the first
  lobby read lands, matching 1.x's own "key omitted entirely until then"
  behavior (`:28-35`; the adapters drop the key when this value is `null`);
  populated `clients[]` once that read lands; 1.x's default empty object
  everywhere else. The stable tournament-manager aggregate (1.x's
  `is_tournament_manager()` branch, production-dead per the "Why" below)
  must exist before the 2.0 release — it is an explicit M4a-or-later task:
  attach the `--tournament` process as the manager instead of declining it
  — it becomes an identified `stable` session (`SESSION_IDENTIFY`
  `variant: "stable"`) that sets `focus_priority: true` (`docs/v2/plugin-abi.md`
  §6.2), visible in `/api/v1/sessions` and, while it exists,
  always the focused session regardless of OS foreground or recency — a
  deliberate addition to `docs/v2/api-v1.md`'s Focus rule, matching 1.x's
  `select()`, which prefers the manager unconditionally
  (`src/instance.rs:702-721`, `src/runtime/worker.rs:873-896`; if two
  `--tournament` processes somehow attach at once, 2.0 keeps whichever is
  identified first rather than 1.x's lowest-PID choice, an edge of an edge) — and
  publish the resulting aggregate under `vosu.osu.tourney` to **every**
  currently attached stable session, not only the manager's own, exactly
  as 1.x's stable branch builds it per client. Until that task lands,
  `--tournament` stays
  declined exactly as D6 originally adopted, and the osu! plugin
  takes 1.x's "no manager found" branch unconditionally on stable, matching
  1.x's own production behavior; the M2 and M4a exits exempt exactly the
  `stable-tournament` oracle case's `tourney` members (`v1`, `v2`,
  `v2-precise`), recorded as a `temporary`-class divergence in
  `tests/fixtures/upstream/DIVERGENCES.md` (G1's per-entry class scheme), and
  the M8 day-0 gate fails closed on a remaining `temporary` entry, or on
  any entry whose class is missing or is none of the four defined classes;
  the file's permanent `security`/`contract`/`licensing` entries, correctly
  recorded, never fail it, so this entry must be removed — the aggregate
  task must land — before day-0. "Never identify" is never used as a
  deliberate
  state. Why: 1.x
  production never marks a tournament manager (`src/instance.rs:136` has
  only test callers; `src/instance.rs:509`) and exposes spectators as
  selectable instances (`src/instance.rs:66-83`, `:702-721`), so hiding them
  would leave API v1 with no session during a tournament; the 1.x rules
  depend on argv and cannot be written as full-match regex matchers
  (plugin-abi.md:115) for the argv-based skip, and the Linux fallback is a
  classification choice 1.x never expresses as skipping a process at all;
  core cannot tell an unidentified session from a plugin bug.
- **D7 Bounded namespace schemas.** `[[provides]]` entries naming a whole
  `field:` namespace carry `schema = "<path>"`. Core consumes only a bounded
  profile: at most 256 KiB, depth at most 32, `$ref` only within the schema
  or to core `common-defs` by fixed id, lanes only; core never validates
  publishes against third-party schemas at runtime; the schema's namespace
  must equal the `provides` id. `catalog-v2.json` mirrors the schema
  `version` and the extracted lane table so the resolver plans without
  downloading. Core's demand compiler is a hand-written lane walker. Why:
  the trust surface of an arbitrary schema (size, references, patterns)
  was unspecified, and runtime `jsonschema` would contradict D13.
- **D8 Install paths and imports.** `vosu.osu/internal/paths` has per-variant
  fields — stable `{game, songs, skins, replays}`, lazer `{data, files}` —
  each `null` when unknown or not applicable to the variant. The install plan
  gains a field flagging plugins that require `internal.paths`, labelled
  disclosure, not a sandbox. Writers use a new `vosu.osu.beatmap.import`
  action (M3) instead of a writable path. Why: install-plan `adds` entries
  show no `requires` (catalog-v2.md:648-702), 1.x's `songs_dir` already means
  two things (`vosu-tools/tools/replay-renderer/src/main.rs:628-636`), and
  writing into lazer's store directly is unsafe.
- **D9 Multi-crate plugins and core.** The osu! plugin spans five crates and
  core four; ADR 0019:111-114 is amended in A1. `vosu-adapter-support` and
  `vosu-osu-model` are justified by dependency direction. The build budget
  covers prepush wall-clock, native Windows Clippy/test/doctest, clean and CI
  builds, and feature-unification rebuilds, not only incremental edits
  (section 8). Why: incremental edits alone would hide costs the split can
  add elsewhere.
- **D10 Names.** Crates `vosu-adapter-upstream`, `vosu-adapter-gosumemory`,
  `vosu-adapter-streamcompanion`; plugin ids `vosu.adapter.upstream`,
  `vosu.adapter.gosumemory`, `vosu.adapter.streamcompanion`, `vosu.osu`,
  `vosu.dashboard`, `vosu.overlayhost`, `vosu.toolhost`; display names
  describe the routes served. A1 amends ADR 0021:119-120. Why: ids that
  extend a registered id are rejected (catalog-v2.md:120-131), so bundled
  `vosu.tools`/`vosu.overlays` would block the ecosystem's natural
  `vosu.tools.<name>` ids; upstream-brand names would need allowlist entries
  that ADR 0021 required but `AGENTS.md:5-6` discourages.
- **D11 SDK distribution.** `vosu-plugin-api` is published to crates.io as
  `1.0.0-alpha.N` prereleases and `1.0.0` at release, versioned by SDK semver
  whose major equals the ABI major, not by the vosu release; the bundled
  registration glue and `PlanMemory` stay out of the published surface
  (unpublished features); the C header ships as a release asset. Why: a git
  dependency clones this whole repository (assets, `vosu-pp`, fixtures) into
  every plugin build.

  **AF.9/AF.10 amendment (approved 2026-11-24).** Cargo has no unpublished
  features: anything in a published crate's source, whatever its feature
  gate, comment or `#[doc(hidden)]`, ships to crates.io. S0 therefore moves
  `PlanMemory`, the SDK-side synchronous-read interception and the bundled
  macro arms and registration glue, unchanged in behavior, into the
  `publish = false` crate `crates/vosu-plugin-internal`, migrates every
  internal caller and leaves no re-export. D5's reentrant delivery before
  `send` returns, serialized and concurrent dispatch, and zero-allocation
  scalar reads keep their existing tests at the new owner. The published
  SDK exposes only external `Plugin`/`ConcurrentPlugin` exports, ABI types
  and codecs, and validators, plus the `vosu-package-check` example
  ([catalog v2](catalog-v2.md#package-checker-af10)); its package needs no
  workspace-relative file and no path-only dev-dependency such as
  `vosu-proptest`, and no source-stripping release variant exists. A version
  bump is never a publication: each crates.io upload is a separate owner
  approval (AF.1).

  **F S0 implementation (2026-11-25).** The dispatch seam is the SDK's own
  generic glue: `vosu-plugin-internal`'s `vtable_for`/`vtable_for_concurrent`
  take the SDK's `create`/`destroy` unchanged and wrap its `dispatch`, so a
  synchronous `READ_PLAN_RESULT` is claimed (header read first, no decode for
  `read_flat`) before the SDK glue decodes it or takes the serialized lock,
  and every other message runs the unmodified SDK path with the
  dispatch-thread mark set. The SDK keeps only codecs that external plugins
  can use for allocation-free async reads and publishes — `StageRef`,
  `encode_read_plan_submit_into`, `HostServices::send_flat`, now
  unconditional — and drops `HostServices::submit_read_plan_sync`
  (`PlanMemory` sends its own `sync` submission). `read_plan_profile` moved
  with `PlanMemory`. `scripts/sdk-package.py verify` checks the packaged
  `.crate` outside the workspace, including an isolated registry-requirement
  consumer and probes proving the removed private surface does not resolve.
  Hosted CI's validation job and `prepush.py` run it whenever
  `scripts/ci-changes.py`'s `sdk` flag finds an SDK-relevant path.
- **D12 Bridge path.** The 2.0 bridge is served at
  `/tools/_host/v2/bridge.js`; the v1 path is removed in M7. Why: every
  official tool is rebuilt and 1.x user tools are disabled by the migration
  (catalog-v2.md:897-902).
- **D13 Dependencies.** `ciborium` and `regex` become runtime dependencies
  with full review and `THIRD_PARTY_NOTICES.md` entries (today they are in
  `Cargo.lock` only through the dev-dependency `criterion`, `Cargo.toml:130`);
  `libloading` is added; HMAC comes from the crypto stack already in the tree
  (`aws-lc-rs` 1.18.2 or `ring` 0.17.14, `Cargo.lock:147-148,2193-2194`)
  instead of a new `hmac` crate; `linkme` is dropped for an explicit
  `BUNDLED` array in the root crate; `jsonschema` is dev-only with
  `default-features = false`. Why: a linker-collected slice still needs every
  crate referenced to be linked, so it gives no omission guarantee that a
  compile-time list does not give more simply.
- **D14 Owner-controlled captures** (amended by A4b.1, 2026-11-24). A
  one-pair pilot capture runs as soon as the M4b tooling lands; the full set
  (Windows stable, Windows lazer, Linux lazer) runs in one later session;
  a pin move before M8 triggers
  a full recapture. Captures are owner-controlled, not owner-hand-operated:
  the owner authorizes each session in writing (commit and pin, client
  build and install, scenario, account/data/media policy, evidence path, GUI
  operator scope, review route) and keeps control of scope and publication;
  an agent operates the isolated VM test installs within that authorization,
  and main reviews every candidate fixture before commit (A4b.8). VM
  lifecycle approval is not capture authorization. Why: a single
  end-of-project session would surface adapter mismatches only at the
  release gate, and agent operation keeps the pilot from waiting on the
  owner's hands while the owner keeps control of scope and publication.
- **D15 Forge verification.** CI uses a local Forgejo or Gitea container
  behind an internal certificate authority in `network_integration`, making
  the private-network exception and the OS trust-store path repeatable (ADR
  0023:52-60); the owner runs a one-time check against GitHub, GitLab,
  Codeberg, and a Gitea or Forgejo instance with tokens never stored in the
  repository. Why: real forges cannot run in CI, and ADR 0023 requires
  verification against each.
- **D16 vosulay hosting.** vosulay ships a small official plugin, running
  in-process, that supervises vosulay's existing host executable as a child:
  job object on Windows, `PR_SET_PDEATHSIG` on Linux. Spike S1 validates it
  on Linux and Windows before M5. Why: under `vosu plugin-host`, the
  `no_new_privs` bit (plugin-abi.md §7.3) disables the setuid `chrome-sandbox`
  that vosulay's Linux packages install (`vosulay/docs/security.md:58-60`). The
  plugin host's seccomp user-namespace denial (A5.1) disables Chromium's
  namespace sandbox, which S1 observed in use. CEF on
  Windows must start through vosulay's sandbox bootstrap
  (`vosulay/docs/security.md:265`, `vosulay/docs/architecture.md:24`), CEF
  cannot be reinitialized within one process (`vosulay/docs/architecture.md:7`)
  and installs its own signal handlers (`vosulay/docs/architecture.md:35`);
  CEF renderers already run out of process, and the standalone mode for
  upstream-compatible backends keeps working unchanged. **S1 outcome
  (2026-11-25): D16 stands** ([S1 outcome](#s1-outcome-2026-11-25)). On
  Windows it is conditional on a Medium, unelevated vosulay host launched with
  a verified same-session token, with no High fallback. Session and focus
  reach the attached child through its API-v1 subscription (AF.8), not stdin.

  **AF.7/AF.8 amendment (approved 2026-11-24).** The supervisor is the
  external official plugin `vosu.vosulay` from `18-lab-dev/vosulay`, never in
  `BUNDLED`; no id extending it is registered. It discovers and validates a
  separately installed vosulay runtime (vosulay's own installer, archives,
  Winget, Scoop, AUR or distribution packages) and shows an actionable
  missing-runtime state; it never installs the runtime, elevates, creates a
  setuid helper or falls back to `--no-sandbox` or job breakaway. On
  Windows it creates the packaged CEF bootstrap suspended with a Medium,
  unrestricted token even from an elevated daemon, assigns it to its own
  strict kill-on-close job, then resumes it. On Linux the child has zero
  permitted, effective, inheritable and ambient capabilities, does not
  inherit `no_new_privs` (unlike a plugin-host child under A5.1), re-arms
  `PR_SET_PDEATHSIG` after exec and checks its expected parent; no
  CAP_SETPCAP is added. Start succeeds only after the child reports CEF and
  host readiness, not on spawn. The attached child subscribes to API v1
  itself (AF.8); private IPC carries lifecycle, configuration and concrete
  editor operations, never a generic message passthrough. A8.6 stands: own
  CI runs the production supervisor logic under the process runtime against
  a separately identified non-CEF fixture child, and real in-process CEF
  sandbox and parent-death smokes run on Linux and Windows; neither
  substitutes for the other.
- **D17 Performance gates.** Benches: M1 runs the reader benches and the
  allocation invariant (`scalar_readers_allocate_nothing`) over `PlanMemory`
  with the in-process executor, since `raw_snapshot_reads`
  (`benches/hot_paths.rs:236-264`) and the invariant
  (`tests/performance_invariants.rs:220-259`) read through `ProcessHandle`
  directly; M2 adds an end-to-end bench measuring `end_to_end/bus` (osu!
  projection → publication batch → bus commit → API-projected JSON + one
  `STATE_UPDATE`) against `end_to_end/legacy` (1.x `v2` + precise
  encoders), since in M2 `/json/v2` itself still comes from bridge B1 and
  API v1 frames are M3's (R30); M4a re-runs the bench with the adapter
  path, then comparing against the same 1.x serializers end to end,
  because `publication` (`benches/hot_paths.rs:442-498`)
  measures only the 1.x hub; M1, M2, and M4a record daemon CPU per tick and
  read-to-frame latency with `examples/live_reader_bench.rs`. Bar: no
  regression above 10% of the median per milestone **and** cumulatively
  against the pre-M0 commit, judged by non-overlapping Criterion 95%
  confidence intervals on pinned CPUs; Linux brokered reads within 2x of
  brokered-reads.md's prototype; the Windows brokered bar is set from M5's
  measurement. A5.6 fixes both brokered metrics before measurement
  ([M5 D17 contract](../testing/benchmarks.md#m5-d17-contract)):
  `production_sync` p50 is at most 2× the same-run prototype p50 for each
  plan shape. Why: the first draft's benches bypassed the code under
  change, and per-milestone bounds alone compound to about 33%.
- **D18 Matcher hints and overlap.** `[match]` gains optional
  `comm_hints` (substrings of at most 15 bytes) used only to fill the
  bounded in-kernel and CN_PROC pre-filter, with full matching in
  userspace, and a second optional field, Linux-only
  `linux_name_contains` (A1b/DB, additive under abi §1/§3's growth model —
  a new manifest field, no existing field's meaning changes, no ABI major
  bump): a process whose `comm` *contains* an entry matches, 1.x's Linux
  substring rule; each entry also feeds the pre-filter like a `comm_hints`
  entry, so declaring it there instead never widens the filter. Windows is
  unchanged, keeping exact, case-insensitive `process_names`; `comm_hints`
  itself never matches a process on its own, on either platform. If any
  enabled plugin declares path or command-line patterns without hints, the
  filter widens to all execs and the dashboard says which plugin caused it.
  Overlap is checked statically on `process_names`, `comm_hints`, and
  `linux_name_contains`: two plugins overlap when a `process_names` entry
  is shared, when a `comm_hints`/`linux_name_contains` entry of one is
  equal to or a substring of a `process_names` entry of the other, or
  when a `comm_hints`/`linux_name_contains` entry of one is equal to or a
  substring of a `comm_hints`/`linux_name_contains` entry of the other —
  exactly the pre-A1b D18 hint-overlap rule, with `linux_name_contains`
  treated like `comm_hints` for this static check, not a pairwise
  "could both match some 15-byte `comm`" merged-length rule (an earlier
  revision of this decision tried that and made almost any two short
  entries statically conflict, e.g. `linux_name_contains = ["mygame"]`
  against `vosu.osu`'s `"osu!"`, since `"osu!mygame"` is 15 bytes or
  fewer — rejecting most real plugin pairs outright); at runtime, any
  process two enabled plugins' matchers both accept — which that static
  check cannot always catch, including two hints with no containment
  that nonetheless both match one real `comm` (e.g. `osu!` and `lazer`
  both matching `osu!lazer`) — goes to whichever plugin was installed
  earlier, and core logs once. Why: `process_names` alone is exact and
  cannot express 1.x's Linux substring rules (gap 10); a separate
  `linux_name_contains` field, rather than giving `comm_hints` a second
  meaning, keeps every existing plugin's `comm_hints` declaration
  unchanged and avoids widening the filter, which would discard the
  measured benefit `AGENTS.md:130` requires to keep; static overlap
  stays a cheap, conservative early refusal, and regex overlap cannot
  be decided statically either way.
- **D19 osu!-specific configuration.** `CALCULATE_PP`, `POLL_RATE`,
  `PRECISE_DATA_POLL_RATE`, `SHOW_MP_COMMANDS`, and `READ_MANIA_SCROLL_SPEED`
  become `vosu.osu` settings in its `settings_schema` (M1); bridge B2 serves
  them from `vosu.toml` until the M8 migration moves the values and removes
  the keys. Why: they configure only the osu! plugin, and core configuration
  should hold only core concerns.
- **D20 1.x overlays using removed APIs.** User overlays stay enabled, as
  catalog-v2.md specifies, but requests to removed routes and fields are
  attributed to the requesting overlay (M4a) and the dashboard shows a
  notice with a link to the migration guide (M7); A1 amends
  catalog-v2.md:903-905 from "unaffected" to "left enabled; flagged when
  they use removed APIs"; the quickstart documents the port. Why: overlays
  built from the 1.x quickstart (`vosu-overlay-quickstart/app.js:449`)
  stop receiving data after M4a, and silent breakage is worse than a notice.
- **D21 Alpha data isolation.** Builds whose version has a semver
  prerelease part keep config, data, and cache in a prerelease subdirectory
  of each vosu root and never write the 1.x files; on first start they import
  1.x settings read-only. Release 2.0 uses the normal roots and the M8
  migration. Why: 1.x's canonical rewrite drops unknown keys
  (`src/config/tests.rs:359-395`), so shared roots would make returning to 1.x
  lossy and untested.
- **D22 SDK dispatch shape.** The SDK has `Plugin` (`fn on_message(&mut self,
  …)`, the SDK serializes calls behind a lock, safe whatever the manifest
  says) and `ConcurrentPlugin` (`fn on_message(&self, …)`, `Send + Sync`)
  for `concurrent_dispatch = true`; A1 amends the plugin-abi.md §9 sketch.
  Why: `&mut self` (plugin-abi.md:315) cannot accept the concurrent calls the
  osu! plugin needs.

Risks that need no further decision:

- **Integration drift after M4a:** 1.x fixes in code 2.0 has rewritten
  (`src/api/`) are ported by hand; the weekly merge surfaces them early.
- **Ecosystem schedule:** ADR 0022 ties the release date to four
  repositories; the trigger in section 7 is as early as the contracts allow,
  and alphas with isolated data let each repository integrate continuously.
- **Hidden 1.x quirks:** the adapters rely on M4b to catch behavior the
  source oracle and `server_compat` do not cover; the pilot narrows that
  window.
- **Windows transport:** unmeasured until M5. A5.6 keeps the unnamed
  section and events unless a pipe wins under the frozen tie rule and gains an
  approved secure handoff.
- **Linux capability drop:** must run before any thread exists, so
  `plugin-host` dispatch happens first in `main`, before the Tokio runtime is
  built (`src/main.rs:3-16`). A5.1(b) replaces the bounding-set drop with
  `no_new_privs`, the user-namespace seccomp denial and a fail-closed readback.
- **vosulay:** S1 passed on both OSes (D16 stands). The remaining
  setuid-helper and elevated-launch evidence belongs to the vosulay ecosystem
  owner.

## Critical files

- Contracts: [plugin ABI](plugin-abi.md), [`include/vosu_plugin.h`](../../include/vosu_plugin.h),
  [state schema](state-schema.md), [`schemas/`](schemas/), [API v1](api-v1.md),
  [catalog v2](catalog-v2.md), [brokered reads](brokered-reads.md),
  [ecosystem release matrix](ecosystem-release-matrix.md), ADRs 0019–0023.
- Structure and checks: `Cargo.toml`, `build.rs`, `src/lib.rs`, `src/main.rs`,
  [`scripts/module-boundaries/`](../../scripts/module-boundaries/) (per crate since M0),
  [`scripts/crate-boundaries.txt`](../../scripts/crate-boundaries.txt), `scripts/check-crate-boundaries.py`,
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

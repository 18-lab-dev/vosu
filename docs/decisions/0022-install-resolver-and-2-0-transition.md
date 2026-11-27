# Resolve plugin dependencies automatically and ship 2.0 with day-0 ecosystem support

- Status: Accepted (vosu 2.0)
- Date: 2026-11-21

## Context

In 2.0, overlays and tools depend on plugins for their data and actions
([ADR 0019](0019-plugin-runtime-and-trust.md)). Users must not have to find
those plugins. Installing anything should take one action.

The transition from 1.x has three hazards found in the current code:

- `parse_latest_release` (`src/updater/release.rs`) installs any newer stable
  release with a matching asset. A 1.x install would update itself to 2.0
  without notice, and overlays that rely on 1.x-only additions would stop
  working ([ADR 0021](0021-api-v1-and-upstream-adapters.md)).
- 1.x rejects catalogs whose `catalog_version` is not 1
  (`src/tools/catalog.rs`) and resolves `releases/latest/download/catalog.json`.
  If an official repository's latest release published only 2.0 packages, 1.x
  catalogs would fail.
- Every first-party tool, overlay, the tool SDK and web bridge, vosulay, and the
  overlay quickstart use `/websocket/v2/subscribe` or 1.x-only fields, which the
  2.0 adapters do not serve.

## Decision

### Requirements and resolution

Overlay, tool, and plugin manifests declare required and optional
**capabilities**, not plugin identifiers: data fields and namespaces
([ADR 0020](0020-game-agnostic-data-model.md)), resource kinds, and actions.
Plugins declare what they provide.

- The resolver maps requirements to providers across enabled catalog sources.
  When several plugins provide a capability, the official one wins; the user is
  not asked to choose.
- Install installs the item and every missing required provider in one
  confirmed action, listing what it will add and which items will run native
  code. Untrusted native plugins still need approval.
- Plugins installed only to satisfy a dependency are removed when nothing
  requires them.
- Game plugins declare process matchers. When a running client matches a
  catalog plugin that is not installed, the dashboard offers to add support for
  that game.

**A6b.5–A6b.8 amendment (2026-11-24):** ranked bounded backtracking selects
the first complete satisfying graph with one assignment per plugin, including
kept tool/overlay consumers. Optional edges never initiate installation.
Plans pin exact artifacts/policy generations (64 pending, 10-minute TTL,
single use, one running operation); confirmation cannot recompute unseen work.
Prepare all packages and journal same-filesystem publication/rollback before
loading any committed graph; recover before discovery. Requested promotion
and recursive dependency-only removal preserve settings/data; explicit removal
of a still-required provider is refused with named dependents. Package limit
defaults to 512 MiB, integer `1..=4096`. The full
[resolver contract](../v2/catalog-v2.md#resolver-algorithm) is normative.

### Catalog format

2.0 reads `catalog-v2.json`, a separate release asset whose entries declare the
host versions they support. Official repositories publish both `catalog.json`
for 1.x and `catalog-v2.json` for 2.0 in every release until 1.x support ends.

### Transition from 1.x

- The next 1.x release requires explicit consent before an update that changes
  the major version. It protects only installations that receive it before 2.0
  is published.
- On first start, 2.0 migrates 1.x state once and idempotently: removed
  configuration keys, catalog sources, and installed official tools, which move
  to their 2.0 releases. A 1.x user tool with a service is kept but disabled and
  marked as needing an update from its author.

### Day-0 release gate

2.0 is published only when every official source (vosu-tools, vosulay, and
vosu-overlays) has a published release with a `catalog-v2.json`, and the overlay
quickstart has a 2.0 release. A release workflow check enforces this. Ecosystem
work starts after the plugin ABI, manifest, catalog, and API v1 contracts are
frozen and the SDK and web bridge exist, with one owner per repository.

## Consequences

- Users install overlays and tools with one action; dependency plugins never
  appear as a separate search.
- Official repositories publish two catalogs for the length of the 1.x support
  window.
- 2.0's release date depends on ecosystem readiness, by design.
- 1.x installations that skip the consent release still update to 2.0
  automatically.

## Alternatives considered

- **Requirements by plugin identifier:** rejected; it ties consumers to one
  implementation and prevents a replacement provider.
- **One catalog file with a new version:** rejected; 1.x rejects it.
- **Hiding 2.0 from the 1.x updater by renaming release assets:** rejected; it
  strands 1.x users with no upgrade path instead of asking them.

## Amendments

**2026-11-24** (AF, ecosystem fan-out contract freeze; full record in
[`docs/v2/ecosystem-release-matrix.md`](../v2/ecosystem-release-matrix.md)):

- **Trigger.** Ecosystem implementation starts only when M3 and M6a have
  landed on `v2/main` and the owner has published `v2.0.0-alpha.1` as a
  GitHub prerelease with `make_latest=false`; external plugin builds also
  need the owner-published `vosu-plugin-api` `1.0.0-alpha.N` on crates.io.
  Every push, tag, upload, signature and publication is a separate owner
  approval with an exact version and target (AF.1).
- **Identities and transfer (AF.2).** A 1.x service tool becomes a plugin
  plus, where it has a UI, a new same-kind web tool id that `replaces` the
  1.x id. Because `replaces` cannot cross kinds, "installed official tools
  move to their 2.0 releases" above is carried out for durable data and
  native or secret settings by a closed, one-time map that M8's migration
  executes; plugin code never reads 1.x tool roots.
- **Two catalogs (AF.4).** Each official release's `catalog.json` is a
  frozen copy of that repository's last published 1.x catalog and points
  only at immutable, independently packaged 1.x assets. When 1.x support
  ends is decided later.
- **Day-0 gate (AF.5).** The official repositories and the quickstart are
  released publicly when F ships, so the unauthenticated latest-release
  check stays unchanged; prereleases never satisfy it.

**2026-11-24** (A8, M8 migration and release gate freeze; full record in
[`docs/v2/implementation-plan.md`](../v2/implementation-plan.md#a8-approved-contract-freeze-2026-11-24)
and [`docs/v2/catalog-v2.md`](../v2/catalog-v2.md#first-start-migration-from-1x)):

- **Offline local conversion and pending official intents (A8.2, A8.3).**
  First-start migration finishes durable local conversion offline using one
  normal-root versioned journal (`<config>/migration-v2-journal.json`),
  publishing destination plugin settings before deleting moved keys and never
  writing `local_conversion_complete` before local conversion is durable.
  Official replacement lookups persist as independent pending intents resumed
  explicitly through M6b plans; queueing never marks an item installed, zero
  claims leave the 1.x install unmanaged, multiple same-kind/source claims
  surface a visible conflict, catalog errors remain retryable without
  cross-source fallback, and native adds still require explicit artifact and
  runtime approval.
- **Candidate-bound release gate (A8.4).** Stable `2.0.0` publication (not
  ecosystem-bootstrapping alphas) requires both a fresh day-0 HTTP/ledger check
  and a passing candidate-bound JSON evidence manifest, enforced in `release.yml`
  and rechecked in local final publication (`scripts/sign-release.py`).
- **Stable host ranges and SDK independence (A8.5).** Stable `2.0.0` bundled
  and official packages default to `host = ">=2.0.0, <3.0.0"` unless the owner
  explicitly requests an alpha-compatible range. `vosu-plugin-api` `1.0.0`
  releases on its own independent version and publication gate.
- **Supervisor CI split (A8.6).** `vosu.vosulay` satisfies the official
  process-runtime smoke requirement via an own-CI `runtime = "process"` smoke
  against a separately identified non-CEF child fixture plus a separate native
  Linux/Windows `in-process` production CEF sandbox, Medium-token, and
  parent-death smoke; neither substitutes for the other.

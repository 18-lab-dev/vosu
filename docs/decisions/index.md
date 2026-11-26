---
type: Documentation Index
title: "Decisions"
description: "Files and subdirectories in Decisions."
---

# Files

- [One daemon owns deterministic multi-instance lifecycle](0001-single-daemon-multi-instance-lifecycle.md) - Accepted decision for one daemon to own deterministic discovery, generations, selection, and publication across multiple osu! processes.
- [Publish immutable, latest-only API snapshots](0002-immutable-latest-only-snapshots.md) - Accepted decision to reduce state into immutable atomic snapshots and coalesce pending publications to the latest state per process.
- [Keep process-memory access behind platform boundary](0003-process-memory-platform-boundary.md) - Accepted decision to isolate native process handles, reads, discovery, and target architecture behind Linux and Windows backends.
- [Keep calculation contracts and engines separate](0004-separate-calculation-contracts.md) - Accepted decision to preserve distinct live-state and legacy endpoint calculation contracts and compatibility semantics.
- [Serve files through root-confined retained handles](0005-root-confined-file-serving.md) - Accepted decision to serve attacker-selected paths through validated relative children and retained root-confined filesystem handles.
- [Perform self-updates as durable transactions](0006-transactional-self-update.md) - Accepted decision to validate, record, recover, and commit self-updates through durable single-owner transactions.
- [Adopt vosu as the sole product identity](0007-vosu-clean-cutover.md) - Accepted decision for a clean vosu identity cutover with one-time migration of supported legacy state.
- [Keep in-game overlays outside vosu](0008-external-in-game-overlays.md) - Accepted decision to keep overlay rendering and UI ownership outside the native vosu telemetry daemon.
- [Explicit counter search source and no local-overlay guide](0009-dashboard-search-and-local-overlay-guide.md) - Accepted decision to remove the /local-overlays page and select counter search results with an explicit source parameter instead of the Referer header.
- [Ship Linux distribution packages alongside portable archives](0010-linux-distribution-packages.md) - Accepted decision for distro packages, capability hooks, externally managed updates, AUR rendering and optional desktop integration.
- [Make live parity an optional diagnostic](0011-live-parity-optional.md) - Accepted decision that live captures against upstream are an optional maintainer diagnostic, not a merge, push, or release gate.
- [Simplify provenance checks to asset digests and the release LICENSE](0012-simplify-provenance-checks.md) - Accepted decision to keep the asset SHA-256 manifest and LICENSE appendix check while dropping duplicate hashes, notice cross-checks, the dependency-license inventory and the provenance script.
- [Keep installed counters on the dashboard origin](0013-counters-share-dashboard-origin.md) - Accepted decision to keep counter URLs, storage, and settings links compatible and disclose that installed counters share dashboard management authority instead of isolating them.
- [Overlay settings profiles](0014-overlay-settings-profiles.md) - Default profiles and per-instance command identities without moving legacy values.
- [User-managed API keys](0015-user-managed-api-keys.md) - Persistent scoped credentials alongside per-start file tokens, with immediate revocation and peer-policy enforcement.
- [Separate the dashboard origin without moving overlays](0016-separate-dashboard-origin.md) - Supersedes ADR 0013's shared-origin boundary; independent listener, dashboard CSRF, unchanged overlay storage, and recoverable network Apply.
- [Host tools on the overlay listener under a reserved `/tools` prefix](0017-tool-host.md) - Dashboard-minted UI sessions and overlay-origin hosting. In 2.0, tools are web-only and native work is a plugin action.
- [Enforce repository rules with checks, not prose](0018-enforce-rules-with-checks.md) - Accepted decision to turn agent-facing rules into lints, ratchet scripts, a recorded local gate, a required CI check on main, property tests, and a nightly canary.
- [Split vosu 2.0 into a minimal core and plugins](0019-plugin-runtime-and-trust.md) - Accepted 2.0 decision: vosu-core, bundled plugins compiled in, a message-based C ABI, manifest-requested and trust-granted in-process or plugin-host execution, and web-only tools.
- [Publish game state as owned namespaces with a best-effort common layer](0020-game-agnostic-data-model.md) - Accepted 2.0 decision: required `vosu.client`, declared best-effort common fields, plugin-owned namespaces, and sessions across games.
- [Serve vosu API v1 and keep upstream APIs as exact adapters](0021-api-v1-and-upstream-adapters.md) - Accepted 2.0 decision: `/api/v1` with filtered subscriptions and published resource references; bug-for-bug compatibility adapters; `/api/ingame` removed from 2.0 (2026-11-25 amendment), with vosulay as the in-game overlay path.
- [Resolve plugin dependencies automatically and ship 2.0 with day-0 ecosystem support](0022-install-resolver-and-2-0-transition.md) - Accepted 2.0 decision: capability-based one-click installs, `catalog-v2.json`, a 1.x major-update consent gate, migration, and a day-0 release gate.
- [Support private and self-hosted catalog sources](0023-authenticated-catalog-sources.md) - Accepted 2.0 decision: any-forge sources, scoped write-only credentials, a per-host private-network exception, and user-trusted sources.
- [Run graphical launches without a console and inspect logs from files](0024-background-daemon-console.md) - One console-subsystem executable preserves CLI semantics; the tray opens a file-backed live log viewer.

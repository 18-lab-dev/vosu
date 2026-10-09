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

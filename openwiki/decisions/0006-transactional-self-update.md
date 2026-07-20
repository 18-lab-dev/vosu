---
type: Architecture Decision Record
title: Perform self-updates as durable transactions
description: Accepted decision to validate, record, recover, and commit self-updates through durable single-owner transactions.
tags: [architecture, decision, adr, updater, reliability]
---

# ADR 0006: Perform self-updates as durable transactions

## Status

Accepted

## Context

The running executable cannot safely replace itself as an unrecorded sequence of download, extraction, rename, and restart operations. Downloads and archives are untrusted inputs, crashes can occur between filesystem operations, and a helper process must not claim an update transaction created by another process.

## Decision

Represent an update as a durable, create-new transaction record with explicit phases, random nonce and token, owner/helper process identities, expected paths, and old/archive/new digests. Sync records and parent directories at durability boundaries; validate every loaded record path against the transaction root and quarantine inconsistent or unrecoverable state.

Download atomically into transaction work storage, verify the release digest, require the archive to contain only the expected root executable and `LICENSE`, reject links/directories/duplicates and expansion beyond the configured limit, and extract to staging. Transfer installation to a copied helper only after persisting its exact identity and receiving a token-bound durable acknowledgement. Install through platform atomic replacement with backup/recovery rather than overwriting the running binary in place.

The transaction and recovery rules are in [`updater/transaction.rs`](../../src/updater/transaction.rs), input verification and handoff in [`updater/prepare.rs`](../../src/updater/prepare.rs), and native replacement operations in [`updater/platform.rs`](../../src/updater/platform.rs).

## Consequences

- Interrupted updates have a recorded phase and owned artifacts that can be cleaned up, recovered, or quarantined.
- Digest and archive-shape checks occur before staged code is handed off for installation.
- Tokens, exact process identities, create-new files, and validated paths prevent accidental transaction takeover or path redirection.
- The updater carries more states, disk synchronization, helper coordination, and platform-specific replacement logic than an in-place downloader.
- A quarantined transaction intentionally blocks a new update until diagnostics or recovery resolves it.

## Alternatives considered

- **Overwrite the running executable directly:** rejected because interruption and platform executable-locking behavior would leave no durable recovery protocol.
- **Extract an arbitrary release archive into the program directory:** rejected because archive paths, links, duplicates, and expansion would cross the installation boundary.
- **Keep transaction state only in memory:** rejected because the helper and crash recovery must validate ownership and phase after the original process exits.
- **Trust a helper PID alone:** rejected because PID reuse cannot establish the exact process identity or transaction token.

## Related documentation

- [Architecture](../architecture/overview.md)
- [Benchmarks](../testing/benchmarks.md) — update correctness is defined by transaction invariants, not benchmark timing.

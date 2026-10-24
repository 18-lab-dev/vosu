---
type: Architecture Decision Record
title: Adopt vosu as the sole product identity
description: Accepted decision for a clean vosu identity cutover with one-time migration of supported legacy state.
tags: [architecture, decision, adr, identity, migration]
---

# ADR 0007: Adopt vosu as the sole product identity

- **Status:** Accepted

## Context

The native daemon is a Rust implementation targeting compatibility with upstream [tosu](https://github.com/tosuapp/tosu). Keeping the upstream name as the current product identity would blur responsibility for vosu's releases, first-party services, and state on disk. The rename does not establish copyright ownership or source independence; see the [provenance audit](../provenance-audit.md).

A durable rename affects more than display text. Package and crate names, the executable, canonical TOML configuration and uppercase bindings, legacy migration inputs, data roots, API terminology, release archives, updater metadata, first-party endpoints, operational commands, and documentation form one public identity. Supporting both identities indefinitely would create ambiguous precedence, duplicate test surfaces, and updater and state-management paths that could diverge.

Existing installations may nevertheless contain user configuration, counters, settings, cache data, and `tosu.env`. That state must survive the cutover without making the former name a permanent runtime interface.

## Decision

Adopt **vosu** as the sole current product identity and make a clean cutover across the repository and release surface:

- use `vosu` for the package, crates, executable, API terminology, logs and metadata;
- use flat typed TOML in `vosu.toml` with the current uppercase binding names, and `vosu` platform data roots for configuration and state;
- publish `vosu-linux-<tag>.zip` and `vosu-windows-<tag>.zip` from `18-lab-dev/vosu`;
- use the upstream tosu catalog service at <https://tosu.app/api.json>; vosu does not operate a website;
- reject former executable, archive, environment-variable, updater, and package names rather than accepting aliases.

Preserve user state through a one-time, idempotent migration. On Linux, move non-conflicting files from the former `tosu` XDG config, data, and cache roots into the corresponding `vosu` roots and remove empty former roots. On every platform, initialization layers defaults, `tosu.env`, `vosu.env`, and `vosu.toml` with precedence `vosu.toml > vosu.env > tosu.env > defaults`. It writes the canonical `vosu.toml` successfully before removing either readable legacy env file; malformed canonical TOML leaves all sources untouched. After migration, read, write, and watch only `vosu.toml`.

Retain the string `tosu` only when its role is explicit and cannot truthfully be renamed: the upstream project and source revision, historical release records, one-time migration inputs, and parity or rejection fixtures that validate behavior against or away from the upstream identity. Provenance-pinned vendored inputs remain governed by their owning evidence workflow.

## Consequences

- Users and operators have one executable, configuration namespace, state location, update channel, and documentation vocabulary.
- The rebrand is intentionally breaking for scripts or clients that depend on branded names; they must move to vosu rather than rely on aliases.
- Existing user state is preserved once, while steady-state code and documentation remain free of fallback paths.
- Upstream compatibility remains reviewable under a distinct product identity. Source provenance and licensing require separate evidence.
- Audits can treat unexplained former-brand occurrences as defects; allowed occurrences must identify themselves as upstream, migration, parity, provenance, or history.
- The migration may be removed after the supported migration window without affecting the current storage contract.

## Alternatives considered

### Keep tosu as the Rust daemon's public name

Rejected because it couples vosu's releases and services to the upstream identity.

### Support tosu and vosu aliases indefinitely

Rejected. Dual executables, packages, environment variables, paths, archives, API names, and updater channels create permanent precedence rules and split testing, documentation, and state ownership. A compatibility alias strategy would undermine the clean cutover.

### Rename the product but leave state in former locations

Rejected because current on-disk paths and configuration names are part of the product contract. The bounded migration preserves user data without retaining those locations as ongoing interfaces.

## Related documentation

- [User installation, configuration, and migration](../../README.md)
- [Architecture](../architecture/overview.md)
- [Agent instructions](../../AGENTS.md)

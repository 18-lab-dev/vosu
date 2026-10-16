---
type: Reference
title: vosu maintainer quickstart
description: Entry point for engineers and agents working on vosu, a native Rust daemon that reads osu! stable and lazer state and serves compatible HTTP and WebSocket APIs.
resource: README.md
tags: [vosu, rust, documentation, onboarding]
---

# vosu maintainer quickstart

`vosu` is a single native Rust daemon for osu! telemetry, counters, reports, live calculations, and compatible APIs. It discovers osu! stable/lazer processes, reads memory without modifying the client, reduces samples into coherent immutable state, and serves dashboard, HTTP, and WebSocket consumers.

To set up a checkout, build, run, and run the checks, follow [CONTRIBUTING.md](../CONTRIBUTING.md). This page maps the maintainer documentation.

## Dashboard connection and attachment indicators

Installed cards show a connected chip only while at least one live WebSocket can
be attributed to that overlay. The read-only `GET /api/dashboard/status` response
contains per-folder `connections` and the existing native discovery roster,
client type, published game version, and configured common/precise polling intervals.
Instances without a published state show “resolving offsets” or “starting reader”,
according to the existing lifecycle's offset readiness, rather than “not detected”.
Published instances show “reading”; this is the reader lifecycle, not a separate health probe. Dashboard
previews count too; these are socket counts, not unique OBS sources or people.

The command socket's `l` identity is decoded using the command protocol, including
`/@vosu-profile/<profile>/<overlay>` identities. Other sockets can be counted when
they explicitly supply `l`, or a same-server Referer names an overlay folder.
Browsers normally send only an Origin on a WebSocket handshake, not a Referer.
Origin has no overlay path, so a stock `/ws`, `/tokens`, V2, precise, or subscription
socket with only Origin cannot be assigned to a particular overlay. Native clients
with neither identity nor Referer also cannot be counted. No zero or estimated
count is shown for those cases.

`GET /api/counters/details/<folder>` reports the absolute folder path, summed file
lengths in bytes, scan entry count, and `complete`. The metadata-only scan skips
symlinks and Windows reparse points, uses no-follow anchored directory descriptors
on Linux, and stops at 10,000 entries, 32 levels, or a 100 ms work deadline. A
truncated scan is labelled “at least” in the delete dialog. It never reads file
contents. Filesystem calls themselves may take longer than the work deadline.
Both UX read endpoints are dashboard-origin-only GET routes; the overlay listener
returns 404 for them. Same-origin dashboard fetches include `X-Vosu-CSRF`, including
these reads. The public cross-origin `/api/dashboard` discovery probe deliberately
omits the secret. Size-scan errors mark the result incomplete; if details cannot be
retrieved at all, deletion can still be confirmed without a size.

## Start here

1. Read [Architecture overview](architecture/overview.md) for ownership, lifecycle, process generations, sampling, snapshots, and server boundaries.
2. Use the [Source map](architecture/source-map.md) to find the owner of a concern before editing code.
3. Follow [Runtime and configuration workflows](workflows/runtime-and-config.md) for startup, paths, reload, counters, listener behavior, and updates.
4. Follow [Compatibility and calculation workflows](workflows/compatibility-and-calculation.md) when changing API schemas, WebSockets, precise state, parity, or pp calculations.
5. Use the [Operations runbook](operations/runbook.md) for platform prerequisites, security boundaries, release/update handling, and troubleshooting.
6. Use [Testing and verification](testing/verification.md) to choose the narrowest behavioral check and the broader CI/release gates.

The repository’s detailed source documentation remains authoritative: [README.md](../README.md) is the product/operator contract, the [architecture overview](architecture/overview.md) explains structural boundaries, [decision records](decisions/index.md) explain durable choices, and [benchmarks](testing/benchmarks.md) contain machine-specific measurements. See the [documentation index and conventions](index.md) for the complete map.

## Maintain documentation with the change

`docs/` is the maintained, ordinary reviewed Markdown tree. Update affected API/configuration/runtime contracts in the implementation PR, architecture when boundaries change, and benchmark notes only with reproducible measurements. Preserve dated experiments and provenance as historical evidence. No generator, scheduled inference, or post-merge rewrite is required. Optional AI assistance should address a bounded task and its diff must be reviewed.

## Compatibility hotspots

API payloads are the most compatibility-sensitive area: upstream `hitErrorArray`/`hitErrors` arrays sit beside the additive `pressHitErrors` and `releaseHitErrors` siblings, v2 parsed beatmap stats expose `hitWindow` judgement fields, and v2 selected-field subscriptions, WebSocket close handshakes, and precise keypress state are covered by fixtures. Use the targeted verification in [Testing and verification](testing/verification.md) when touching them.

## Documentation conventions

- `src/` owns runtime behavior; `crates/` owns workspace subcrates; `tests/`, `benches/`, and `tools/` provide behavioral evidence and maintainer tooling.
- Preserve exact routes, payloads, status behavior, bounded queues, process-generation checks, updater verification, and platform safety rules.
- Extend the atomic snapshot path rather than reading independent state components from a route or wire builder.
- Keep `tosu` references only where they are explicitly migration, upstream, parity, provenance, or historical evidence; the current product name is `vosu`.

## Backlog

- **Quarantined updater transaction operator procedure** — anchor: `src/updater/transaction.rs`, [architecture overview](architecture/overview.md); recovery behavior is documented, but a step-by-step diagnosis/escalation procedure is not explicit.
- **Linux stable/Wine parity evidence** — anchor: `tools/live-parity/live_parity.py::SUPPORTED_MATRIX`; runtime support exists, while the inspected parity matrix currently lists Linux lazer rather than Linux stable.

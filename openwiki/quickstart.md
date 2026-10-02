---
type: Reference
title: vosu code wiki quickstart
description: Entry point for engineers and agents working on vosu, a native Rust daemon that reads osu! stable and lazer state and serves compatible HTTP and WebSocket APIs.
resource: README.md
tags: [vosu, rust, code-wiki, onboarding]
---

# vosu code wiki

`vosu` is a single native Rust daemon for osu! telemetry, counters, reports, live calculations, and compatible APIs. It discovers osu! stable/lazer processes, reads memory without modifying the client, reduces samples into coherent immutable state, and serves dashboard, HTTP, and WebSocket consumers. The dashboard supports persistent installed-counter pinning and drag ordering, Shift-click URL opening, and `/` search focus (`assets/server/homepage.js`). The workspace targets Rust 1.96.1 and contains the production binary, the clean-room `vosu-pp` calculation facade with its `rosu-map-api` and `rosu-pp-api` workspace members, and a test-only harness (`Cargo.toml`).

## Start here

1. Read [Architecture overview](architecture/overview.md) for ownership, lifecycle, process generations, sampling, snapshots, and server boundaries.
2. Use the [Source map](architecture/source-map.md) to find the owner of a concern before editing code.
3. Follow [Runtime and configuration workflows](workflows/runtime-and-config.md) for startup, paths, reload, counters, listener behavior, and updates.
4. Follow [Compatibility and calculation workflows](workflows/compatibility-and-calculation.md) when changing API schemas, WebSockets, precise state, parity, or pp calculations.
5. Use the [Operations runbook](operations/runbook.md) for platform prerequisites, security boundaries, release/update handling, and troubleshooting.
6. Use [Testing and verification](testing/verification.md) to choose the narrowest behavioral check and the broader CI/release gates.

The repository’s detailed source documentation remains authoritative: [README.md](../README.md) is the product/operator contract, the [architecture overview](architecture/overview.md) explains structural boundaries, [decision records](decisions/index.md) explain durable choices, and [benchmarks](testing/benchmarks.md) contain machine-specific measurements. See the [documentation index and conventions](index.md) for the complete map.

## Maintain documentation with the change

`openwiki/` is ordinary reviewed Markdown. Update affected API/configuration/runtime contracts in the implementation PR, architecture when boundaries change, and benchmark notes only with reproducible measurements. Preserve dated experiments and provenance as historical evidence. No OpenWiki CLI, subscription, recurring inference job, or post-merge regeneration is required. Optional AI assistance should address a bounded task and its diff must be reviewed. The directory name is retained to keep links stable.

## Build and run

Use the pinned toolchain and lockfile:

```sh
cargo build --locked
cargo run --locked
```

The default listener is `127.0.0.1:24050`; the dashboard is `/`. Linux production builds use `--release --locked --no-default-features --features linux-process-events`; Windows uses `--release --locked --no-default-features`, each with its native target. Ordinary daemon builds require no Node.js, Electron, .NET, or C# installation: dashboard and baseline offsets are vendored, while release binaries carry the self-contained generator payload for exact-instance local generation when needed.

## Recent direction

Recent changes explain current hotspots: the dashboard now persists pinned counter order; compatibility payloads classify hit errors into `all`, `press`, and `release`; and v2 parsed beatmap stats expose `hitWindow` with optional numeric judgement fields `miss`, `meh`, `ok`, `good`, `great`, and `perfect`. Catch publishes all six fields as zero, while `stars.hitWindow` is absent. Earlier changes added v2 selected-field subscriptions, WebSocket close handshakes, precise keypress state/timestamps, external consumers, and the precise input-validation harness. Treat these as compatibility-sensitive areas and use the targeted verification in [Testing and verification](testing/verification.md).

## Documentation conventions

- `src/` owns runtime behavior; `crates/` owns workspace subcrates; `tests/`, `benches/`, and `tools/` provide behavioral evidence and maintainer tooling.
- Preserve exact routes, payloads, status behavior, bounded queues, process-generation checks, updater verification, and platform safety rules.
- Extend the atomic snapshot path rather than reading independent state components from a route or wire builder.
- Keep `tosu` references only where they are explicitly migration, upstream, parity, provenance, or historical evidence; the current product name is `vosu`.

## Backlog

- **Live parity operations cadence and pinned-artifact ownership** — anchor: `tools/live-parity/`, `AGENTS.md`; the repository defines the gate and supported matrix but not a recurring maintainer schedule.
- **Quarantined updater transaction operator procedure** — anchor: `src/updater/transaction.rs`, [architecture overview](architecture/overview.md); recovery behavior is documented, but a step-by-step diagnosis/escalation procedure is not explicit.
- **Linux stable/Wine parity evidence** — anchor: `tools/live-parity/live_parity.py::SUPPORTED_MATRIX`; runtime support exists, while the inspected parity matrix currently lists Linux lazer rather than Linux stable.

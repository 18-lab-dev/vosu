# Split vosu 2.0 into a minimal core and plugins

- Status: Proposed
- Date: 2026-11-21
- Supersedes: [ADR 0017](0017-tool-host.md) in part (tool services, the rejection of in-process plugins and of a native plugin ABI)
- Extends: [ADR 0003](0003-process-memory-platform-boundary.md)

## Context

vosu 1.x is one daemon crate of about 122,000 lines that discovers osu!
clients, reads their memory, reduces state, serves four compatibility APIs,
hosts overlays and tools, and updates itself. Every extension point is
osu!-specific, and tools that need native work each ship a supervised service
with its own HTTP server, port, per-launch token, and readiness protocol.

vosu 2.0 keeps the same out-of-the-box experience for osu! users but turns the
daemon into a general memory-reading runtime for rhythm games. Game support,
data providers, API compatibility layers, and the dashboard become plugins on a
small core. osu! remains the only publicly supported game.

A rewrite in Zig or C3 was considered to make builds near-instant and rejected.
On a Ryzen 9 9950X3D at `d01e151`, with dependencies compiled, an edit in
`src/api/fixed.rs` cost 2.4–2.7 s for `cargo check`, 3.8–6.2 s for
`cargo build`, and 6.3–7.0 s for a library test build; rebuilding the `vosu`
crate alone took 26.5 s. The cost comes from compiling one large crate. Zig's
fast incremental path is experimental, ELF-only, and pre-1.0 (the 0.16.0 release
notes list known miscompilations), and a rewrite would replace 402 locked
crates, including the pinned calculator forks and the network, TLS, and archive
hardening. Splitting the crate (below) addresses the cost directly.

## Decision

### Core and distribution

`vosu-core` owns only privileged and shared services:

- process discovery, including the eBPF and CN_PROC backends, and native memory
  reads, signature scanning, and module enumeration, exposed to plugins as host
  services scoped to the process a plugin matched;
- the state bus: immutable latest-only snapshots
  ([ADR 0002](0002-immutable-latest-only-snapshots.md)) and demand-driven
  sampling from field subscriptions;
- the HTTP and WebSocket listeners and their origin split
  ([ADR 0016](0016-separate-dashboard-origin.md)), configuration, logging,
  the updater, the plugin registry, and the install resolver
  ([ADR 0022](0022-install-resolver-and-2-0-transition.md)).

The interface between a game plugin and core does not assume memory reading: a
plugin may also read a game's log, a mod's socket, or an official API.

The `vosu` executable is core plus the bundled plugins, compiled in and
registered through the same plugin interface that external plugins use. This
keeps the updater's single-root-executable staging, rollback, and recovery
([ADR 0006](0006-transactional-self-update.md)) and makes the bundled set work
offline. The bundled set is the dashboard, overlay hosting, tool hosting, the
osu! plugin (stable and lazer readers and calculation), and the tosu v2,
gosumemory v1, and StreamCompanion adapters
([ADR 0021](0021-api-v1-and-upstream-adapters.md)).

The dashboard cannot be disabled from the dashboard. Headless installations
disable it with a configuration key or `VOSU_DASHBOARD=off`.

### Roles

- **Plugins** define and retrieve data: game plugins read clients; provider
  plugins compute or fetch data and expose actions; adapters project the state
  bus into a published API contract.
- **Tools** do things with that data. A tool is web code only: a UI and
  optional browser-side code that reads data and invokes plugin actions through
  a dashboard-granted session. Tools no longer ship services. Native work that a
  1.x tool service did moves into a plugin; tool command lines (`vosux`) become
  commands that plugins provide.
- **Overlays** display data and are read-only.

Native code runs only in plugins.

### Plugin ABI

Plugins implement one versioned C ABI with a Rust SDK and a C header. Calls
exchange messages and handles; no pointer crosses the boundary into host or
plugin memory. The same plugin binary can therefore run inside the daemon or in
a plugin-host process without changing its contract.

### Execution and trust

A plugin manifest requests `runtime = "in-process"` or `runtime = "process"`.
The host decides:

| Granted runtime | Eligible plugins | Memory access |
|---|---|---|
| `in-process` | bundled plugins, plugins from the official sources, and plugins from a source the user explicitly trusts ([ADR 0023](0023-authenticated-catalog-sources.md)) | direct reads through core's process handle |
| `process` | every other plugin, after user approval as for 1.x user tools; any plugin may request it | a batched read plan sent to core, executed only against the process that plugin matched |

A request for `in-process` from a plugin that is not eligible is refused rather
than downgraded silently, so the manifest cannot bypass the trust decision.

Plugin hosts run `vosu plugin-host`. On Linux, `vosu --setup` grants the
executable `CAP_SYS_PTRACE` (and, where eBPF works, `CAP_BPF`, `CAP_PERFMON`,
and `CAP_SYS_RESOURCE`). With `CAP_SYS_PTRACE`, `process_vm_readv` can read any
process on the machine, so a plugin host drops every capability before it loads
plugin code and obtains game memory only through core. On Windows, same-user
`PROCESS_VM_READ` needs no extra privilege; plugin hosts are confined to a job
object for lifetime cleanup, as 1.x tool services are.

An out-of-process plugin is isolated from daemon crashes but is not sandboxed:
it runs with the user's filesystem and network access. Approval for untrusted
plugins therefore remains a trust decision, as for 1.x user tools.

### Source layout

The workspace splits along the existing module boundaries
(`scripts/module-boundaries.txt`): `vosu-core`, `vosu-plugin-api` (ABI and
SDK), one crate per bundled plugin, and one per adapter. An edit then recompiles
its own crate and the link step rather than the whole daemon.

## Consequences

- One plugin crash in a `process` host cannot stop other games or the daemon.
  An in-process plugin can, which is why that runtime requires trust.
- Brokered reads add an inter-process round trip per sample plan. Its cost must
  be measured with `memory_reader_bench` against direct reads before the plugin
  ABI is frozen; the estimate that read syscalls dominate is unmeasured.
- 1.x tool services, their HTTP proxying, readiness protocol, and per-launch
  service tokens are removed. Every official tool is rebuilt for 2.0 before
  release ([ADR 0022](0022-install-resolver-and-2-0-transition.md)).
- The 1.x rule that tool manifest permissions are informational remains true
  for `process` plugins.

## Alternatives considered

- **Everything in-process:** fastest, but a third-party crash stops every game,
  and downloaded native code would run inside the process holding
  `CAP_SYS_PTRACE`.
- **Everything out-of-process:** isolates all plugins but puts first-party hot
  paths behind brokered reads for no security benefit.
- **WebAssembly plugins:** one artifact for both platforms and enforceable
  permissions, but a large runtime dependency, a third execution mode to test,
  and no GPU or .NET work. Revisit when untrusted third-party plugins become
  common; the message-based ABI keeps that possible.
- **Rewrite in Zig or C3:** see Context.

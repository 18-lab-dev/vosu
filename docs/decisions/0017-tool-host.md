# ADR 0017: Host tools on the overlay listener under a reserved `/tools` prefix

Status: accepted.

## Context

Practice and analysis utilities (rate changers, skill trackers, difficulty calculators) each reimplement process discovery, memory reading, settings, storage, and an HTTP server. vosu already owns game state, beatmap files, calculation, and a dashboard. Tools should consume those services instead, in any language, and share work: a difficulty calculation should run once and serve every consumer.

## Decision

Add **tools** as vosu's first extension type, specified in the [tool protocol](../tools/protocol.md):

- Tools are out-of-process. A `tool.toml` manifest declares identity, an optional static UI, an optional supervised service, API routes, dependencies on other tools, and requested host permissions. No native plugin ABI.
- Tools are served on the **overlay listener** under the reserved `/tools/` prefix: UIs at `/tools/<id>/`, proxied service APIs at `/tools/<id>/api/`. No additional listener or user-visible port. No overlay may be installed, served, or configured as `tools`.
- The dashboard listener gains a Tools page and management APIs, following ADR 0016: lifecycle, trust, settings, logs, and data actions are dashboard-origin mutations with CSRF.
- Tools may depend on other tools. A dependent tool calls its dependencies through the same proxy with its per-launch token, so shared providers (for example the first-party difficulty provider) compute and cache once.
- Services bind loopback port 0, announce readiness on stdout, authenticate every forwarded request with a per-launch token, and exit when stdin closes.
- Tool roots are watched and `--tool <directory>` runs a development tool from any checkout, restarting it when its executable changes, so authors iterate without packaging or restarting vosu.
- Official tools are distributed separately through the vosu-tools HTTPS catalog. Size and SHA-256 verification, hardened ZIP extraction, manifest identity checks and atomic replacement precede trust. This preserves the daemon updater's single-root-executable archive validation. No tools or SDK are embedded in vosu releases.
- Tools may declare optional `[cli]` argv. `vosux` (argv[0] dispatch in the
  same executable) and `vosu x` discover the running daemon through private
  `broker.json`, then spawn directly with inherited terminal streams. The CLI
  path bypasses daemon startup, locking, tray, logging and updater work.
- The native broker can mint bounded, revocable, 24-hour CLI sessions. Unlike
  service tokens, CLI tokens can write their own tool API as well as declared
  dependencies, keeping the service the single writer of shared tool data.
  The launcher revokes after child exit. Tools never receive the broker token.
- [vosulay](https://github.com/shiruviaa/vosulay) is an optional official out-of-process tool, not an embedded overlay engine. It owns its rendering and manager and retains standalone mode, preserving [ADR 0008's boundary](0008-external-in-game-overlays.md). A voluntary status-0 service exit means `stopped` (`exited`), not a crash; even `always` tools wait for manual start or the next vosu launch.

### Write authority on a shared origin

Tool UIs share the overlay origin, so overlay JavaScript can read anything a tool UI can read and can load tool pages. Tool APIs therefore split routes into `read` (open, like other overlay reads) and `write`. Write routes require either a dependent tool's token or a **UI session** that only the dashboard can mint: Open requests a single-use launch code with CSRF and passes it in the URL fragment of a new, opener-less tab, where the bridge exchanges it for a session held only in that document's JavaScript memory. A UI opened directly, or reloaded, is read-only until opened from the dashboard again.

The session is deliberately never written to `sessionStorage`, `localStorage`, IndexedDB, or cookies: all of them are scoped to the origin, not the tool path, so an overlay that later loads in the same tab (or a window that inherits a copy of `sessionStorage`) could read it. An earlier `sessionStorage` design was reproduced as exploitable this way. Losing write access on reload is the accepted cost.

This is still weaker than an opaque-origin sandbox. Any same-origin document holding a script reference to the tool document — a window the tool UI opens without `noopener`, or an overlay the tool UI frames — can reach its live bridge, so tool UIs must not do either. Opaque-origin UIs were rejected because they lose storage, module loading, and workers, which tools need, and the dashboard origin must stay free of third-party JavaScript (ADR 0016).

### Trust

Only tools installed from the exact default source identities `shiruviaa/vosu-tools` and `shiruviaa/vosulay` are `official` and automatically trusted. Exact shorthand identity survives removal/re-addition; tagged and URL variants do not gain trust by resolving to the same repository. These sources use the ordinary source resolver, not privileged transport. The explicit identity allowlist avoids trust expansion through URL equivalence or redirects. Catalog sources are user-configurable and ordered; other catalog tools and manually installed tools are `user` and remain untrusted until approved on the dashboard. Integrity checks do not establish author trust. Overlays from these sources have no tool trust gate and retain the existing third-party JavaScript disclosure. Services run as the user's OS account: host permissions constrain only vosu APIs, not an OS sandbox.

## Rejected alternatives

- **Separate tools listener or dashboard-origin tool UIs**: an extra port to configure and expose, or third-party code with management authority.
- **In-process plugins**: crashes, dependency conflicts, and language lock-in inside the memory-reading daemon.
- **Tool marketplace, dependency resolution/installation, native plugin ABI, OS sandboxing**: deferred; 1.2.0 establishes the contracts they would extend.

## Consequences

Overlays can consume tool data (for example current skill ratings) with plain `fetch`. Disabling a tool keeps its data. Tools never receive the dashboard CSRF token, native broker credential, or API keys. A user tool's manifest permissions are informational at the OS level.

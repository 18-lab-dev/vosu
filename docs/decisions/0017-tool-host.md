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

### Write authority on a shared origin

Tool UIs share the overlay origin, so overlay JavaScript can read anything a tool UI can read and can load tool pages. Tool APIs therefore split routes into `read` (open, like other overlay reads) and `write`. Write routes require either a dependent tool's token or a **UI session** that only the dashboard can mint: Open requests a single-use launch code with CSRF and passes it in the URL fragment of a new tab, where the bridge exchanges it for a session kept in that tab's `sessionStorage`. An overlay in another browsing context never sees the fragment or the session. A UI opened directly is read-only.

This is weaker than an opaque-origin sandbox: an overlay running in the same tab as a launched tool UI could read its session. It was preferred because opaque-origin UIs lose storage, module loading, and workers, which tools need, and because the dashboard origin must stay free of third-party JavaScript (ADR 0016).

### Trust

Bundled tools ship with and are verified by vosu releases. User-installed tools are untrusted until approved on the dashboard, because their services run as the user's OS account. Host permissions constrain only vosu APIs; they are not an OS sandbox and the dashboard says so at approval.

## Rejected alternatives

- **Separate tools listener or dashboard-origin tool UIs**: an extra port to configure and expose, or third-party code with management authority.
- **In-process plugins**: crashes, dependency conflicts, and language lock-in inside the memory-reading daemon.
- **Tool marketplace, dependency resolution/installation, native plugin ABI, OS sandboxing**: deferred; 1.2.0 establishes the contracts they would extend.

## Consequences

Overlays can consume tool data (for example current skill ratings) with plain `fetch`. Disabling a tool keeps its data. Tools never receive the dashboard CSRF token, native broker credential, or API keys. A user tool's manifest permissions are informational at the OS level.

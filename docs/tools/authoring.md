---
type: Guide
title: "Authoring vosu tools"
description: "Develop, test, and distribute a tool in Rust, Python, or any language."
---

# Authoring vosu tools

Tools are local applications supervised by vosu, with an optional UI and HTTP service. Start with the [protocol v1 contract](protocol.md); it defines every environment variable, API, and lifecycle rule. Tools run with your OS permissions, not inside a sandbox. Install only tools you trust.

## Run the Python example

From the repository root, with Python 3.11+ available as `python` on PATH:

```sh
vosu --tool examples/tools/hello-python
```

Use an isolated vosu configuration and unused listener ports if another vosu is running. Open **Tools** in the dashboard, then **Open** beside Hello Python. The count read route works in a read-only tab; increment requires opening from the dashboard to obtain a UI session. The reference [manifest](../../examples/tools/hello-python/tool.toml), [service](../../examples/tools/hello-python/main.py), and [UI](../../examples/tools/hello-python/web/index.html) have no third-party dependencies. The service requests effective settings from `GET /tools/_host/v1/settings` on each count read. If the host is unavailable it reports a real 502 rather than inventing settings.

The host uses the tool directory as its working directory. `command = ["python", "main.py"]` works on Windows as well as Linux when `python` resolves through PATH. Bare executable names resolve through PATH; relative executable paths containing `/` or `\` resolve against the tool directory. Windows adds `.exe` to extensionless executable paths. There is no shell expansion, so use an argv array, not `"python main.py"`. If Linux only has `python3`, change the first argument accordingly.

## Rust with vosu-tool-sdk

Add a binary crate depending on the workspace's `vosu-tool-sdk`, `axum`, and Tokio. The SDK reads the launch environment, checks the token on every route, binds loopback port 0, emits readiness, adds `/healthz`, and shuts down on stdin EOF:

```rust
use axum::{routing::get, Router};
use vosu_tool_sdk::{serve, ToolEnv};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env = ToolEnv::from_env()?;
    let router = Router::new().route("/v1/hello", get(|| async { "hello" }));
    serve(&env, router).await?;
    Ok(())
}
```

Use `env.client().settings::<YourSettings>().await?` for settings and `.context().await?` for the selected beatmap (requires `permissions.host = ["context"]`). Dependency calls use `.tool_json("difficulty", "v1/beatmap/current")` and require a matching `[[requires]]` declaration. Do not log or expose the service token to the browser.

## Manifest walkthrough

`manifest_version = 1`, a unique lowercase `id`, display `name`, and semver `version` are required. IDs match `^[a-z0-9][a-z0-9-]{0,31}$`. `[ui]` points at root-confined static assets and defaults its entry to `index.html`. `[service]` declares argv and `startup` (`always` or `on-demand`). A UI-only tool omits the service; a service-only tool omits the UI. `[[requires]]` pins compatible dependency versions; `[permissions].host` declares access to `context` or advisory `songs:write`. Permission declarations are not a process sandbox.

Put `#:schema https://raw.githubusercontent.com/shiruviaa/vosu/main/docs/tools/tool.schema.json` at the top of `tool.toml` for Taplo / Even Better TOML validation. Offline checkouts can use a relative path to [tool.schema.json](tool.schema.json). vosu also validates semantic versions, path confinement, and dependency cycles at runtime.

### Read versus write

List read-only path prefixes in `[api].read`: only GET, HEAD, and WebSocket upgrades are exposed to overlays. Put mutations in `[api].write`; they require the tool's UI session or an authenticated declared dependent tool. A write prefix wins if both lists match. Prefixes are segment-aware: `/v1/count` does not expose `/v1/countdown`. Unlisted paths return 404. Do not mutate state from a GET: overlays can call read routes without a UI session.

Load `/tools/_host/v1/theme.css` for theme tokens, and import `vosu` from `/tools/_host/v1/bridge.js`. Await `vosu.ready` before fetching. Use `vosu.fetch("v1/count")`, which supplies the current UI session header; disable write controls when `vosu.canWrite` is false. The launch code is single-use and removed from the URL by the bridge. Never send the service token to browser code.

### Settings, jobs, and live data

Optional `settings.json` reuses the [overlay settings format](../../README.md#overlay-settings), including defaults and the dashboard editor. Tools do not use overlay profiles. Read effective values through the authenticated host `/settings` API, not by assuming a file format for persisted host state.

Version API paths under `/v1/`. Errors use `{"error":"message"}` with a suitable 4xx/5xx status. Work taking more than about one second should return `202 {"job":"id"}` from POST, expose `GET /v1/jobs/<id>` with state/progress/message/result, and support DELETE cancellation. Live read WebSockets send a complete snapshot on connect and each change. Keep queues and request sizes bounded; do not publish incremental deltas that require clients to guess missing state.

## Development and distribution

Run `vosu --tool <directory>` against a checkout; this explicitly trusts it for that run and overrides an installed tool of the same id. Manifest and settings edits are watched; rebuilding a development service restarts it. For a Rust first-party crate, build its named binary with `cargo build --locked -p vosu-tool-<id>` and use `vosu --tool crates/vosu-tool-<id>`. See the protocol for the host's Cargo development binary resolution rule.

For any language, implement the same contract: loopback port 0, one flushed `vosu-tool-ready <port>` line, token-authenticated `/healthz` and every other request, exit within five seconds after stdin EOF. Keep stdout available for logs after the handshake and never print secrets. Use the supplied data directory for durable state and cache directory for disposable computations.

Distribute user tools as a directory under `<vosu data directory>/tools/<id>/`; approve the tool in the dashboard before enabling it. Include its interpreter/runtime requirements, license, static assets and manifest. First-party release tools are instead built and staged, then embedded in the vosu executable. At runtime vosu materializes them in a versioned cache directory; packages and updater ZIPs remain single-executable deployments with no adjacent `tools/` payload. See the [release runbook](../operations/runbook.md#updates-and-releases).

## Test a tool

1. Parse its manifest against the schema and check id, paths, required dependencies, and unknown fields.
2. Start with a fake host on a temporary loopback port and fresh data/cache directories. Set all six launch variables from the protocol; capture stdout and keep stdin piped.
3. Check the readiness line and authenticated `/healthz`; missing or incorrect tokens must return 401.
4. Exercise reads, writes, error paths, cancellation and live reconnects. Test host settings with defaults and changed values.
5. Close stdin and require clean exit within five seconds, including while clients are connected.
6. Through an isolated vosu, verify read-only UI access, dashboard launch/session writes, dependency permissions, and disable/restart behavior.

The repository's `python scripts/test_stage_tools.py` exercises schema examples, temporary-workspace staging (including Windows filenames), and the Python service handshake/authentication/settings/EOF lifecycle without touching a running vosu. Release archive tests remain `python scripts/test_package_release.py`.

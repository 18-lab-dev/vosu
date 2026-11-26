# Authoring a native plugin

Choose a plugin when your extension needs native code, a state provider, or an action. A [tool](../tools/authoring.md) is a static UI that can request actions; an [overlay](../overlays/authoring.md) reads state and resources and cannot invoke actions. Neither runs a native service. osu! is the supported game; the common state layer is designed so readers need not hard-code a game where a common field suffices.

## Pin the SDK and the host separately

Use the published [`vosu-plugin-api`](https://crates.io/crates/vosu-plugin-api) crate, not a daemon workspace dependency, a Git checkout or `vosu-plugin-internal`. Pin the published version you support exactly, keep the lockfile, and test against the exact vosu builds you support. For a new Rust library, use this pin:

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
vosu-plugin-api = "=1.0.0-alpha.3"
```

| Version | Meaning |
| --- | --- |
| `vosu-plugin-api` `1.0.0-alpha.2` | Rust SDK release; alpha.2 adds job-artifact codecs and host calls that alpha.1 does not have. |
| `vosu-plugin-api` `1.0.0-alpha.3` | First SDK with package checking for `[runtime.linux]`; older checkers reject the runtime table. It adds no native ABI requirements. |
| `abi_version = 1` | Native ABI major; the shared library exports `vosu_plugin_entry_v1`. Additive opcodes do not change that symbol. |
| `host` | Semver requirement on the **vosu application**, not the SDK. Use `>=2.0.0, <3.0.0` for stable-only packages. Explicitly include the prerelease of the host you support when testing alphas; `>=2.0.0` does not admit an alpha. For packages using `[runtime.linux]`, use `>=2.0.0-alpha.2, <3.0.0`: the first host alpha does not accept the table. |
| `manifest_version = 1`, `catalog_version = 2` | Package and catalog formats, not ABI or application versions. |
| Package `version`, capability versions | Independent semver. A field uses its namespace schema version; an action uses the version of that action, not its package version. |

Follow the [SDK crate guide](../../crates/vosu-plugin-api/README.md) for the actual Rust traits and `export_plugin!`. `Plugin` serializes dispatch; use `ConcurrentPlugin` and the matching export form only with `concurrent_dispatch = true` and internally synchronized state. Do not block callbacks or hold locks across host calls. The [ABI specification](../v2/plugin-abi.md) defines the layout and wire messages; C authors use [`vosu_plugin.h`](../../include/vosu_plugin.h).

## Package the capabilities you need

Every ZIP puts [`manifest.toml`](../v2/catalog-v2.md#package-manifest-v2-manifesttoml) at its root. A `kind = "plugin"` manifest has a dotted id (use a namespace you own), a runtime request, the ABI major and a relative library path per native target (`x86_64-unknown-linux-gnu` `.so`, `x86_64-pc-windows-msvc` `.dll`). `kind = "tool"` has `[ui]` instead; `kind = "overlay"` has a root `index.html` and can set a `resolution`. Do not mix kind-specific sections.

- Only plugins declare `[[provides]]`. Publish a whole namespace with a package-relative JSON Schema whose `title` is the namespace. The host reads the bounded [schema and sampling annotations](../v2/catalog-v2.md#capability-fields-plugin-tool-and-overlay-manifests); it does **not** validate every state publication against a third-party JSON Schema. Keep public values safe at the producer.
- Use `[[requires]]` for capabilities you cannot run without, `[[optional]]` for those you can do without. Capabilities include `field:`, `resource:`, `action:` and `game:`; overlays may only require or optionally use fields and resources. Declare exactly what you need. A plugin's ABI read and action grants are enforced; HTTP reads under API v1's peer policy are not made private by a capability declaration.
- An action has matching `[[provides]]` and `[[actions]]` entries at the same version. Ship both argument and result schemas using the [bounded action-schema profile](../v2/catalog-v2.md#bounded-action-schema-profile-a3). Core checks arguments before dispatch and successful results before sending them to callers.
- Put native settings in the package's `settings.schema.json` and reference it with `settings_schema`. Keep presentation settings in the web tool's `settings.json`. The plugin receives effective settings and its data directory at creation and a complete `SETTINGS_CHANGED` object on each accepted edit. Store native secrets as write-only `password` settings; never echo them in a log, field, error, action result or artifact.
- Mirror the manifest in `catalog-v2.json` and validate the catalog and extracted target directories with the SDK's [`vosu-package-check`](../v2/catalog-v2.md#package-checker-af10). The [manifest and catalog schemas](../v2/catalog-v2.md#offline-packaging-schemas) check shape; the SDK checker additionally checks semantics and assets. A pass is not source trust or authorization to run code.

## Runtime, trust and native approval

`runtime = "process"` runs in a separate `vosu plugin-host` child, with crash supervision. On Linux the child drops the daemon's capabilities and prevents privilege regain before opening the library; on Windows a job object controls its lifetime. **Neither is an OS sandbox.** Native plugin code still has the user's ordinary filesystem and network authority. ABI grants constrain operations requested *through vosu*, not what arbitrary native code can do itself.

`runtime = "in-process"` shares the daemon's address space and privileges. It is permitted only for bundled plugins and plugins from official or explicitly trusted sources (except for the debug-only development override). An ineligible request is refused, never silently run as `process`. Hashes, HTTPS, catalog credentials and private-network permission do not make an author trusted.

An untrusted-source process plugin needs **explicit approval** before it can run. On Linux this also applies to an official or trusted plugin requesting [user namespaces](../v2/plugin-abi.md#8-manifesttoml-runtime-fields-for-kind--plugin). Inspect the dashboard install plan: it names the native code, requested runtime, source, required approvals, and any declared user-namespace or private osu! path access. Approval binds the manifest and target library hashes and runtime to the exact source/version/package identity (or canonical development directory); changed bytes require approval again. Revoking trust blocks new loads; affected process plugins stop and may need fresh approval. In-process code already loaded is reported as **revocation pending restart**: vosu does not unload it or restart on your behalf.

In an isolated debug or prerelease build, `vosu --plugin <directory>` registers a development package. A prerelease release build admits only the process runtime for such packages, and still requires approval in **Tools → Plugins**. A debug build additionally has an explicit in-process exception; a debug process plugin still needs approval. Stable release builds refuse this development option. See [runtime policy](../v2/plugin-abi.md#71-runtime-request-and-the-hosts-decision) and [approval and management](../v2/catalog-v2.md#process-plugin-approval-and-management-a55-approved-2026-11-24).

## State, resources and precision

The [state schema](../v2/state-schema.md) describes the common `vosu.client`, `vosu.status`, `vosu.chart`, `vosu.play` and `vosu.skin` namespaces and the osu!-specific `vosu.osu` namespace. A game provider declares the common fields it fills through `covers` and the resource kinds it implements. Clients use the common layer first and handle absent namespaces and unknown values without substituting zero. A plugin attached as a `vosu.sessions` subscriber can publish plain values in its own namespace on those sessions, not in another provider's namespace, common fields or resource slots.

Plugins use the [ABI state services](../v2/plugin-abi.md#66-state-publication-to-namespaces); tools and overlays use [API v1](../v2/api-v1.md) on the overlay listener. Publish public chart, audio, background or skin assets as [resource or tree references](../v2/api-v1.md#resources-and-trees), not absolute paths. Never publish credentials, private records or OS paths. When a native plugin really needs private osu! install paths, declare the narrow [plugin-only grant](../v2/state-schema.md) for the ABI. No API v1 reader, overlay or launched tool can see it.

Use `DEMAND_CHANGED` and the namespace schema's sampling lanes to do precise work only while it is selected. A reader should select only the fields it renders: choosing the hit-offset or per-key error subtree opts into that collection, while an unrelated precise field does not. A one-shot HTTP read does not create demand. There is no separate API v1 precise socket.

## Actions, timeouts and long work

An action declaration sets `session` to `required`, `optional` or `none`. A session-bound job resolves `active` at creation, never follows focus thereafter. Use the bare `<plugin-id>.<action>` in calls; the `action:` prefix is for capability ids only.

- **Tool:** declare the capability in `requires` or `optional`. The provider must resolve and the tab must have been opened by the dashboard. Use [`vosu.invoke` or `vosu.start`](../tools/protocol.md#browser-bridge) with a session id (or `active`) for a session-scoped call and `null` for a sessionless one. Opening the URL directly or reloading it is read-only.
- **CLI:** set `cli = true` to expose `vosux <plugin-id> <action> [json-args]` (or `vosu x <plugin-id> <action> [json-args]`). It uses a native loopback broker credential from the running daemon, not an API key. It defaults to `active` for `required` or `optional`, accepts `--session <id>` for those actions and refuses that option for `none`. `--detach` returns the job id without waiting.
- **Another plugin:** declare the required or optional action capability and use the ABI's `ACTION_CALL`. This creates no HTTP job. An overlay cannot invoke actions at all.

The daemon's invocation deadline is **30 seconds**; neither HTTP `wait` nor a bridge handle from `vosu.start` can extend it. Report progress when it changes and send exactly one terminal result, even after honoring `ACTION_CANCEL`. Cancellation is advisory; a result that arrives first stands. A timed-out job ends even if in-process code continues. For a process provider, core can kill the host after the five-second timeout grace; a mere cancellation request does not kill it.

For a render, download or other long task, implement a bounded [plugin-owned operation](../v2/api-v1.md#plugin-owned-operations-af3): a short `.start` action returns an opaque `operationId`, and short `.status` and `.cancel` actions take that id. An operation id is **not** the job id of `.start`. Bound queue count, bytes and retained results. Preserve restart behavior only where the feature has a durable queue; the host provides no general scheduler or resumption guarantee. Use [staged uploads](../v2/api-v1.md#staged-uploads-af6) for existing large upload flows, not a larger action body.

## Return protected files as job artifacts

If a successful action produces a private or sessionless file, declare a [job artifact](../v2/api-v1.md#job-artifacts-af12) before its terminal result. Use the alpha.2 SDK's `HostServices::declare_artifact` and [ABI artifact messages](../v2/plugin-abi.md#613-invocation-owned-artifacts-af12). Supply an opaque lookup key, a safe single filename and the exact byte length, not an OS path. Serve it on `ARTIFACT_OPEN` from a declared plugin-wide root or a stream; release backing data only after `ARTIFACT_RELEASE` or after tearing down the instance. An unsupported host is a clear action error, never a fallback to a public resource. An ABI-only call cannot declare a job artifact.

For long work, finish the operation first; let a short `.output` action create the job that owns the file. A tool uses `vosu.start` with `handle.artifacts()` to get download handles. Only the creating launch session or the native broker can read the job file, with one byte range; no public tokenized URL is generated. Files are released on job expiry, failure, provider loss or launch revocation. `vosux` lists artifact metadata on stderr but does not download it.

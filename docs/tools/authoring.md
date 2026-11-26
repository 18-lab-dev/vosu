---
type: Reference
title: "Tool authoring reference"
description: "Manifest, UI, state, actions, settings and testing reference for 2.0 web tool authors."
---

# Tool authoring reference

New to tools? Follow [Create a tool](create-a-tool.md) from an empty directory to a published catalog. This page is the companion reference; the [tool protocol](protocol.md) is the contract for routes, launch sessions and the bridge, and [API v1](../v2/api-v1.md) for state, resources and actions.

A 2.0 tool is a static web application. vosu serves its files; only the dashboard's **Open** gives a browser tab a launch session. No tool process is started. Native work belongs to a [plugin](../plugins/authoring.md) action.

## Development launch

`vosu --tool <directory>` loads a package directory for that run and trusts it, replacing an installed tool with the same id. Use [isolated paths and spare ports](create-a-tool.md#3-run-an-isolated-vosu) when another vosu is running. Edits to `manifest.toml`, `settings.json` and the UI files apply without restarting vosu: reload the tool tab, then open it from the dashboard again for a fresh session.

The [Hello web example](../../examples/tools/hello-web/) is a complete tool without a build step: live chart and play values, a background resource, a setting, and an optional plugin action with progress and cancel.

## Manifest walkthrough

`manifest.toml` needs `manifest_version = 1`, `kind = "tool"`, a lowercase `id` matching `^[a-z0-9][a-z0-9-]{0,31}$`, a display `name`, a semver `version`, a `host` requirement on the vosu version, and `[ui]` with its static `root` (and `entry`, default `index.html`). Alpha hosts need the prerelease in the range, for example `host = ">=2.0.0-alpha.1, <3.0.0"`. The [catalog v2 manifest](../v2/catalog-v2.md#package-manifest-v2-manifesttoml) is the field reference.

`[[requires]]` and `[[optional]]` list the capabilities the tool uses, with semver requirements:

- `field:<namespace>[.<field>…]` for state it reads, such as `field:vosu.play` or `field:vosu.osu.play.pp.current`;
- `resource:<kind>` for files it loads through resource references, such as `resource:chart.background`;
- `action:<plugin-id>.<name>` for each plugin action it invokes.

The install plan shows the list and an unresolved `requires` entry blocks the tool. An action is callable only when listed **and** resolved to an installed provider, and only from a tab with a live launch session. Field and resource requirements describe dependencies; they are not a read firewall. Put a capability in `optional` when the tool works without it. Built-in namespaces (`vosu.client`, `vosu.status`, `vosu.chart`, `vosu.play`, `vosu.skin`) resolve without installing anything; fields the current session does not fill remain absent or unknown.

Give the tool an identity on the Tools page with `icon` (square SVG, or PNG at least 256×256, shown at 40–64 px) and `banner` (3:1 PNG, JPEG, WebP or SVG, recommended 1200×400, cropped to the card width, so keep the subject centred). Both are paths inside the package. Catalogs repeat them as HTTPS `icon_url`/`banner_url`.

## The bridge and write authority

Import the bridge by its absolute path and wait for it before acting:

```js
import { vosu } from '/tools/_host/v2/bridge.js';
try { await vosu.ready; } catch (error) { showReadOnly(error.message); }
```

`vosu.canWrite` means this page holds a session, not that it can call every action. The dashboard's **Open** supplies a single-use launch code; the bridge strips it from the URL before asynchronous work, exchanges it, and keeps the session in module memory. A reload, a bookmark or a direct visit is read-only by design: disable action controls and point the user at **Open**. Handle revoked sessions and unavailable optional providers even when a tab still holds a token.

Reads never need the session. Use plain `fetch` for your own static files and API v1 reads; the bridge sends the session only on action, job and job-artifact requests it builds itself. Never copy anything into `localStorage`, `sessionStorage`, IndexedDB or cookies in a way that could grant authority, never open same-origin pages without `noopener`, and never frame overlays: overlays share the tool's origin and storage.

## Look and layout

Link `/tools/_host/v1/theme.css` and build from its `v-` classes, so every tool matches the dashboard and each other. The bridge sets `data-theme` from the dashboard's mode, and the theme sets `color-scheme`, so native popups and scrollbars stay legible without tool code. Follow the [design system guide](design.md): labels only (descriptions belong in `settings.json`), a slim top bar instead of a header, no page scrolling at common desktop sizes, a help hover only for ambiguous labels, and muted audio by default. `/tools/_host/v1/design` shows every component in both modes.

## Live state

`vosu.subscribe(fields, onFrame, onStatus?, {session}?)` follows API v1 JSON Pointers such as `/vosu.play/combo`; see [State through API v1](protocol.md#state-through-api-v1) for the pointer table and frame shapes. Rules that keep a UI honest:

- Reset everything on an initial frame (`frame[0] === 0`): it follows every reconnect and every change of the focused session.
- Delete removed indices from update frames instead of keeping the last value.
- Treat an absent or `null` value as unknown, never as zero.
- Show `onStatus`: while no game is focused the `active` subscription is refused and keeps retrying.
- Call the returned stop function on `pagehide`, and when a fixed session you subscribed to has ended.

Resource and tree references (`{"url", "type"}`, `{"url"}`) are ordinary same-origin URLs: put them in `<img src>`, `<audio>` or `fetch` directly. Never build file paths; state carries none.

Prefer common fields such as `/vosu.play/combo`; fall back to osu!-specific
fields only when needed. Select precise fields only while displaying them:
their demand follows the subscription, not an HTTP read or a separate feed.

## Actions

`await vosu.invoke('<plugin-id>.<name>', args, {session, signal, onProgress})` resolves with the action result. `await vosu.start` returns a job handle (`id`, `done`, `onProgress`, `cancel`, `artifacts`). Omit `session` to target the focused session, pass a specific id to pin it, or pass `null` to call an action that permits a sessionless invocation. Errors carry a `code`: a missing launch session or grant can give `forbidden`, no focused game gives `unknown_session`, and other codes include `invalid_arguments`, `cancelled`, `timed_out`, `plugin_crashed`, or the plugin's own code.

Pass an `AbortSignal` to cancel: aborting before the request sends nothing, aborting while the job is being created still cancels it, and aborting later sends `DELETE`. Cancellation is advisory; a result that wins the race stands.

The job deadline is 30 seconds even with `start`. For longer work, poll
short status actions with the plugin's [operation id](../v2/api-v1.md#plugin-owned-operations-af3),
not the start action's job id. Once ready, a short output action can
attach a [protected artifact](../v2/api-v1.md#job-artifacts-af12) to its job:
use `vosu.start` and `await handle.artifacts()` for authenticated `fetch`
and `download`. Do not put the file at a public resource URL.

## Settings

Optional `settings.json` reuses the [overlay settings format](../../README.md#overlay-settings-profiles), including defaults and the dashboard editor; tools do not use profiles. `await vosu.settings()` returns the effective values with defaults applied. It is a public read that overlays can make too, so `password` settings are never included: a web tool cannot read a secret. A credential belongs in the settings of the plugin that uses it. Re-read settings when the tab regains focus rather than caching them for its lifetime.

## Distribution

A release is a ZIP with `manifest.toml` at the root, the UI files, `settings.json` when used, and redistributable license texts. List it under `tools` in `catalog-v2.json` and validate the catalog and extracted packages with the SDK's [`vosu-package-check`](../v2/catalog-v2.md#package-checker-af10). See [publishing](create-a-tool.md#9-package-and-publish) and [catalog identity updates](catalog.md). Any native dependency is a separately reviewed plugin package: an untrusted-source process plugin needs explicit native approval and is not sandboxed. Official tools are maintained in [vosu-tools](https://github.com/18-lab-dev/vosu-tools); vosu embeds none.

## Test a tool

1. Run `vosu-package-check` on the catalog and extracted package; it applies the same manifest validation as vosu.
2. Start an isolated vosu with `--tool <directory>` on spare ports and confirm `GET /api/tools/<id>` on the dashboard listener reports `status: "running"` with every `requires` entry `satisfied`.
3. Open the tool directly (read-only) and through **Open** (session), and check both states in the UI.
4. Exercise live state with and without a focused game, a reconnect after restarting vosu, action success, failure and cancellation, and a missing optional capability.
5. Check that the tool writes nothing to browser storage that could grant authority and that every action control is disabled while read-only.

The host's own tool tests are `cargo test --locked --test server_compat`, the `vosu-tool-host` crate tests, and `python tests/tool_host_smoke.py`, whose browser phase drives the bridge against a real daemon.

---
type: Contract
title: "Tool protocol v1"
description: "Manifest, process, HTTP, browser-bridge, and dashboard contract between vosu and hosted tools."
---

# Tool protocol v1

A **tool** is an application that vosu discovers, supervises, and serves next to overlays. Tools may ship a browser UI, a background HTTP service, or both. Any language that can read environment variables, write one stdout line, and serve HTTP on loopback can implement a tool. Rust tools may use the `vosu-tool-sdk` crate; [`examples/tools/hello-python`](../../examples/tools/hello-python/) is a dependency-free Python reference. The decision record is [ADR 0017](../decisions/0017-tool-host.md). Editors can validate manifests with [`tool.schema.json`](tool.schema.json).

## Layout and discovery

Each tool is one directory containing `tool.toml`. vosu scans these roots, in order:

| Root | Location | Trust |
| --- | --- | --- |
| Development | Each `--tool <directory>` passed on the vosu command line | Trusted for that run: the user launched it |
| Official | Catalog-installed packages in `<vosu data directory>/tools/<id>/` | Trusted; downloaded from the official catalog and verified against its SHA-256 and size |
| User | `<vosu data directory>/tools/<id>/` | Untrusted until the user approves it in the dashboard |

A development tool replaces an official or user tool with the same `id` for that run. Catalog installation never overwrites a development or user tool. In official and user roots the directory name must equal `id`. Installation provenance is stored in vosu's private tools state, not an archive-controlled manifest flag.

Durable tool data is stored separately at `<vosu data directory>/tool-data/<id>/`, and caches at `<vosu cache directory>/tools/<id>/`. The installation root contains only packages (and temporary hidden staging siblings); no valid tool id can collide with durable data.

vosu watches every root. Added, removed, or edited manifests and `settings.json` files apply without restarting vosu. For development tools, a change to the service executable also restarts a running service, so `cargo build` or saving a script is the whole edit loop.

vosu embeds no tools. The official SDK, packages, and catalog are maintained in [vosu-tools](https://github.com/18-lab-dev/vosu-tools). Installation downloads dependencies first, checks HTTPS destinations and redirects, verifies package size and SHA-256, rejects unsafe ZIP entries, and validates the manifest before atomically replacing a tool. Running services restart after updates; failed extraction leaves the previous installation intact.

For a development service, a missing relative command is resolved by file name in Cargo output: starting at the tool directory, each ancestor containing `Cargo.toml` is checked for `$CARGO_TARGET_DIR/{debug,release}/<file>` (a relative environment value is resolved against that ancestor), or `target/{debug,release}/<file>` when the variable is unset. Debug is tried before release; the nearest matching ancestor wins. Bare executable names such as `python3` resolve through `PATH`; paths containing a separator resolve under the tool directory before the development fallback. On Windows an extensionless executable gets `.exe`.

## Manifest (`tool.toml`)

```toml
manifest_version = 1             # required; vosu 1.2.0 accepts only 1
id = "skill-tracker"             # required; ^[a-z0-9][a-z0-9-]{0,31}$, not starting with "_"
name = "Skill Tracker"           # required, display name
version = "0.1.0"                # required, semver
description = "…"                # optional, one paragraph
author = "18-LAB"                # optional
homepage = "https://…"           # optional, http(s)
icon = "web/icon.svg"            # optional, path inside the tool directory (svg/png)
banner = "art/banner.webp"       # optional, path inside the tool directory (png/jpg/jpeg/webp/svg)

[ui]                             # optional: browser UI
root = "web"                     # static directory, served at /tools/<id>/
entry = "index.html"             # default "index.html"

[service]                        # optional: background process
command = ["bin/vosu-skill-tracker"]  # argv; relative paths containing a separator resolve inside the tool directory.
                                 # Bare names (e.g. "python") resolve through PATH. On Windows extensionless executable paths gain ".exe".
startup = "always"               # "always" (start with vosu) | "on-demand" (default; first request, stop after 10 idle minutes)
role = "background"              # "background" (default) | "backend" (requires on-demand)

[cli]                            # optional: terminal process; same command resolution as service
command = ["bin/vosu-skill-tracker", "cli"]
summary = "Profile and play history"  # optional one-line description

[api]                            # routes reachable through /tools/<id>/api/; anything unlisted is 404
read = ["/v1/profile", "/v1/history", "/v1/current", "/v1/stream"]  # path prefixes; GET/HEAD and WebSocket upgrade; open to overlays
write = ["/v1/import"]           # authenticated mutations, plus authenticated GET/HEAD where not also in read

[[requires]]                     # optional, repeated: tools this tool calls
id = "difficulty"
version = ">=0.1.0"              # semver requirement on the dependency's manifest version

[permissions]                    # optional; shown at approval, enforced for vosu host APIs
host = ["context", "songs:write"]  # see "Host API" below
```

`settings.json` in the tool directory, when present, uses exactly the overlay `settings.json` schema and is edited from the tool's card on the Tools page (**Settings** opens the same dialog overlays use; `/settings?tool=<id>` shows it as a page). Profiles do not apply to tools. Settings of `type: "password"` are write-only from the dashboard: they are never shown or returned there, a blank field keeps the stored value, and **Clear** removes it. Use that type for credentials; only the tool's own service reads the value, through `GET /tools/_host/v1/settings`.
Editors supporting JSON Schema for TOML (Taplo / Even Better TOML) can put `#:schema https://raw.githubusercontent.com/18-lab-dev/vosu/main/docs/tools/tool.schema.json` on the first line. A local manifest can instead use `#:schema ../../docs/tools/tool.schema.json`, adjusted relative to that manifest. The schema validates structure; vosu additionally checks filesystem confinement, dependencies, and semantic versions.

`icon` and `banner` give a tool its identity on the dashboard. The icon is square (SVG, or PNG at least 256×256) and is shown at 40–64 px. The banner is 3:1 (recommended 1200×400) and spans the full card width, cropped with `object-fit: cover`, so keep important content centred. Both are served from the tool directory on the overlay origin as `/tools/<id>/_icon` and `/tools/<id>/_banner` in every tool status, including disabled, untrusted, and failed tools, with a sandboxing `Content-Security-Policy` so an SVG opened directly cannot run script; ToolView reports them as `icon_url` and `banner_url`. A missing or unreadable icon falls back to the name's initials, and a missing banner leaves the card without one.

Prefix matching is segment-aware: `/v1/beatmap` matches `/v1/beatmap` and `/v1/beatmap/…`, never `/v1/beatmaps`. Access is method-aware: GET/HEAD/WebSocket upgrades matching `read` are open even when the same prefix is also in `write`. Other methods require a matching `write` prefix and authenticated write authority. GET/HEAD on write-only prefixes also require that authority; unlisted routes return 404.

`background` services provide useful independent background work or APIs and expose SERVICE plus Start/Stop/Restart. `backend` services are implementation details of their UI or CLI: no SERVICE badge or service controls; requests still start them on demand, with the same dependency and idle-stop rules. Tool cards derive UI / SERVICE / CLI badges from `interfaces`, not `kind`.

On dashboard cards, usable tools without a background service show **Ready**, or **Working** while their backend starts or runs. Background services keep **Stopped**, **Starting** and **Running**. Failed, blocked, untrusted, disabled and invalid states are unchanged for all tools. This is card presentation only: API `status` values and the process status in Details stay unchanged.

For background services, **Start with vosu** in Details and the Settings dialog overrides the author's `startup`. The host persists `autostart: true|false|null` (`null` follows the manifest). Effective startup is true only for background services with override true, or no override and `startup = "always"`. Enabled, trusted services start dependencies first after the daemon listeners bind; service readiness never delays overlay serving. Changing the override applies at the next vosu launch and does not start or stop a running service.

## Process contract

vosu starts a service with the tool directory as working directory, stdin piped, stdout/stderr captured, and this environment (all other inherited variables are kept):

| Variable | Meaning |
| --- | --- |
| `VOSU_TOOL_PROTOCOL` | `1` |
| `VOSU_TOOL_ID` | The tool `id` |
| `VOSU_TOOL_TOKEN` | Per-launch random secret (64 hex characters). Never log it or send it to a browser. |
| `VOSU_URL` | Base URL of the vosu overlay listener reachable from this machine, e.g. `http://127.0.0.1:24050`, no trailing slash |
| `VOSU_TOOL_DATA_DIR` | Durable per-tool directory (created). Kept when the tool is disabled. |
| `VOSU_TOOL_CACHE_DIR` | Evictable per-tool directory (created). The dashboard may clear it while the tool is stopped. |
| `VOSU_TOOL_INTERFACE` | `service` for supervised HTTP services; `cli` for a command-line invocation |

Readiness: the service binds a TCP listener on `127.0.0.1` port `0`, then writes exactly one line `vosu-tool-ready <port>` to stdout. vosu waits up to 20 seconds; afterwards it probes `GET /healthz` (with the token header) and expects 2xx. Other stdout/stderr lines go to a bounded per-tool log shown in the dashboard.

Shutdown: vosu closes the service's stdin; the service must exit within 5 seconds, after which vosu kills it. Services must also exit when stdin reaches EOF, so they never outlive vosu. A status-0 exit observed before vosu initiates stop, restart, shutdown, or stdin closure means the user stopped the service (for example through its tray): status becomes `stopped`, `status_detail` is `exited`, and neither the restart count nor crash history increases. Even `always` services remain stopped until explicitly started from the dashboard or vosux, or until the next vosu launch. This also applies before readiness. Nonzero exits, signal exits, and failed readiness/health checks still restart with backoff (1, 2, 4, 8, 16 s); five crashes within five minutes mark the service `failed` until a manual start. A status-0 exit caused by vosu closing stdin after a failed handshake is not a voluntary exit.

Overlay reads and dependency requests do not resurrect a voluntarily stopped service. A request authenticated by the tool's own vosux CLI session is an explicit start request; the dashboard Start action also clears the stopped state.

Every request vosu forwards carries `X-Vosu-Tool-Token: <VOSU_TOOL_TOKEN>`. Services must reject (401) requests without the exact token: other local programs can reach the loopback port. vosu also sets `X-Vosu-Caller` to `overlay`, `ui`, or `tool:<id>` (informational).

## HTTP surface on the overlay listener

The `/tools` path prefix is reserved on the overlay listener: overlays cannot be installed, served, or configured under `tools`.

| Route | Behavior |
| --- | --- |
| `GET /tools` | 302 to the dashboard Tools page |
| `/tools/<id>/api/<rest>` | Proxied to the service as `/<rest>` with query string, method, body (≤ 1 MiB), and WebSocket upgrades on `read` routes. Hop-by-hop headers, cookies, and `X-Vosu-*` request headers are stripped and re-set by vosu. On-demand services are started and the request held until ready. 503 when the tool is disabled, untrusted, failed, or blocked. |
| `/tools/<id>/<path>` | Static file from `[ui].root` (root-confined, traversal-safe); `/tools/<id>/` serves `entry`. `/tools/<id>/_icon` and `/tools/<id>/_banner` serve the manifest art in any status. 404 for service-only tools. |
| `GET /tools/<id>/_settings` | `{"values": {"<setting id>": value}}`: effective `settings.json` values with defaults applied, for the tool's UI. Read-only and open like other tool reads (overlays can read it too), so `password` settings are never included. 503 while the tool is unavailable. |
| `/tools/_host/v1/*` | Host API and browser bridge (below) |
| `GET` / `HEAD /files/skin/` | Current-skin metadata retains `client`, `name`, `files:boolean`, `builtin`, `base` and adds `entries:[{path,size,modified?,hash?}]`, `truncated:boolean`. Relative paths use `/`; stable entries have mtime milliseconds (`modified`), lazer entries have store `hash` instead. Built-ins have no entries. At most 20,000 files / 40,000 visited directory entries; incomplete enumeration sets `truncated:true`. Cached by skin identity/root, validated against filesystem changes on metadata reads. Same overlay peer policy/CORS; no absolute paths. |
| `GET` / `HEAD /files/skin/<path>` | Current stable/imported lazer skin file, case-insensitive on both clients and confined to its selected root. Exact stable paths win; complete-index case collisions choose the lexicographically smallest path. Truncated/new paths fall back to exact-then-lexical component lookup. Missing/unavailable lazer files return 404. Built-ins report `files:false` and `builtin` (`argon`, `argon-pro`, `triangles`, `legacy`, `retro`); use your own defaults. |
| `GET` / `HEAD /files/beatmap/*` | Existing current beatmap/background/audio and relative file reads, with unchanged readiness, range, peer and CORS behaviour. |

Skin/beatmap file responses provide strong `ETag`, representable `Last-Modified`
and `Cache-Control: no-cache`. Lazer ETags are store hashes; stable ETags hash path,
size and high-resolution mtime. Matching `If-None-Match` returns bodyless 304
(weak comparison, lists and `*` supported) and takes precedence over
`If-Modified-Since`. HEAD has full-file headers and no body; it ignores Range.
The skin metadata report also has a content-derived ETag. A truncated index must
not be treated as an exhaustive absence check; retain file probes as a fallback.
Cache validation compares bounded directory membership (names and entry types)
as well as file size/mtime; directory timestamps alone are not authoritative.
This also applies to the parent chain used for case-insensitive filename lookup.

Access rules for `/tools/<id>/api/*`:

- `read` routes: GET/HEAD/WebSocket from anyone permitted by the overlay listener's peer policy; CORS `*` like other overlay read APIs. Matching a `write` prefix does not change the open-read rule.
- Mutating methods on `write` routes, and GET/HEAD on write-only routes: require `X-Vosu-Tool-Session` (a UI session for this tool) or `X-Vosu-Tool-Token` of a running tool that lists this tool in `[[requires]]`. Otherwise 403. Methods other than GET/HEAD never use `read` alone; WebSocket upgrades require a read prefix.
- A dependent tool presenting its token gets both `read` and `write` routes of its declared dependencies.
- Any other valid tool or CLI token may use `read` routes, so a tool can call another tool as an optional dependency (without `[[requires]]`, a missing tool never blocks it); `X-Vosu-Caller` then names the calling tool. An invalid or revoked token is rejected with 403 rather than downgraded.

## Host API (for services)

Base `VOSU_URL/tools/_host/v1`, authenticated with `X-Vosu-Tool-Token`. Public overlay APIs (`/json/v2`, `/websocket/v2/subscribe`, `/files/beatmap/*`, `/api/calculate/pp`) need no token.

| Route | Permission | Response |
| --- | --- | --- |
| `GET /self` | none | `{"id","version","data_dir","cache_dir"}` |
| `GET /settings` | none | Effective settings object `{ "<setting id>": value }` with defaults applied |
| `GET /context` | `context` | `{"client": "stable"\|"lazer"\|null, "songs_dir": abs path\|null, "osu_dir": abs path\|null, "skin": null\|{"name": string, "folder": abs path\|null, "files": bool, "builtin": string\|null}, "beatmap": null \| {"checksum","mode","folder","file","audio","background"}}`; all values come from the selected instance. `osu_dir` is the stable installation directory (null for lazer); `skin.name` is the current skin and `skin.folder` is stable's `Skins/<name>` (null for lazer). `skin.files` indicates accessible current skin files; `skin.builtin` identifies a built-in lazer skin (otherwise null). With no selected instance both `osu_dir` and `skin` are null. `checksum` is the .osu MD5; `mode` is `"osu"\|"taiko"\|"fruits"\|"mania"` including converts. Beatmap paths are absolute; audio/background may be null. |
| `GET /overlays` | `context` | `{"overlays":[{"id","name","nickname","alias","author","version","folder","width","height"}]}`: installed overlays that have an `index.html`, sorted by display name (`nickname`, else `name`, case-insensitive). `id` is the folder path relative to the overlays folder, the same id `/api/counters` uses; `name` is metadata.txt `Name`, else the folder name; `nickname` and `alias` are the dashboard's names for it (string or null); `author` and `version` come from metadata.txt as `/api/counters` reports them (an empty `Author` is null; a missing `Version` reads `"1.0"`) and are null for folders without metadata.txt; `folder` is the absolute canonical directory containing `index.html`; `width`/`height` are metadata.txt `Resolution: WxH` when positive, else null. Discovery follows the overlay catalog: a top-level directory symlink is a mounted overlay (its `folder` is the target), nested symlinks and symlinked `index.html` files are not followed, and `ignore.txt`, `node_modules` and dot-folders are skipped. Read-only; settings values are never included. |

The context's `skin` object also includes additive `base:string|null`, with the
same rendering-family semantics as `/files/skin/`. Rendering state and selected
mods are available through the public [v2 renderer fields](../../README.md#renderer-state-v2);
no service token is needed to subscribe. A valid pointer to a missing optional
field reports per-pointer absence rather than rejecting the subscription.

Subscriptions request native sampling by selected field, not by connection count.
`/client`, `/folders/skin`, `/settings/mode` and `/beatmap/checksum` leave configured
reader intervals unchanged. Fast gameplay selectors accelerate only their producing
lane while playing; `/beatmap/time/live` can accelerate the precise song-select
clock. Parent selectors inherit descendant demand (`/settings` includes bass
density), so follow narrow leaves when tracking ruleset or file identity. Frame
debounce and latest-only backpressure are unchanged; see the
[sampling table](../../README.md#renderer-state-v2).


`songs:write` is advisory: it declares that the service writes into the osu! songs directory and is shown at approval. Host permissions limit what vosu APIs return; they do not sandbox the process, which runs with the user's OS rights. Approving a user tool is an explicit trust decision.

## Browser bridge (for UIs)

UIs load `<script type="module">` from `/tools/_host/v1/bridge.js` and link `/tools/_host/v1/theme.css`: the dashboard's design tokens and fonts followed by the shared 18-LAB components (`.v-app`, `.v-bar`, `.v-panel`, `.v-btn`, …). Its rules, copy guidelines and class reference are in the [design system guide](design.md); every component is shown in both modes at `/tools/_host/v1/design`.

```js
import { vosu } from "/tools/_host/v1/bridge.js";
await vosu.ready;            // exchanges a launch code from the URL fragment
vosu.tool;                   // { id, name, version }
vosu.colorScheme;            // "dark" | "light": the dashboard's mode at launch, else the system preference
vosu.canWrite;               // true when this tab holds a UI session
vosu.dashboardUrl;           // dashboard origin, for "Settings" links
await vosu.fetch("v1/profile");          // relative to /tools/<id>/api/, adds the session header
const stop = vosu.subscribe(fields, onFrame, onStatus);  // /websocket/v2/subscribe, reconnecting; stop() ends it
await vosu.settings();                   // GET /tools/<id>/_settings → values (no password settings)
```

`vosu.subscribe(fields, onFrame, onStatus?)` keeps one WebSocket to `/websocket/v2/subscribe` and sends `fields` (JSON-pointer selectors such as `"/beatmap"`) each time it connects. When vosu restarts or the connection drops, it reconnects with exponential backoff: 0.5 s doubling to a 30 s ceiling, each delay randomized between half and all of its step. The step resets only after a connection stayed open for 10 s, so a rejected subscription (its error frame is passed to `onFrame`) is retried ever more slowly; the browser's `online` event retries immediately. It never holds two sockets for one subscription. `onFrame` receives each parsed frame; frames that are not JSON are skipped. The optional `onStatus(state)` is called with `"connecting"`, `"open"`, or `"closed"` whenever the state changes, so a UI can show connection state; each new connection starts with a full snapshot, so no state is lost. The returned function closes the socket and stops reconnecting (reporting `"closed"`). Tools should not wrap it in their own reconnect loop. Older bridges ignore `onStatus` and do not reconnect.

`vosu.colorScheme` is read once when the bridge loads: the launch fragment's `theme=` value when present, otherwise `prefers-color-scheme`. The bridge applies it as `data-theme` on `<html>`, so `theme.css` renders in the dashboard's mode and sets the matching CSS `color-scheme` for native controls (select popups, scrollbars, pickers). A tool may change `data-theme` afterwards. Older bridges lack the property; tools can fall back with `vosu.colorScheme ?? (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")`.

Launch: the dashboard's **Open** calls `POST /api/tools/<id>/launch` (dashboard origin, CSRF) and opens the returned URL `<overlay origin>/tools/<id>/#vosu-launch=<code>&theme=<dark|light>` in a new tab without an opener. The code is single-use and expires after 60 seconds; the bridge exchanges it via `POST /tools/_host/v1/session` `{"tool","code"}` → `{"session"}`, keeps the session only in the page's JavaScript memory, and strips the fragment. Never persist it to `sessionStorage`, `localStorage`, IndexedDB, or cookies: those are shared with every overlay that later loads in the same tab. Sessions last until vosu restarts. A UI opened without a launch code, or reloaded, is read-only until opened from the dashboard again. Tool UIs must not open same-origin pages without `noopener` or embed overlays in frames, because such documents can script the tool page and use its session.

## Dashboard API (dashboard listener only)

All POSTs require dashboard provenance and `X-Vosu-CSRF`, like other management mutations.

| Route | Purpose |
| --- | --- |
| `GET /tools` | Tools page |
| `GET /api/tools` | `{"tools": [ToolView]}` |
| `GET /api/tools/<id>` | `ToolView` |
| `GET /api/tools/<id>/logs?after=<seq>` | `{"lines": [{"seq","ts","stream": "stdout"\|"stderr"\|"host","text"}]}` (bounded, newest 1000) |
| `POST /api/tools/<id>/{enable,disable,start,stop,restart,trust,launch,open-data,clear-cache}` | Lifecycle and data actions; `launch` returns `{"url"}` |
| `PUT /api/tools/<id>/autostart` | `{"autostart":true\|false\|null}` → updated `ToolView`; background services only (400 otherwise), dashboard provenance and CSRF required |
| `GET`/`POST /api/tools/<id>/settings` | Same request/response shapes as `/api/counters/settings/<name>` without profiles, except `password` settings: GET returns them as `""` (also in the schema's default) plus `"secrets": {"<id>": true\|false}` telling whether a value is in effect; POST keeps the stored value for `""` or an omitted field and clears it for `null` |

`ToolView`:

Alongside the legacy `kind`, every view includes `service_role` (`"background"`, `"backend"` or null), `interfaces` (ordered subset of `["ui","service","cli"]`; `service` only for background services), persisted `autostart` (boolean or null), and effective `starts_with_vosu` (boolean). Start/stop/restart mutations reject backend services with 400. Backend logs remain available.

```json
{
  "id": "skill-tracker", "name": "Skill Tracker", "version": "0.1.0",
  "description": "…", "author": "18-LAB", "homepage": null,
  "icon_url": "http://127.0.0.1:24050/tools/skill-tracker/_icon", "banner_url": "http://127.0.0.1:24050/tools/skill-tracker/_banner",
  "source": "official", "update_available": false, "path": "…/tools/skill-tracker", "trusted": true, "enabled": true,
  "kind": "hybrid", "startup": "always",
  "status": "running", "status_detail": null,
  "ui_url": "http://127.0.0.1:24050/tools/skill-tracker/",
  "cli": {"usage": "vosux skill-tracker", "summary": "Profile and play history"},
  "requires": [{"id": "difficulty", "version": ">=0.1.0", "satisfied": true}],
  "required_by": [],
  "permissions": {"host": ["context"]},
  "api": {"read": ["/v1/profile"], "write": ["/v1/import"]},
  "has_settings": true,
  "data": {"data_dir": "…", "cache_dir": "…", "data_bytes": 12345, "cache_bytes": 0},
  "process": {"pid": 1234, "started_at": "2026-10-21T04:00:00Z", "restarts": 0}
}
```

`source` is `dev`, `official`, or `user`. `update_available` compares the installed version with the last fetched catalog. `kind` is `ui`, `service`, `hybrid` (UI and service), or `cli` (CLI only). `status` is one of `stopped`, `starting`, `running`, `failed`, `blocked` (a dependency is missing, disabled, or unsatisfied), `untrusted`, `disabled`, `invalid` (manifest error; `status_detail` explains). Tools without a service report `running` when enabled; CLI processes are owned by their launchers, not supervised services.

Starting a tool first starts its service dependencies. Dependency cycles make every tool in the cycle `invalid`.

### Catalog sources and installation

Settings / **Catalog sources** manages an ordered list of enabled or disabled sources. A source is `owner/repo`, `owner/repo@tag`, or an HTTPS URL ending in `/catalog.json`. Repository shorthand resolves to GitHub `/releases/latest/download/catalog.json` or `/releases/download/<tag>/catalog.json`. Defaults, in order, are exactly `18-lab-dev/vosu-tools` and `18-lab-dev/vosulay`; both may be disabled, removed, or reordered. The list is atomically persisted in `<vosu config directory>/tools/catalog-sources.json`. The private overlays repository is not a default source or a published catalog; overlays continue to be available through the existing tosu community catalog.

At startup, exact former-organization official shorthand identities migrate to the new handles in saved lists and installed tool/overlay state. Tagged, differently cased, HTTPS, and other user-added sources stay unchanged. An untouched old three-entry default list becomes the two current defaults. The earlier two-entry default list also becomes the current defaults unless `catalog-sources-v2.json` records an explicit Settings edit. Order and enabled flags in customized lists are preserved, including a deliberately retained private overlay source; such a source is unreachable and can be removed in **Settings / Catalog sources**. A three-entry list with the exact former defaults in their original order and all enabled is considered untouched even if the earlier migration marker exists. The migration is idempotent; there is no separate catalog URL override.

`GET /api/tools/catalog/sources` returns `{"sources":[{"source","enabled","url","status","last_error","fetched_at"}]}`. CSRF-protected `POST` to the same endpoint replaces the list with `{"sources":[{"source":"owner/repo","enabled":true}]}`. `GET /api/tools/catalog` fetches all enabled sources, returning merged `tools`, `overlays`, source fetch results, and `shadowed` entries. Earlier sources win each tool/overlay id; later entries are displayed as shadowed and cannot be installed. Entries carry `source`; tool views also include `installed`, `managed`, `installed_version`, `supported`, and `update_available`. Catalogs may contain tools, overlays, or both; dependencies resolve across enabled sources.

Catalog tool and overlay entries may carry optional `icon_url` and `banner_url`, absolute HTTPS URLs to the same art the manifest's `icon` and `banner` describe, so Available cards show it before installation. vosu validates them like package URLs (a non-HTTPS or credentialed URL rejects the catalog) and passes them through unchanged. First-party packaging publishes them as release assets named `vosu-tool-<id>-icon.<ext>` and `vosu-tool-<id>-banner.<ext>`.

The dashboard loads catalog art (`icon_url`, `banner_url`, and an overlay's `preview`) through `GET /api/tools/catalog/art?url=<URL>` on the dashboard listener, because release downloads usually redirect to storage that answers `application/octet-stream`, which browsers do not render as SVG. vosu fetches only a URL that the loaded catalog lists exactly as written (anything else is `404`), using the catalog download client: HTTPS only, no downgrade, public destinations only (also after each of at most 5 redirects), at most 1 MiB. The type comes from the content (SVG, PNG, WebP, or JPEG; anything else is `502`), and a file extension on the listed URL must agree; the upstream `Content-Type` is ignored. Results are cached in memory per catalog revision (at most 256 images and 16 MiB; a changed catalog starts over, since a release can serve new bytes at the same URL). Every response, including errors, carries `Content-Security-Policy: sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:`, `X-Content-Type-Options: nosniff`, `Cross-Origin-Resource-Policy: same-origin`, and `Content-Disposition: inline`, and sets no cookies: opened directly, an SVG is an inert document with an opaque origin. Cards fall back to initials, or no banner, when art fails. Installed tools' art keeps coming from `/tools/<id>/_icon` and `/_banner` on the overlay listener.

Only tools from the exact default source identities `18-lab-dev/vosu-tools` and `18-lab-dev/vosulay` are installed as `official` and trusted automatically. Trust follows the exact source string, not its position or resolved URL: removing and re-adding either identical shorthand retains official status. These entries resolve through the same repository resolver as user sources. Tagged, differently cased, and HTTPS URL spellings are not aliases for trust, even when they resolve to the same repository. This deliberately narrow allowlist avoids URL/redirect-based trust expansion and does not pretend a user-editable entry has authenticated creation provenance. All other catalog tools are `user` and remain untrusted until approved, just like manual installations. Size/digest verification establishes package integrity, not author trust. Catalog-installed tools cannot overwrite development tools or manually installed directories.

Only `test-harness` builds accept source URLs at `http://tools.invalid:<port>/…/catalog.json` when `VOSU_TOOL_CATALOG_TEST_SOCKET` names that exact loopback listener; production builds contain no HTTP/loopback exception.

`POST /api/tools/<id>/install`, `/update`, and `/uninstall` use the dashboard's normal CSRF protection. Uninstall requires JSON `{"remove_data":false}` to keep data or `{"remove_data":true}` to remove it; installed dependents must be removed first. Each action returns `{"job_id":"…"}`. Poll `GET /api/tools/catalog/jobs/<job_id>` for `{id,status,phase,tool,error}`; status is `running`, `completed`, or `failed`.

The dashboard's Available page includes source overlays alongside the unchanged community catalog, with its existing third-party disclosure. `POST /api/tools/catalog/overlays/<id>/install` or `/update` verifies package size/digest and archive safety, requires root `index.html`, and atomically installs `<static folder>/<id>`. The body `{"install_tools":true}` explicitly accepts installing missing `requires_tools` first; absent approval the job fails rather than silently running extra programs. Tool approval remains separate. Overlay setting values live outside the replaced directory and survive updates. Remove overlays through the existing installed-overlay delete action.

`vosux --install <id>`, `--update <id>`, and `--uninstall <id> (--keep-data|--remove-data)` use broker-authenticated overlay endpoints `POST /tools/_host/v1/cli/catalog/<id>/<action>` and `GET /tools/_host/v1/cli/catalog/jobs/<job_id>`, with the same bodies/results. No dashboard credential is disclosed.

`vosux --sources` lists configured sources through broker-authenticated `GET /tools/_host/v1/cli/catalog/sources`. Install/update/uninstall CLI options remain tool-only.

`vosux --autostart <id> on|off|default` saves the same override through broker-authenticated `PUT /tools/_host/v1/cli/<id>/autostart`, with `{"autostart":true|false|null}`. It accepts only background services.

### Catalog and package format

This is the consumer contract for third-party publishers as well as official tooling.
For an end-to-end publishing example, see [Create a tool](create-a-tool.md#12-package-and-publish).

```json
{
  "catalog_version": 1,
  "name": "Example tools",
  "published": "2026-10-24T00:00:00Z",
  "tools": [{
    "id": "hello-python",
    "name": "Hello Python",
    "version": "0.1.0",
    "description": "A dependency-free counter. Requires Python 3.11+ on PATH.",
    "author": "Your name",
    "protocol": 1,
    "interfaces": ["ui", "service", "cli"],
    "requires": [],
    "packages": {
      "any": {
        "url": "https://example.org/releases/v0.1.0/vosu-tool-hello-python-0.1.0-any.zip",
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "size": 12345
      }
    }
  }]
}
```

The digest and size above illustrate the fields; publishers must compute them from
the actual ZIP bytes. `catalog_version`, `published`, and each tool's `id`, `name`,
`version`, `description`, `author`, `protocol`, `interfaces`, and `packages` are
required. `name` on the catalog, `tools`, `overlays`, and tool `requires` default to
empty when omitted. `homepage`, `icon_url`, and `banner_url` on a tool are optional.
Protocol is `1`; interfaces must be a nonempty list drawn from `ui`, `service`,
`cli`. IDs must be valid and unique, versions semver, and dependency version
requirements valid semver requirements. `requires` mirrors the manifest's required
dependencies (not optional read integrations), as `{"id","version"}` entries
in the same order and with the same version-requirement strings, so installation
can resolve them across enabled sources. Dependency cycles, missing
dependencies, and incompatible versions fail installation.

Package keys are `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`, or `any`
for platform-independent code. A matching native package takes precedence over
`any`; no matching package means unsupported. `any` does not install interpreters:
state Python or other runtime requirements in the description and release notes.
Each package needs an absolute HTTPS `url` without embedded credentials, a
64-character hexadecimal SHA-256 of the compressed ZIP, and its exact positive
byte `size` (at most 512 MiB). Artwork URLs use the same HTTPS/credential rules.
Downloads also reject private/reserved destinations and unsafe redirects.

Each tool ZIP has **`tool.toml` at the archive root**, not inside an enclosing
directory. Include the files it references, runtime code, `settings.json` when
used, and redistributable license texts. The manifest id and version must equal
the catalog entry. The conventional filename is
`vosu-tool-<id>-<version>-<target>.zip`; the host uses the URL, not that spelling.
Extraction allows only regular files/directories, confined relative paths, at
most 10,000 entries and 512 MiB expanded; absolute paths, traversal and symlinks
are rejected. Unix files directly under `bin/` receive executable permissions.
Installation stages beside `<vosu data directory>/tools/<id>/`, then atomically
replaces it; durable tool data is outside that package directory.

An overlay entry instead requires `id`, `name`, `version`, `description`, `author`,
and one `package` object with the same URL/digest/size fields. It may include
`homepage`, `resolution: [width,height]`, `preview`, `icon_url`, `banner_url`,
and `requires_tools: [{"id","version"}]`. Its ZIP has `index.html` at the root.
Overlays install into the static folder; settings values survive package updates.
First-party build/licensing/artwork policy and platform-catalog merging are
documented in [vosu-tools packaging](https://github.com/18-lab-dev/vosu-tools/blob/main/docs/catalog.md);
those publisher-specific build conventions are not additional host requirements.


## Conventions for tool APIs

These are not enforced, but first-party tools follow them so overlays and other tools can rely on the same shapes:

- Version every route under `/v1/`; add `/v2/` beside it rather than changing `/v1/` shapes.
- Errors are JSON `{"error": "<message>"}` with a 4xx/5xx status.
- Work longer than about a second is a job: `POST` returns `202 {"job": "<id>"}`, `GET /v1/jobs/<id>` returns `{"state": "queued"|"running"|"done"|"failed"|"cancelled", "progress": 0.0..1.0, "message", "result"}`, and `DELETE /v1/jobs/<id>` cancels.
- Live data is a WebSocket on a `read` route that sends a full JSON snapshot on connect and on every change.

## Command-line tools (vosux)

`vosu x [OPTIONS] <tool> [ARGS...]` and `vosux [OPTIONS] <tool> [ARGS...]`
use the same executable. `vosux` is argv[0] dispatch, case-insensitive on Windows
with `.exe` stripped, not another binary. Linux distro packages install a sibling
`vosux -> vosu` symlink; Windows ZIPs stay single-executable. The launcher performs
no daemon initialization, logging, listener, tray, updater, or single-instance work.

Options are recognized only before the tool id: `--list` (also the default with
no tool), `--list --json`, `--help`, and `--open <tool>`. Every argument after the
id, including `--help`, `--`, whitespace, and option-like text, goes verbatim to
the child without a shell. A tool with `[ui]` but no `[cli]` opens its UI when
invoked without arguments; arguments instead produce usage error 2.

`[cli].command` is a nonempty argv array, resolved exactly as `[service].command`,
including development Cargo fallback. `summary` is optional and cannot contain
NUL or line breaks. CLI-only tools are allowed. ToolView always includes `cli`:
`null` or `{"usage":"vosux <id>","summary":"…"}` (summary may be null).
The dashboard shows that usage and summary.

The launcher discovers `origin` and `token` through the per-user
[`broker.json`](../../README.md#native-overlay-brokers). The following overlay
routes require a transport-observed loopback peer and
`Authorization: Bearer <native broker token>`, even for reads:

| Route | Response |
| --- | --- |
| `GET /tools/_host/v1/cli` | `{"tools":[{"id","name","version","status","cli","ui":true,"trusted":true,"enabled":true}]}` |
| `POST /tools/_host/v1/cli/<id>` | `{"session":"opaque id","argv":["absolute executable","…"],"cwd":"tool directory","env":{…}}` |
| `DELETE /tools/_host/v1/cli/session/<session>` | Revokes the session (idempotent) |
| `POST /tools/_host/v1/cli/<id>/open` | `{"url":"…"}` with the same single-use launch code as dashboard Open; optional JSON `{"theme":"light"}` or `"dark"`, default dark |

Unknown tools return 404; no CLI returns 409; untrusted, disabled, invalid or
dependency-blocked tools return 403 with `error` and `status`. Session exhaustion
returns 503. Each session has a fresh 256-bit token; at most 64 live sessions
coexist and expire after 24 hours or daemon restart. Disable/replacement and
explicit revocation invalidate tokens. Expired/revoked tokens fail host API
authentication (401) and proxy authority checks (403).

The returned environment contains `VOSU_TOOL_PROTOCOL`, `VOSU_TOOL_ID`,
`VOSU_TOOL_TOKEN`, `VOSU_URL`, `VOSU_TOOL_DATA_DIR`, `VOSU_TOOL_CACHE_DIR`, and
`VOSU_TOOL_INTERFACE=cli`. Merge it over the launcher's current environment.
CLI tokens have their tool's permission-filtered host API access, read/write
access to declared dependencies, read access to other tools' `read` routes, and
read/write access to their own declared `/tools/<id>/api/` routes. Writes to
undeclared tools are refused. The proxy
sets `X-Vosu-Caller: cli:<id>` and replaces the token with the target service
credential, retaining the service as the single writer of shared state.

The child runs directly in the returned cwd with inherited stdin/stdout/stderr.
After child exit (or spawn failure), the launcher best-effort revokes the session.
Exit status is the child's; Unix signal exits map to `128 + signal`.
Launcher errors: 2 usage, 69 daemon unavailable (missing/stale broker file or
connection refused), 77 tool not runnable, 127 unknown tool, 1 other.
Unix children stay in the foreground process group: terminal Ctrl+C/Ctrl+\\
reach the child directly while vosux waits; SIGTERM/SIGHUP are forwarded to the
child. Windows children share the console; a temporary vosux console handler
consumes Ctrl+C/Ctrl+Break in the launcher only, letting the child decide.


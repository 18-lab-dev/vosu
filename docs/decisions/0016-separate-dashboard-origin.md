# ADR 0016: Separate the dashboard origin without moving overlays

Status: accepted. Supersedes the shared-origin part of [ADR 0013](0013-counters-share-dashboard-origin.md).

## Context

Installed overlays are third-party JavaScript. Serving them on the dashboard origin grants them management authority despite exact-origin CSRF checks. Moving overlays would strand existing localStorage and IndexedDB, whose keys include the scheme, hostname, and port.

## Decision

Keep overlays on `SERVER_IP:SERVER_PORT` (default `127.0.0.1:24050`), including the existing localhost alias. Move trusted pages and management APIs to an independent `DASHBOARD_IP:DASHBOARD_PORT` listener (default `127.0.0.1:24051`).

Use a port rather than `dashboard.localhost`: ports work remotely and with every resolver/browser. A `.localhost` name would point at the remote user's own machine and relies on browser/resolver special handling. Neither design would justify changing overlay storage origins.

### Route boundary

| Origin | Routes |
| --- | --- |
| Overlay | Overlay static files and COUNTER_PATH injection; JSON, calculation, game files, and state WebSockets; `/websocket/commands` and its unchanged settings protocol; native broker and scoped API-key settings/profile writes; read-only profiles/settings/catalog APIs; `GET /api/dashboard`. |
| Dashboard | `/`, `/available`, `/settings` (including profile query parameters), embedded dashboard assets, read APIs used by dashboard pages, and every management mutation: install/delete/open/update, daemon settings, reports, schema edits, profile management, and key management. No overlay static files. |

Overlay GETs to dashboard pages return 302 with path and query preserved. Redirect authorities come from the effective dashboard listener; only a configured hostname or safe loopback alias can be retained from Host. Wildcard binds map to loopback for local dashboard discovery. Previews use the overlay origin. `/api/dashboard` publishes the effective dashboard origin to integrations such as vosulay; native editors use HTTP APIs rather than framing dashboard pages.

Dashboard mutations require trusted peers, exact dashboard-origin provenance, and a per-start random `X-Vosu-CSRF` token injected only into dashboard pages. Overlay-origin browser management writes fail with 403, even with overlay-origin provenance. Existing explicitly scoped key grants and command-socket settings writes remain compatible. Tray/startup/second-instance links use the dashboard listener. Both listeners rebind independently; a failed rebind retains its preceding effective listener.

Every dashboard request also checks Host against the effective safe origin, and the
dashboard listener refuses all WebSocket upgrades. Dashboard HTML uses a CSP that
allows only same-origin script files, with runtime context carried in inert JSON,
plus `X-Content-Type-Options: nosniff`. Every dashboard page sends
`frame-ancestors 'none'`; no query parameter enables framing. Native editors use
the settings/profile HTTP API through the native broker credential instead.

Network edits made in the dashboard are staged by an explicit Apply. Only one edit may be pending. Confirmation must arrive from the new dashboard origin within 30 seconds; an independent server timer restores only the previous network fields if not confirmed, even after a closed page. Other settings continue autosaving. CIDR allowlists support precise IPv4/IPv6 network ranges alongside legacy wildcard octets. `absolute` is retained as a literal legacy hostname token, never a universal peer grant.

The config-adjacent `vosu.network.json` journal is written before changing network
values. Only the owning runtime recovers unconfirmed changes, after updater handoff
and live-owner checks but before listener startup; orderly owner shutdown also
rolls back. A second launch reads the pending dashboard address without recovering
or cancelling it. A failed rollback retains its pending guard and is retried.
Keep requires both effective listeners to match the requested targets. The journal
retains previous and candidate overlay endpoints, including rolled-back candidates:
a dashboard bind must not overlap any current or recorded overlay origin.
The current dashboard endpoint also cannot become a candidate overlay endpoint,
so an automatic rollback cannot restore the dashboard onto a temporary overlay origin.
Browser rollback destinations come only from the Apply request's verified origin,
preserving remote access to wildcard binds, never from URL parameters.
Cross-origin discovery probes carry no CSRF secret.
Apply/revert navigations omit Referer, so a previous dashboard address need not be
independently allowed as an overlay hostname to load the destination dashboard.

## Consequences

Overlay URLs and browser storage stay unchanged. Dashboard storage moves to its new origin; no overlay receives the dashboard token. Users exposing overlays on a LAN do not implicitly expose the loopback dashboard. Native same-account clients can still read dashboard pages and forge provenance: this boundary isolates browser origins, not hostile local programs. Overlay scripts still have existing read access and their settings command protocol, so this is not a general-purpose overlay sandbox.

## Amendments

**2026-11-24** (A7, M7 contract freeze; full text in the
[A7 freeze](../v2/implementation-plan.md#a7-approved-contract-freeze-2026-11-24)):

- **Route boundary in 2.0 (A7.1).** The origin split stands. Bundled hosts
  register exact routes on a listener role, and core keeps the boundary:
  management routes stay dashboard-only, with dashboard provenance and
  `X-Vosu-CSRF` checked by core before any effect. The CSRF secret reaches
  dashboard pages only through core's page-context injection, never a host.
  The dashboard listener serves no API v1, state, game-file, overlay-static,
  tool or `/websocket/commands` route and still refuses every upgrade.
  Dashboard-only embedded assets are no longer served on the overlay
  listener. The shared design-system assets are, because tool pages and the
  styled 404 page use them.
- **Dashboard off (A7.3).** With `ENABLE_DASHBOARD = false` or
  `VOSU_DASHBOARD=off`, no dashboard listener binds. The overlay→dashboard
  redirects and `GET /api/dashboard` are 404, tray, startup and second-launch
  links open nothing, and the overlay listener, its storage origin and the
  network-Apply journal recovery are unchanged.

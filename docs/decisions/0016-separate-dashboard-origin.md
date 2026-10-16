# ADR 0016: Separate the dashboard origin without moving overlays

Status: accepted. Supersedes the shared-origin part of [ADR 0013](0013-counters-share-dashboard-origin.md).

## Context

Installed overlays are third-party JavaScript. Serving them on the dashboard origin grants them management authority despite exact-origin CSRF checks. Moving overlays would strand existing localStorage and IndexedDB, whose keys include the scheme, hostname, and port.

## Decision

Keep overlays on `SERVER_IP:SERVER_PORT` (default `127.0.0.1:24050`), including the existing localhost alias. Move trusted pages and management APIs to an independent `DASHBOARD_IP:DASHBOARD_PORT` listener (default `127.0.0.1:24052`). Port 24051 is avoided because local tosu test builds use it.

Use a port rather than `dashboard.localhost`: ports work remotely and with every resolver/browser. A `.localhost` name would point at the remote user's own machine and relies on browser/resolver special handling. Neither design would justify changing overlay storage origins.

### Route boundary

| Origin | Routes |
| --- | --- |
| Overlay | Overlay static files and COUNTER_PATH injection; JSON, calculation, game files, and state WebSockets; `/websocket/commands` and its unchanged settings protocol; native broker and scoped API-key settings/profile writes; read-only profiles/settings/catalog APIs; `GET /api/dashboard`. |
| Dashboard | `/`, `/available`, `/settings` (including profile/embed query parameters), embedded dashboard assets, read APIs used by dashboard pages, and every management mutation: install/delete/open/update, daemon settings, reports, schema edits, profile management, and key management. No overlay static files. |

Overlay GETs to dashboard pages return 302 with path and query preserved. Redirect authorities come from the effective dashboard listener; only a configured hostname or safe loopback alias can be retained from Host. Wildcard binds map to loopback for local dashboard discovery. Previews use the overlay origin. `/api/dashboard` publishes the effective dashboard origin to integrations such as vosulay; the embed postMessage payload/behavior is unchanged, but messages now originate from the dashboard origin.

Dashboard mutations require trusted peers, exact dashboard-origin provenance, and a per-start random `X-Vosu-CSRF` token injected only into dashboard pages. Overlay-origin browser management writes fail with 403, even with overlay-origin provenance. Existing explicitly scoped key grants and command-socket settings writes remain compatible. Tray/startup/second-instance links use the dashboard listener. Both listeners rebind independently; a failed rebind retains its preceding effective listener.

Every dashboard request also checks Host against the effective safe origin, and the
dashboard listener refuses all WebSocket upgrades. Dashboard HTML uses a CSP that
allows only same-origin script files, with runtime context carried in inert JSON,
plus `X-Content-Type-Options: nosniff`. Normal pages send `frame-ancestors 'none'`.
The explicit `/settings?overlay=…&embed=1` integration mode permits framing only
with a non-empty first `overlay` and first `embed=1`, matching browser parsing.
Vosulay may run on a separately configured origin. This remains a clickjacking residual:
an untrusted page can frame embed mode and attempt to solicit user interaction.
CSRF and exact-origin mutation checks still apply; embedding is not an additional
authorization grant.

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

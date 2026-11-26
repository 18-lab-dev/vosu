# Migrating from vosu 1.x

This guide collects what changes for people and overlay authors moving from
vosu 1.x to 2.0. The normative contracts are the linked specifications; this
page only points to them.

## Overlays that used removed APIs

The dashboard shows a notice when an overlay, or an unidentified client,
still requests a route vosu 2.0 removed. Each request gets the ordinary
`404 Not Found` that any unknown route gets. vosu does not disable, reinstall
or retry the overlay: it keeps running, and only the removed feature stops
working until the overlay is updated. The notice names an overlay only when
vosu can tell which installed overlay sent the request (its `l=` connection
name or its own page path). Notices are kept in memory for 24 hours after the
last request and are cleared when vosu restarts
([adapter surfaces A4.5 and A7.4](adapter-surfaces.md#a45-removed-route-attribution)).

If you use the overlay, check its author's page or the catalog for a 2.0
release. If you wrote it, replace the removed request as below.

| Notice | Removed request | Use instead |
| --- | --- | --- |
| Selected-field live feed | `WS /websocket/v2/subscribe` | API v1 field subscriptions, `WS /api/v1/sessions/{id\|active}/subscribe` ([API v1](api-v1.md#field-subscriptions-ws-apiv1sessionsidactivesubscribe)). Select the fields the overlay shows; high-frequency fields are sampled only while selected, which replaces the separate precise feed. |
| Skin folder listing | `GET /files/skin/` (the index, not individual files) | Skin trees and resource references in API v1 ([resources and trees](api-v1.md#resources-and-trees)). Individual `/files/skin/<name>` files are still served by the upstream-compatible adapter. |
| In-game overlay page | `/api/ingame` (every method) | vosulay, the in-game overlay. The notice offers to install the `vosulay-editor` tool from the `18-lab-dev/vosulay` catalog, through the ordinary install plan and its approvals ([catalog v2 "API placement"](catalog-v2.md#api-placement)). |

The upstream-compatible state routes (`/json/v2`, `/websocket/v2`,
`/websocket/v2/precise`, gosuMemory and streamCompanion) are not removed and
need no change ([API v1 route ownership](api-v1.md#route-ownership-upstream-compatible-adapters)).
New overlays should still prefer API v1: it addresses any game's session, not
only osu!, and its [worked examples](api-v1.md#worked-examples) show a
cross-game counter and a background image read through a resource
reference.

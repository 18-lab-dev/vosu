# Migrating from vosu 1.x

This page is the removed-route reference linked from dashboard notices.
For alpha paths, packages and settings, see
[Migrating from 1.x to 2.0](../migration/2.0.md).

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
| Selected-field live feed | `WS /websocket/v2/subscribe` | API v1 field subscriptions, `WS /api/v1/sessions/{id\|active}/subscribe` ([API v1](api-v1.md#field-subscriptions-ws-apiv1sessionsidactivesubscribe)). Select the fields the overlay shows; high-frequency fields are sampled at their own cadence while selected. API v1 needs no separate precise feed. |
| Skin folder listing | `GET /files/skin/` (the index, not individual files) | Skin trees and resource references in API v1 ([resources and trees](api-v1.md#resources-and-trees)). Individual stable skin files at `/files/skin/<name>` remain in the upstream-compatible adapter; use API v1 for lazer skin files. |
| In-game overlay page | `/api/ingame` (every method) | vosulay, the in-game overlay. The notice offers the official `vosulay-editor` install plan, including the `vosu.vosulay` plugin and normal approvals. [Install the matching native runtime separately](../migration/2.0.md#overlays-and-vosulay), or use vosulay standalone. |

The upstream-compatible state routes (`/json/v2`, `/websocket/v2`,
`/websocket/v2/precise`, gosuMemory and streamCompanion) are not removed and
need no change for upstream fields. vosu-only fields removed from those
adapter payloads have [API v1 replacements](state-schema.md#1x-to-20-field-mapping).
For new overlays, see [Overlay authoring](../overlays/authoring.md) and the
[API v1 examples](api-v1.md#worked-examples).

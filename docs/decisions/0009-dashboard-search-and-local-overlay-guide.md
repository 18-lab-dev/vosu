---
type: Architecture Decision Record
title: Explicit counter search source and no local-overlay guide
description: Accepted decision to remove the /local-overlays page and select counter search results with an explicit source parameter instead of the Referer header.
tags: [architecture, decision, adr, dashboard, compatibility]
---

# ADR 0009: Explicit counter search source and no local-overlay guide

- **Status:** Accepted

## Context

The dashboard sidebar linked to an "Overlay tools" section: open the overlays folder, a `/local-overlays` instructions page, and an external submission form. The sidebar link was the guide's only entry point.

`GET /api/counters/search/{query}` chose between installed counters and the catalog by checking whether the request's `Referer` path was `/available`. Referrer policies, privacy tooling, and non-browser clients can omit or rewrite that header, which silently changes the result set. The dashboard also replaced the whole Installed list with search results, restarting every live preview iframe on each query.

## Decision

- Remove the overlay-tools section and the `/local-overlays` page. The path now falls through to the counter-static route like any other unknown page path. `POST /api/counters/open-root` remains a documented API.
- Select the search result set explicitly: no `source` or `source=installed` searches installed counters, `source=available` searches the catalog, and any other value returns 400. `Referer` no longer affects the result.
- Search failures return 502 with the escaped `Error:` text used by the dynamic pages, instead of an unescaped 200 body.
- The dashboard filters Installed cards in place and uses the endpoint only for Available.

## Consequences

- Clients that relied on `Referer: …/available` to receive catalog results must add `?source=available`; without it they receive installed counters.
- Bookmarks to `/local-overlays` now reach the counter-static fallback.
- Installed search keeps live previews running; client-side matching mirrors the server's normalization in `src/runtime/render.rs`.

## Alternatives considered

### Keep `/local-overlays` unlinked

Rejected because an unreachable page with tests and styles is dead surface.

### Keep `Referer` as a fallback when `source` is absent

Rejected because the default would still depend on a header that browsers may strip, so identical requests could return different lists.

## Related documentation

- [User-facing product and API boundary](../../README.md)
- [Architecture decision record index](index.md)

# ADR 0008: Keep in-game overlays outside vosu

- **Status:** Accepted

## Context

vosu is a native telemetry and counter daemon. It does not launch an in-game overlay, but retaining overlay configuration, an embedded browser page, dedicated storage, and catalog commands blurred the boundary between the daemon and an overlay product. That compatibility shell also kept UI and state-management paths whose only owner was the removed overlay.

In-game overlay software is moving to an independent product. The daemon still needs stable, documented interfaces for counters and external API clients, including software that renders its own overlay.

## Decision

vosu owns its telemetry and counter HTTP and WebSocket APIs, dashboard and counter management, and key-overlay data published through compatible schemas. It does not own or provide an in-game overlay product.

Remove the dedicated in-game overlay surface as a clean cutover:

- remove the in-game overlay configuration bindings and dashboard controls;
- remove `/api/ingame` and its embedded HTML, JavaScript, and CSS;
- remove dedicated `__ingame__` settings storage;
- remove the command WebSocket catalog flow used to populate the embedded overlay.

External in-game overlay software is a separate product and API client. It may consume only vosu's remaining documented interfaces. The generic `/websocket/commands` endpoint remains for counter settings and filter commands, ordinary dashboard and counter “overlay” terminology remains, and key-overlay telemetry remains part of the compatible API schemas.

## Consequences

- Removing the retained compatibility shell is intentionally breaking for clients that depend on its configuration, page, storage, or catalog commands.
- The daemon and embedded dashboard have less product-specific UI, configuration, storage, and command state.
- External overlay software must implement and own its UI and persistence and rely only on vosu's documented HTTP and WebSocket APIs.
- vosu can evolve its telemetry and counter service without implicitly owning a second overlay product.
- Generic counter commands, counter overlays, and key-overlay data remain available because they belong to the daemon's surviving contracts.

## Alternatives considered

### Retain the compatibility shell

Rejected because configuration and browser/storage/catalog paths without an owning in-process overlay preserve ambiguous product ownership and permanent maintenance surface.

### Build a replacement in-game overlay inside vosu

Rejected because it would expand the native telemetry daemon into a separate interactive overlay product with its own UI, lifecycle, configuration, and persistence concerns. Independent software can consume the daemon APIs without coupling those concerns to vosu.

## Related documentation

- [User-facing product and API boundary](../../README.md)
- [Architecture](../architecture.md)
- [Architecture decision record index](README.md)

# Keep installed counters on the dashboard origin

- Status: Accepted
- Date: 2026-10-10

## Context

Installed counters are served from the same origin as the dashboard and its management API, `http://127.0.0.1:24050` by default. A counter's JavaScript can therefore satisfy the exact same-origin check that guards management POSTs and settings saves. It can install, update, delete, or open overlays, save any overlay's settings, change daemon settings, and start an update. Native local clients can also forge `Origin`. The Available page lists third-party overlays, so this boundary matters to users who install them.

Counter URLs on port 24050, counter `localStorage`, and the `/settings?overlay=<name>` link used by vosulay are compatibility contracts shared with tosu overlays and OBS scenes. Three isolation designs were evaluated:

1. **Dashboard on a separate loopback port.** Counters keep their origin and storage. However, this adds a listener and a port setting, moves dashboard pins to a new origin, and stops hosted counters from saving settings over `/websocket/commands`. Native clients can still forge `Origin`.
2. **Separate port plus a launch-code session cookie.** This also blocks native clients and other OS users. However, after every restart, bookmarks and vosulay's settings link land on a "reopen from the tray" page, and headless setups depend on a logged link.
3. **Serve counter HTML with a CSP `sandbox` (opaque origin).** URLs are unchanged, but counters lose `localStorage`, IndexedDB, and cookies, and module scripts and fonts need CORS.

## Decision

Keep counters on the dashboard origin. Management mutations keep requiring exact same-origin browser provenance, as described in the [runbook](../operations/runbook.md#security-hardening-and-remaining-trust). That blocks other websites but not installed counters or native clients. Disclose the shared authority instead of isolating it:

- The **Available** page states that listed overlays are third-party, run their authors' JavaScript with the dashboard's access, and should be installed only when trusted.
- The README, [privacy policy](../../PRIVACY.md), and [third-party notices](../../THIRD_PARTY_NOTICES.md) describe the same boundary.

## Consequences

No overlay, OBS scene, vosulay integration, or dashboard URL changes. An installed counter remains trusted code with management authority, and that must keep being stated wherever counters are installed or described. Revisit this decision when breaking one of those compatibility contracts becomes acceptable; the three designs above are the evaluated starting points.

## References

- [Operations runbook: security hardening and remaining trust](../operations/runbook.md#security-hardening-and-remaining-trust)
- [README: management authorization](../../README.md#pages-counters-reports-and-updates)

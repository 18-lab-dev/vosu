# Keep installed counters on the dashboard origin

- Status: Superseded (shared-origin decision) by [ADR 0016](0016-separate-dashboard-origin.md). Historical rationale below; overlays now retain their origin while the dashboard moves, and command-socket settings writes remain compatible.
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

### Native broker authentication (2026-10-16 amendment)

Overlay software hosting counter frames outside vosu's origin must not forge
`Origin`. Instead, vosu publishes a fresh 256-bit OS-random bearer credential
each daemon start in a private per-user `broker.json`. Loopback
`/websocket/commands` upgrades accept it, with constant-time comparison.
A valid token grants the existing same-origin command-socket mutation permission.
On the overlay HTTP origin it also grants exactly `settings:write` value saves
(`POST /api/counters/settings/<name>[?profile=]`, never `?update=yes`) and
`profiles:manage` (`POST /api/counters/profiles`, all actions). GETs stay open.
Other HTTP mutations, non-loopback peers and invalid/stale tokens return 401.
POST authorization is rechecked after body reading, just like revocable API keys.
Restart rotates the credential; brokers reread the file. No token preserves
existing behavior. Dashboard management still requires provenance and CSRF.

This explicitly trusts same-account native processes that can read the file;
it does not claim to solve the native-client header-forgery limitation above.
It avoids misrepresenting browser provenance while keeping existing counter
message shapes, profiles, and refresh relays unchanged. Brokers must protect
their inbound forwarding boundary and keep the token out of browser JavaScript
and logs. See [README](../../README.md#native-overlay-brokers) for lifecycle,
paths and Windows inherited-ACL residual risk.


## Consequences

No overlay, OBS scene, vosulay integration, or dashboard URL changes. An installed counter remains trusted code with management authority, and that must keep being stated wherever counters are installed or described. Revisit this decision when breaking one of those compatibility contracts becomes acceptable; the three designs above are the evaluated starting points.

## References

- [Operations runbook: security hardening and remaining trust](../operations/runbook.md#security-hardening-and-remaining-trust)
- [README: management authorization](../../README.md#pages-counters-reports-and-updates)

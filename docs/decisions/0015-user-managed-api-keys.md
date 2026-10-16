# 0015: User-managed API keys

Status: accepted

## Context

Trusted sandboxed, remote and browser clients cannot necessarily read the native
broker file or set browser WebSocket headers. They need explicit revocable
settings/profile authority without pretending to originate at the dashboard.

## Decision

Keep the per-start loopback-only broker.json token unchanged. Add persistent
`vosu_k_` credentials with 256 OS-random bits and fixed per-key scopes:
`settings:write` for command-socket writes and overlay value saves (not schemas),
`profiles:manage` for all profile mutations. Neither authorizes updates,
installation/deletion, daemon settings, reports or key management. Management
requires exact same-origin dashboard provenance, including listing metadata.
Names may change; scope changes require revoke and recreate. Revocation cancels
every associated command socket, closing it with 1008 even while idle; it cannot
continue receiving settings relays. Every command also checks its grant, and HTTP
POSTs re-check the granted scope after body receipt and before handler dispatch.

Native clients use Bearer headers; browsers offer `vosu.key` and
`vosu.key.<secret>` subprotocols, selecting only `vosu.key` in the response.
Never authenticate query strings: URLs commonly enter history, referrers,
proxies and logs. Do not add Authorization to CORS allow-headers; cross-origin
browser HTTP is not part of this contract.

Accept keys from remote peers only after the existing ALLOWED_IPS peer gate.
The key replaces browser-origin write provenance only for its explicit routes;
it does not expose other management operations. Plain HTTP/WS on a LAN exposes
the secret to network observers. Default to loopback and recommend a trusted TLS
reverse proxy for remote access; no forwarded header changes the peer policy.

Show each secret exactly once. Store only SHA-256 hashes plus public metadata
in config-root api-keys.toml, using same-directory atomic replacement and 0600
Unix files from creation. A high-entropy random secret does not need a slow
password KDF; hashing prevents accidental disclosure through registry reads,
while constant-time comparisons avoid content-dependent digest comparisons.
A registry writer remains trusted and can replace credentials. Windows inherits
parent ACLs. Limit the registry to 64 keys and last-use writes to one per minute
per key. Invalid files disable keys with a warning, continue serving, and remain
untouched until explicit user key changes. Never log credentials or hashes.
Warn on Unix registry ownership/permissions that expose it to other users, without
rejecting otherwise valid keys. Reject Unicode bidirectional controls in names.

## Consequences

Existing broker clients and same-origin counters remain compatible. Installed
same-origin overlay code still shares dashboard authority under
[ADR 0013](0013-counters-share-dashboard-origin.md); API keys do not isolate it.
Users must distribute keys only to trusted software, protect TLS termination,
and revoke leaked keys. Read APIs retain the existing overlay-compatible policy.
See the [integration and management contract](../../README.md#api-keys).

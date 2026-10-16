# 0014: Overlay settings profiles

Status: accepted

## Context

Users need to switch a complete streaming setup while allowing individual overlay
instances to remain pinned. Existing tosu overlays identify settings through
`COUNTER_PATH` in command frames and their socket's `l` query parameter; adding a
separate profile field to those frames would require modifying third-party code.
Existing values must remain usable without migration.

## Decision

Keep one global active profile and allow a per-instance `vosuProfile` page query.
The reserved `default` profile is exactly the legacy values directory. Store other
profiles under `settings/profiles/<name>/` and names plus active selection in
`settings/profiles.json`. The schema remains in each overlay package. Missing
profile values fall back to schema defaults, not default-profile user values.
Registry and values use same-directory atomic replacement. Management mutations
are serialized; names are ASCII letters/digits/hyphens/underscores, 1–48 bytes,
case-insensitively unique, with at most 32 profiles including `default`.

Without an override preserve the existing HTML suffix byte-for-byte. With an
override, inject `COUNTER_PATH=/@vosu-profile/<profile>/<overlay>` and separately
`VOSU_PROFILE=<profile>`. The leading slash and reserved multi-component prefix
cannot collide with a real immediate counter-folder component. Decode that
identity at the settings boundary; do not create static-file aliases. Unknown
page overrides resolve to the active profile at page load. Explicit unknown API
profile names fail rather than silently writing a different profile.

Global activation refreshes only unpinned command clients. A values write refreshes
clients pinned to the written profile and unpinned clients when it is active.
Keep the existing bounded relay/control queues and coalescing save worker.

## Consequences

Unmodified tosu settings frames can select a pinned profile without protocol
changes. Overlays using `COUNTER_PATH` to construct asset URLs must instead use
the page URL before opting into an override; the qualified identity is not a
resource URL. Renaming/deleting a profile does not rewrite already-open socket
identities; reload pinned instances with a valid profile. Schemas and developer
schema writes affect every profile. Installed overlays remain trusted same-origin
code under ADR 0013, and profile management uses the same mutation provenance.

The first-party card and inline settings editors share profile management,
field search and header navigation. vosulay embeds
`/settings?overlay=<name>&embed=1` with optional `profile=<name>` and uses the
documented HTTP API, rather than maintaining another settings renderer.
Embed mode removes site navigation and external setting links, uses in-document
listboxes and hex/RGB colour controls instead of native popup surfaces, and
confirms deletion in-page. Ordinary pages retain native controls. The iframe
posts `{type: "vosu-profiles", active, profiles}` to its parent after load, profile
actions and successful saves; only profile names are exposed. Receivers validate
origin/source and shape. Wildcard delivery supports local off-screen parents
without relaxing the exact-Origin/Host mutation policy. A localhost page can
mutate a 127.0.0.1-bound listener when both Origin and Host use localhost and the
normal host allowlist permits it.

# Permanent bundled adapter surface contracts

Normative A4 freeze, approved 2026-11-24. Implements the exception in
[plugin ABI §7.5](plugin-abi.md#75-bundled-only-http-surfaces-d3), not a new
external ABI or a second privileged host. P0 and R0 implement these operations;
this document specifies behavior and ownership, not an assertion that they
already exist. Fidelity, approved public additions and exact removals are in
[the source inventory](../../tests/fixtures/upstream/ADAPTER_CONTRACT.md).

## Identity and authorization

Core issues a non-forgeable bundled requester only after the explicit bundled
manifest/vtable registration succeeds. Its identity is the actual plugin
instance, never a caller-supplied plugin id, PID, URL parameter or credentials.
The three identities are `vosu.adapter.upstream`, `vosu.adapter.gosumemory`,
`vosu.adapter.streamcompanion`. External/process plugins cannot obtain this
requester or register HTTP routes. Required adapter initialization fails
closed if any required registration or grant fails; no B1 fallback.

All adapters declare the public fields they consume under `field:vosu.client`,
`field:vosu.status`, `field:vosu.chart`, `field:vosu.play`, `field:vosu.skin`
and `field:vosu.osu` (narrower declared descendants are valid). Requirements
use the actual owning namespace version, not an adapter release version.
Every adapter echoing path data additionally requires exactly
`field:vosu.osu.internal.paths` or justified narrower descendants. A whole
`field:vosu.osu` grant grants no private subtree. Chart parsing/legacy current
calculation requires `field:vosu.chart.resources.file` **and**
`resource:chart.file`; a field grant selects a reference, not byte authority.
Declare any other resource kind actually consumed separately. No new overlay
or tool grant, private non-path payload, or HTTP loopback read is permitted.

## P0: capture and subscriptions

The public conceptual operations are:

- `capture(requester, session_selector, selectors) -> CapturedSelection`;
- `subscribe(requester, session_selector, selectors) -> SubscriptionLease`;
- `capture_roster(requester, game) -> IdentifiedRoster` (also included in an
  adapter capture when requested).

A selector is a namespace plus validated RFC 6901 pointer. Session selection
is fixed opaque session identity or focused. The capture resolves focused
**once**, authorizes all selections against that requester's loaded manifest,
and returns one immutable selected image with:

1. resolved opaque session identity, owning plugin instance and generation;
2. focus/roster generation and the selected namespaces' revision vector;
3. per-selection present/null/absent/error status and bounded immutable CBOR
   values, without exposing any ungranted sibling;
4. an identified roster from the same coherent capture: stable session
   identities/generations, owning game/plugin, variant and optional per-session
   OS foreground. PID is not adapter API identity.

Coherence means there exists one bus/focus/roster cut that produced the whole
result, not merely separately consistent documents. A concurrent commit,
focus switch, identify or detach yields a complete old or complete new cut,
never a mixture. No sampling, demand registration or forced sample occurs on
capture or roster read. Fixed detached sessions report target-gone; no session
selected reports no-session. Missing/ungranted selectors follow the existing
surface absent-value rule; these are not fabricated zeroes. Errors do not
partially install a subscription. Public API projection still strips private
paths on every route/frame/cache/log path.

Subscriptions use the existing grant/lane compiler and latest-only registry.
The lease owns exactly its compiled selector demand. Drop, disconnect,
unregister and failed startup release it; filter replacement atomically
replaces that client's selectors without withdrawing another client's or API
v1's demand. One unfiltered upstream feed selects all fields it emits,
including independent key slots when its schema emits them. Filtered feeds
need not request omitted histories. HTTP is a read, never a subscription.
Notifications identify relevant revisions; they do not transport a legacy
snapshot. A typed adapter-owned view decodes only changed selected components,
reuses immutable histories/strings, and never rehydrates RegularState,
StateBundleSnapshot or CombinedSnapshot across this interface.

Roster foreground is independent of the Focus rule. Windows compatibility
`game.focused` is OR over **all identified, owned osu! sessions**, including
nonselected spectators. Unknown foreground stays unknown until core supplies
an observation; Linux omits this wire field. Manager focus priority does not
mean the manager is OS-foreground.

## P0: route/feed registration

`register_routes(requester, descriptors, providers) -> RegistrationLease` is
atomic. Each descriptor names listener role (overlay-origin or dashboard-origin),
exact HTTP method and exact path or validated path-template/prefix/descendant matcher.
WS upgrades are explicit feed registrations, not inferred from a GET route.
Core rejects overlaps whose match sets intersect for the same role/method,
including static/template/prefix and upgrade collisions; distinct roles or
methods are distinct registrations. Existing core-owned routes participate
in the same collision check. No wildcard may absorb a retired exact route.
`RoutePath::Children` matches nonempty descendants, excluding the base and its
trailing slash; this lets `/files/skin/<name>` coexist with the retired index.
Core transport handlers reserve their descriptors with
`PluginHost::reserve_core_surface_routes`; reservations have no provider callback
and share registration collision, capacity and lease-release rules.

Provider invocation receives an immutable bounded request and opaque selected
context. Core retains generic HTTP/WS framing, peer/Origin/key policy, body
and frame limits, write completion acknowledgements and security headers.
Adapter providers own legacy selection, filters, status/envelope and encoding.
Management provenance is a core-minted opaque witness of dashboard-origin
and valid `X-Vosu-CSRF`, not a bool supplied by a provider or forwarded header.
Overlay routes never inherit that witness. No configuration-write or credential
accessor is added to surface.

`RouteProvider::respond` returns a sendable asynchronous response future.
`SurfaceRequest` carries method, raw path, once-decoded ordered query pairs,
selected context and allow-listed request headers (`range`, `if-none-match`,
`if-modified-since`, `if-range`). Core rejects more than 128 query pairs,
32 headers, or 16 KiB combined query/header bytes before invoking a provider.
It never forwards Origin, API keys, cookies or CSRF credentials.
`SurfaceResponse` carries a validated status, up to 32 headers / 16 KiB,
and `ResponseBody::{Empty,Bytes,File}`. File bodies carry an R0 opened-object
lease plus checked offset/length; the generic transport streams bounded chunks
without reopening a pathname. Response headers are limited to content type,
length/range, accept-ranges, validators, cache control, location and CSP.

`SurfaceFeed` exposes a connection-owned `FeedControl` and `FeedReceiver`.
Replacing bounded legacy-filter JSON atomically replaces the H0 plan/demand;
rejection retains the prior plan. `Outbound::Text` reserves the single state
slot until `write_completed` after the actual socket send. Other frames do not
acknowledge state. Feed close, receiver drop and registration teardown release
demand synchronously; schema cadence and latest-only coalescing remain H0's
responsibility. Core retains command authorization and transport framing.

Provider callbacks, encoding, filesystem operations, plugin dispatch and
awaits run outside host/state/registration/resource locks. Leases unregister
all routes/feeds on drop and stop admitting new callbacks. In-flight callbacks
retain only their already selected context and cancellation/stream leases;
shutdown cancels and joins owned work without joining the current worker.
Teardown ordering must prevent callback-after-provider-destruction. A foreign
requester cannot remove another instance's registration.

## R0: opaque root/file operations

`declared_root(requester, captured_session, slot) -> RootLease` selects the
**owning game provider's** previously declared root. A session-scoped slot
overrides the plugin-wide slot. The captured owner/session generation and
current root epoch bind the lease. Root re-declaration, detach, owner loss or
shutdown invalidates future opens, cached directory lookups and pending
resolutions. An active-session switch never rebinds an existing lease.

Only these root operations are exposed:

- validate a relative child once, then open it under the retained root;
- bounded retained-directory listing/lookup for stable case-insensitive and
  fallback skin matching, returning relative names plus entry kind/metadata;
- same-open-object metadata plus bounded asynchronous body/reader for a file;
- calculation-only selection of an absolute spelling **inside** the selected
  session's declared roots, converted to an authorized retained-root child.

No root pathname, OS file handle, `PathBuf`, directory authority constructed
from a bus string, arbitrary absolute open, or `vosu-sys` type crosses the
surface boundary. Core performs native retained-handle confinement (including
symlink/reparse and Windows spelling checks), retaining the object used for
metadata and bytes. Do not canonicalize a root, validate it, then reopen its
pathname. Calculation's absolute-path containment check is not authority by
itself: the eventual open must still be confined under the retained declared
root. Relative calculation paths resolve under the declared songs slot;
absolute paths may match any declared slot of that same captured session,
with the most-specific containing root selected deterministically. No match
is a bad-request invalid-path result, not a request to another session.

`/Songs/{path}` remains browsable **within its declared game root**, unlike
API v1's reference membership policy. No root operation widens API-v1
resource/tree membership or permits an external plugin to serve HTTP. Decode
legacy URL path once, reject malformed escapes/parent traversal, keep stable
case/fallback matching in upstream adapter, return legacy lazer skin 404,
and leave API-v1 lazer trees unchanged.

Root operations return typed not-found, not-ready, permission, stale/target-gone,
invalid-path and I/O outcomes. The adapter maps them to its pinned route's
status/body/HEAD/range/validator policy; generic core machinery cannot substitute
an API-v1 error envelope. A successfully opened file remains bound to its
original session and object, never retried against newly focused state.

## R0: resource bytes and lifetime budgets

`open_published_resource(requester, captured_session, selected_reference)`
checks both resource capability and that reference's live membership for
that owning session/generation. It reuses M3 resolver, root epochs, content
identity and stream leases, not a loopback fetch and not a resource URL treated
as filesystem authority. Returns immutable resource identity/type/size and a
bounded async reader; successful file metadata/bytes refer to the same object.
Generated streams retain M3 chunk bounds, deadlines, cancellation and exactly
one close on success, error, limit, cancel, drop, detach or shutdown. Current
calculation without `path` uses the captured chart reference through this seam.

A chart-byte consumer enforces the existing **64 MiB** chart/calculation input
limit both before allocation and while reading (limit + 1 detection). Directory
inspection uses the M3 bounded tree-entry/path-byte limits and truncation
contract, and never treats truncated-away entries as API-v1 members. Legacy
private skin lookup may page bounded directory operations; it must not claim
absence from only a truncated page or retain an unbounded full-directory index.
Surface capture inherits the bus's admitted document/selector bounds; it must
not truncate selected histories or silently accept only the first namespace.

## Derived work, encoding and test seams

Budgets are hard ownership bounds, not timers:

- Per SC adapter instance: one owned cancellable chart worker, one running job,
  **one latest pending** job, and **one current successful parsed/derived chart**
  cache. No parse/cache/worker activity without a real dependent consumer.
  Resource bytes never exceed 64 MiB. Replaced cache images retained by an
  in-flight view are released with that view, not stored in a history.
- The derived key includes captured owner/session generation, resource content
  identity, effective ruleset, complete mods/settings/rate/scoring and audio
  length inputs actually used. Cache only successful opens. Same filename or
  checksum is insufficient. Stale completions cannot replace current output.
- Per adapter schema: one current immutable decoded view and one cached
  unfiltered result (including a failed encoding result) per semantic revision
  identity. HTTP then WS and all plain clients share the same immutable bytes.
  Relevant focus/roster/namespace/settings/derived revisions invalidate it;
  unrelated precise-only changes do not invalidate regular data.
- Filter-plan cache: at most **64 entries and 1 MiB retained plan storage** per
  schema, LRU eviction. Above budget, compile/use without retaining; never reject
  a formerly valid filter merely because cache is full. Per-pass encodings are
  discarded after that pass; plan eviction drops no client demand.
- Each connection owns at most one in-flight state frame plus one latest pending
  notification; 1,000 revisions never become a 1,000-frame queue. No client
  creates a periodic adapter poller or a per-client history clone.

Instance-owned test hooks expose capture/view decode, unfiltered/filtered encode,
resource-open/read/close, parse/job start/cancel/join and cache occupancy counts,
plus barriers for open, worker completion and frame write acknowledgement.
No process-global PID fixture registry. Every test wait has a bounded timeout;
never use sleeps as ordering proof or callbacks while locks are held.

Required P0/R0 counterexamples: two disjoint namespace changes around a focus
switch; foreign registration/unregistration; method/template collision;
HTTP read with zero demand; two clients where one disconnects; API-v1 private
sentinel exclusion with plugin positive control; nonselected foreground;
wrong owner/root and stale epoch; rename/symlink/reparse race; drop mid-stream
closes once; A completing after B; unchanged precise tick causes zero parses;
100 plain clients encode once; cache overflow stays bounded without wire change.

## A4.5 removed-route attribution

The removed route still returns ordinary exact 404 and never upgrades.
Attribution is advisory only and changes neither routing nor authorization.
Validate `l` using the existing installed-overlay identity resolver, or use
validated same-origin overlay path provenance. An Origin alone, arbitrary
Referer path or unrecognized identity is **unknown**, never guessed. Do not
record query strings, arbitrary paths/referrers, peer IPs, request bodies,
credentials or private state.

Retain an in-memory deduplicated map keyed by validated installed overlay id
and a fixed removed-feature enum (`selected-v2-feed`, `legacy-skin-index`, or
an explicitly recognized removed-field request). Maximum **128 known overlay
entries**, LRU eviction, **24-hour inactivity TTL**, plus **one unknown bucket
per feature**. Time is injected for tests; expiration occurs on bounded map
access/notice reads, not a periodic wakeup. Duplicate attempts update last-seen
and a saturating count, not append a log. Unknown traffic cannot evict a known
identity. Each notice contains only known identity/display name (escaped by
UI), feature enum, first/last seen and saturating attempt count; unknown notices
contain no claimed overlay name. Identity is validated again before rendering
an actionable overlay link. Restart clears notices; no durable tracking.

M8 migrates exactly the four retired configuration inputs listed in the source
inventory, idempotently preserving unrelated keys and API-v1 debounce. The
D20 notice links to the maintained migration guide when its owning UI lands;
A4 does not claim M8 execution, M4b owner captures or clean-room page completion.

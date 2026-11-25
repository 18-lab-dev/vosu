# M4b behavioral golden contract

Normative A4b freeze, approved 2026-11-24. The owner decided A4b.1 (capture
operator) and A4b.4 (data and licensing); main adopted every other A4b item as
recommended under the owner's delegated spec authority. Baseline `0eedb34`;
upstream pin `2359305a63f2aaebb201bc8d2eee58177f52590e` (`Cargo.toml`
`source-commit`, `PINNED_SOURCE_REVISION` in
`tools/live-parity/live_parity.py`). This is an implementation and operating
contract. It is not evidence that any tool, capture, candidate or fixture
exists. Approval IDs are those of the M4b plan's A4b register.

Behavioral goldens record what the pinned upstream daemon actually answers to
a live client: status, headers, error bodies, file routes, legacy calculation
and WebSocket handshakes and frames. They are separate from the source oracle
in `tests/fixtures/oracle/`, which stays the projection golden and is never
regenerated or replaced by M4b tooling. Exemptions come only from the
[divergence ledger](DIVERGENCES.md) through the rule file in §10.

## 1. Evidence model

**Upstream is the only expected source.** Every expected status, header, body
or frame in a golden comes from the pinned upstream daemon. A vosu dump is a
replay input, never an expected value; a golden derived from vosu output is a
defect, whatever it matches.

Three records exist and must never be confused:

| Record | Location | Content | Publication |
|---|---|---|---|
| Private raw capture | Owner-selected evidence root outside every repository | Exact upstream and vosu raw HTTP/WS transcripts, unnormalized dumps, build/client/process/socket attestations, operator log, checksums | Never committed, uploaded or attached anywhere. Owner decides retention. |
| Candidate fixture | A new directory written by `golden`, never inside a committed fixture tree | Normalized deterministic files (§9), approved assets, inventories | Reviewed by main before any commit (§4) |
| Committed behavioral golden | `tests/fixtures/upstream/<pin>/` | The approved candidate, unchanged | Ordinary repository content only |

A raw capture is not a fixture, a partial fixture or an "unnormalized golden".
It is never committed, renamed into the fixture tree or cited as golden
evidence. `golden` reads one complete, attested, pin- and platform-matching
private capture and writes only a candidate directory.

Never M4b evidence: `StateBundleSnapshot`, bridge B1, typed legacy snapshots,
prebuilt DTOs, source-oracle synthetic engines, fake providers or
`strip_vosu_extensions`; timestamp-nearest joins between daemons; release
live-parity `capture`/`compare` output; active-play exact payloads; any
capture taken from a build without the attestation in §3.

## 2. Capture authority (A4b.1, owner decision)

Golden captures are **owner-authorized and agent-operated**. Each capture
session needs a written owner authorization naming:

- the vosu commit and the pinned source revision;
- the platform, client build/version, install path and profile;
- the scenario set and platform order;
- the account, beatmap, skin, media and network policy (§4);
- the private evidence path;
- the GUI operator scope: what the agent may click, type and launch;
- the fixture review route.

An agent then runs the exact documented commands, operates the VM GUI within
that scope, and collects private evidence. **VM lifecycle pre-authorization is
not capture authorization**: permission to start, stop or stage work in a VM
never authorizes launching a client or a capture. Without the written
authorization, an agent does not start a capture, whatever else it may do on
the VM.

Isolation requirements:

- Captures use only isolated VM test installs and task-owned profiles: the
  Windows agent VM under its test account, and the Linux gaming VM's headless
  `/opt/osu/osu-headless` with a task-owned profile and compositor. The
  persistent Linux VM is never assumed disposable.
- The Windows and Linux VMs never run at the same time.
- An already-running client is never adopted without fresh attestation.
- The agent stops and reports, without answering, on any purchase, licence,
  login, private-account, chat, telemetry, UAC, security or unexpected data
  prompt.

The clean-room `/api/ingame` observation is human-only and outside agent
authority (A4b.12, §8). The owner remains approver of scope and publication.

## 3. Capture build (A4b.2)

The harness endpoint exists only under the root `test-harness` feature, which
`build.rs` rejects in release builds. Golden capture therefore uses a
separately attested **debug** vosu build:

```sh
# Windows
cargo build --locked --no-default-features --features test-harness
# Linux (keeps the default linux-process-events feature)
cargo build --locked --features test-harness
```

The capture records the profile, features, target, executable SHA-256 and
vosu commit. No release-capable harness profile, configuration key or
weakened guard is added. Release live parity stays an optional diagnostic
([ADR 0011](../../../docs/decisions/0011-live-parity-optional.md)) and is
never a golden. Cadence, latency and throughput from debug captures are not
performance or timing evidence.

## 4. Data, licensing and publication (A4b.4 owner decision, A4b.8)

- Accounts: offline or dedicated test accounts only. No chat. No online
  leaderboard, multiplayer or other networked data unless the authorization
  names it.
- Media: committed fixtures use only **generated vosu-owned test maps, skins
  and media**, each recorded in `assets.json` with SHA-256, size, generator
  and license. The owner's library, other local content and third-party maps,
  skins, audio or images are never committed, even when the client shows them.
- Paths: private roots appear in candidates only as synthetic roots (§7).
- Raw captures stay private (§1).
- Review: main reviews each candidate before commit: the diff, the
  brand inventory, the privacy and asset checks and the replay result. `golden`
  never writes canonical fixtures. Fixture commits are separate from adapter
  fixes, because their review is privacy, licensing and content review.
- **Nothing is uploaded or published**: no GitHub release, artifact store,
  attachment, issue or external service receives raw captures or candidates.

## 5. Harness endpoint and dump envelope (A4b.3)

Endpoint: `GET /__vosu_test/v1/adapter-capture?schema=<v1|sc|v2|v2-precise>`.

- Compiled only with root `test-harness`; never present in a release build.
- Mounted only when the daemon receives a per-launch capability from the
  live-parity runner through the private environment entry
  `VOSU_TEST_ADAPTER_CAPTURE` (exactly 64 lowercase hex characters, 32 random
  bytes). A malformed value fails daemon startup with an error that does not
  contain it. With the feature but no capability the path is an ordinary exact
  404.
- Loopback peers only. The request carries exactly one
  `X-Vosu-Test-Capture: <32-byte random hex capability>` header. A request from
  a non-loopback peer, with a missing, duplicated or incorrect capability, or to
  any near-miss path is routed exactly as by a build without the endpoint, so
  it gets the ordinary unknown-path 404 (never an endpoint-specific 401/403).
  The overlay listener's peer and Origin gate still applies first, and the
  dashboard listener never serves the endpoint. No Origin-based access, API key
  or tool-token substitute, and no CORS.
- Exact query: exactly one known `schema` parameter. Unknown, duplicate or
  extra parameters get 400 with a fixed small JSON error.
- GET only; every other method gets 405 and records no sample.
- `200 application/json`, `Cache-Control: no-store`. A response above 8 MiB is
  never truncated: it gets 507 with a fixed error. A failed capture or envelope
  gets 500 with a fixed error that never echoes the internal message.
- Not a stream. No demand registration, subscription, global journal, PID
  registry or unbounded history. Concurrent requests use the provider's
  existing serialization; daemon shutdown drops them.
- The capability, broker/API credentials, PIDs, raw private roots and
  executable paths are never logged, reported or written into a dump. Raw
  evidence redacts the capability header (§7).

The response is the exact `CapturedSelection` that `BusStateProvider`'s
existing `snapshot()` retained and decoded, together with the adapter bytes
from that same snapshot. The endpoint never calls `requester.capture` itself
and never reads state a second time. Envelope `vosu-adapter-capture-v1`:

```json
{
  "format": "vosu-adapter-capture-v1",
  "schema": "v1|sc|v2|v2-precise",
  "provider_generation": 1,
  "session": {"ordinal": 0, "generation": 1, "owner": "vosu.osu", "game": "osu", "variant": "lazer", "foreground": true},
  "focus_revision": 1,
  "roster": {"revision": 1, "sessions": []},
  "bus_revision": 1,
  "namespace_revisions": [["vosu.osu", 1]],
  "entries": [{"ns": "vosu.osu", "pointer": "/...", "value_cbor_b64": "...", "error": null}],
  "derived": {"revision": 1, "status": "ready|pending|none"},
  "http": {"kind": "ready|not_ready|failed", "status": 200, "body_utf8": "..."},
  "digest": "sha256:..."
}
```

Selected entries keep their raw CBOR bytes from the existing codec; nothing
converts them through JSON or merges integer and float, null and absence, or
map order. Session strings and owner instance numbers are private run
identity and become ordinals that preserve equality, the selected session,
roster order and foreground/generation relations. `value_cbor_b64` is the one
permitted binary encoding, because CBOR is binary; text bodies stay UTF-8.

## 6. Cases, stable windows and sandwiches (A4b.5)

Each golden case has a stable case ID, a client state barrier and a request
transcript. Case IDs are `<family>.<slug>`, with `<family>` from the table
below and `<slug>` matching `[a-z0-9]+(-[a-z0-9]+)*(\.[a-z0-9]+(-[a-z0-9]+)*)*`.
An ID is unique within a platform and is never reused for different request
bytes; changed request bytes need a new ID.

Barriers are `no-client`, `menu`, `paused` and `result`. They are the only
**stable windows** in which exact payload goldens are recorded. Active play
contributes only diagnostic cadence/framing and coarse lifecycle evidence,
never an exact payload fixture. Each platform scenario traverses
menu → play → paused → result → client exit.

A case is recorded as:

1. Wait for the case's barrier.
2. **Upstream sandwich:** send request A to upstream, record the response,
   send the identical A again. The case is valid only when both upstream
   responses are equal after the rules of §7.
3. **vosu sandwich:** call the dump endpoint, send A to the real vosu route,
   call the dump endpoint again. Both dumps must agree on provider generation,
   selected-entry digest, focus and roster revisions and derived-input state,
   and a state route's `GET` response must equal the dump's adapter status and
   bytes. Status and headers come from the route record, so a `HEAD` status is
   not compared with the dump. For the `sc` schema only, `provider_generation`
   and `derived.revision` are exempt: every `sc` read releases the
   streamCompanion chart-work handle, so both advance on each read (main
   decision, 2026-11-24). An `sc` sandwich still compares the whole selection
   and digest, `derived.status` and the HTTP result.
4. On failure, retry the **whole case** only, at most three attempts, and
   record every attempt. Attempts are never mixed.

Cases are never joined by timestamp. Request bytes are identical for both
daemons; no redirect is followed and nothing is replayed implicitly. The only
per-daemon substitution is a conditional-request validator that the case
declares as taken from the same daemon's earlier response in that case; the
substituted value is covered by §7's `header-etag` or `header-last-modified`
rule. WebSocket cases record
the actual handshake and the first relevant text frame in the stable window;
filtered-feed cases send exact legacy filter messages and record the
resulting frame. Full feed sequences, normal close and slow-reader behavior
are raw diagnostics, not payload goldens.

Raw HTTP records keep request bytes, status line, ordered raw headers and body
bytes. Raw WS records keep opcode, fin, mask, length and payload bytes. A
golden compares the message body after transfer-coding removal; the raw
framing stays in the private capture.

A connection that the daemon closes or resets before sending any response byte is
an explicit outcome, not a failure: the record is `{"outcome": "closed"}` with the
request (and, for the `ws` family, no frames), and it is compared like any other
record. Both upstream sandwich records must close, and vosu must close too. A
partial response is never a golden. Golden captures hold only golden-role
route-matrix cases. Negative and diagnostic cases carry a `negative` or
`diagnostic` ID segment, and `golden` and `adapter_golden` refuse both.

A case whose request kills the pinned upstream daemon has the explicit outcome
`upstream_crashed`; it is neither a failure nor an exclusion (main's contract
decision and the owner amendment of 2026-11-25). Only route-matrix cases under the
reviewed ledger row `upstream-malformed-uri-crash` ([DIVERGENCES.md](DIVERGENCES.md))
have it: upstream answers a request target with a malformed percent escape, then its
response logger throws and the process exits. These cases run last, after the
`no-client` barrier and its cases, so they can never abort the barrier sequence.
Before each attempt that finds upstream exited, the capture launches and re-attests a
fresh upstream and waits until both daemons report `no-client`. The record holds the
request, every byte upstream sent before it died and its nonzero exit code, followed
by the usual vosu sandwich: vosu must answer (a response, not a closed connection)
and its dumps must agree. `golden` checks the record and the ledger row and treats
the case as a documented divergence (upstream crashes, vosu answers): with no
upstream answer to replay, it writes no case file and lists the case, its ledger ID
and vosu's status in its summary.

### Route matrix families

| Family | Required cases |
|---|---|
| `state-http` | `GET` and `HEAD` for `/json`, `/json/sc`, `/json/v2`, `/json/v2/precise`; not-ready and no-client; `POST`, `PUT`, `DELETE`, `OPTIONS`; trailing slash, wrong case, suffix, double slash, malformed percent escape, a meaningless query, and the maximum query and header budget cases |
| `ws` | Valid upgrades for `/ws`, `/tokens`, `/websocket/v2`, `/websocket/v2/precise`. Invalid upgrades: missing key, wrong version, missing `Upgrade`/`Connection`, POST upgrade, GET without upgrade. Near misses `/websocket/v2/`, `/websocket/v2/precise/`, `/websocket/v2/subscribe`. Filtered frames: no filter, valid nested filters, repeated filter, invalid JSON, oversized message within the existing limit |
| `files` | `/files/beatmap/{background,audio,file}`, an approved fixture `/files/beatmap/<relative>`, `/Songs/<relative file>`, `/Songs/<relative dir>/`, `/backgroundImage`; stable and lazer `/files/skin/<name>`; exact `/files/skin/`; `GET`/`HEAD`; valid single range, unsatisfiable range, multiple ranges; `If-None-Match`, `If-Modified-Since`, `If-Range` |
| `path-security` | Encoded parent, double-encoded parent, backslash; absolute drive and UNC spellings, ADS and trailing dot/space (Windows only); a symlink or reparse fixture where the platform permits |
| `calculation` | `/api/calculate/pp` with no query; mods, accuracy, combo and statistic variants; invalid numbers; relative path; absolute path inside the declared root; absolute sentinel outside it (the `legacy-calculate-declared-roots` security divergence) |
| `origin-peer` | No Origin, loopback Origin, foreign Origin, malformed Host; the API-key-required configuration only if the approved route matrix includes keys |

`path-security` and outside-root `calculation` targets are only run-owned
sentinel files inside a temporary capture root. The probe aborts before
sending if a target canonicalizes outside that root. Real profile, home,
system, credential or owner files are never probed.

Not upstream goldens: `/api/v1`, dashboard, tool, counter and other vosu-only
routes, which appear only as negative near-miss cases, and `/api/ingame` (§8).

### Header comparison (A4b.7)

Compared exactly after §7 (name, presence and value; the reason phrase and
header order are recorded privately): the status code, `content-type`,
`content-length`, `cache-control`, `etag`, `last-modified`, `accept-ranges`,
`content-range`, `location`, `content-security-policy`,
`x-content-type-options`, every `access-control-*` header, and for WebSocket
handshakes `upgrade`, `connection`, `sec-websocket-accept` and the protocol
negotiation fields `sec-websocket-protocol`, `sec-websocket-extensions` and
`sec-websocket-version`. Every other header is recorded only in the private
capture. Another header joins this set only when a client contract relies on
it, through a reviewed amendment to this file.

When `root-prefix` changed a body or frame, `content-length` (or the frame
length) is compared as the byte length of the normalized body, and each side's
raw `content-length` must still equal its own raw body length (for `HEAD`, the
length of the matching `GET` body).

## 7. Normalization inventory (A4b.6)

`golden` and `adapter_golden` apply only the rules below, each to its exact
scope, to upstream records and to vosu output alike. A rule never matches a
key or header by pattern: there is no "any `time` key", "any date-like
string" or "any number" rule. Values are replaced only when the scoped value
is a string; a number, boolean, null or absent member at a scoped location is
never touched. `golden` checks that its implemented rule IDs equal the first
column of this table exactly.

Normalization version: 1

| Rule ID | Scope | Replacement | Retained local invariant |
|---|---|---|---|
| `header-date` | Response and WebSocket handshake header `date` | `<golden:date>` | Value is an IMF-fixdate; presence is compared |
| `header-last-modified` | Response header `last-modified` in the `files` family, and the same value echoed in that case's request `If-Modified-Since`/`If-Range` (§6) | `<golden:last-modified>` | IMF-fixdate, present and equal in both upstream sandwich responses; in replay, equal to the declared replay file's mtime |
| `header-etag` | Response header `etag` in the `files` family, and the same tag echoed in that case's request `If-None-Match`/`If-Range` (§6) | `"<golden:etag:N>"`, one ordinal per distinct value, weak `W/` prefix kept | Valid entity-tag, present and equal in both upstream sandwich responses; distinct tags stay distinct |
| `root-prefix` | Exact token-bounded occurrences of an attested private absolute root prefix: raw, JSON-escaped (one and two levels, including nested JSON strings), percent-encoded and Windows forward-slash spellings, in bodies, frames, headers and dump CBOR text strings | The synthetic root assigned to that attested root (below) | The path below the root is kept verbatim; only the changed body's `content-length` or frame length is recomputed (§6); any remaining private prefix fails the privacy scan |
| `envelope-identity-ordinal` | Session, roster, resource and process identity fields of vosu dump envelopes only; ordinalized by the capture endpoint, verified by `golden` | Stable ordinals | Equal inputs map to equal ordinals and distinct inputs to distinct ordinals; selected session, roster order and foreground/generation relations preserved |
| `raw-capability-redaction` | Request header `X-Vosu-Test-Capture` in private raw evidence, redacted by `golden-capture` | `<redacted>` | `golden` rejects any occurrence of the header or its value in a capture or candidate; dump calls are not route records |
| `client-timestamp-pointer` | String-valued client timestamps at an exact (case family, record part, JSON Pointer) scope listed in this row only | `<golden:client-timestamp>` | Proven volatile: the value differed between the two upstream sandwich responses while nothing else did. **Approved instances in version 1: none.** |

Synthetic roots: one distinct root per attested root, named after its
declared slot as `C:\vosu-golden\<name>\` on Windows or `/vosu-golden/<name>/`
on Linux, for example `C:\vosu-golden\osu!\` or `/vosu-golden/osu/`. The
manifest records the mapping from slot to synthetic root, never the private
prefix. Replay creates real temporary roots and maps resources to them;
echoed display paths are data and never become root authority.

**Never normalized:** status, route or method, body error class;
content-type, security, cache, range or upgrade semantics; numbers; null
versus absence; object and array order; legacy quirks; bus revision
relationships; coherent-sample identity; anything outside the scopes above.

Adding or widening a rule (including a `client-timestamp-pointer` instance)
requires a reviewed amendment to this file, a counterexample test in
`golden` and `adapter_golden`, and a bump of the normalization version above
and of the manifest `normalization_version`. A pilot mismatch is never
resolved by a new rule unless it is a proven volatile value; payload defects
are fixed in the owning vosu module.

## 8. `/api/ingame` exclusion (A4b.12)

M4b captures no `/api/ingame` request, status, header or body, and no upstream
page HTML, JavaScript or CSS; no fixture may contain such a record, and
`adapter_golden` fails if one appears. Agents never read, capture, record or
summarize those page assets or this repository's history before `1ccd4cc` for
them. The clean-room observation runs in the same scheduled session as the
full-set capture but as separate human work by the named eligible author,
whose output is `docs/v2/ingame-behavior.md`
([implementation plan §6](../../../docs/v2/implementation-plan.md#6-clean-room-apiingame)).
An agent operates the machine during it only if the owner explicitly asks,
and then never inspects, records or summarizes page source.

## 9. Fixture layout, format and budget (A4b.8, A4b.13)

```text
tests/fixtures/upstream/<pin>/
  manifest.json            revision manifest (format vosu-golden-fixture-v1)
  inventory.json           SHA-256 and byte size of every file below <pin>/
  divergence-rules.json    §10
  windows-stable/  windows-lazer/  linux-lazer/
    manifest.json          platform manifest (format vosu-golden-platform-v1)
    inventory.json  assets.json  brand-inventory.txt
    cases/…                one file per case: request, upstream expected record, replay envelope
    assets/…               approved generated assets (§4)
```

`<pin>` is the full 40-hex Cargo `source-commit`. Platform directories are
exactly the `SUPPORTED_MATRIX` pairs. A platform manifest records its format,
`normalization_version` (initially 1), `provenance` (`captured` or
`synthetic`), pin, platform and client variant/version, vosu commit, harness
build attestation hash, scenario set, slot-to-synthetic-root mapping, case
counts by family and the review receipt reference. It contains no private
path, account, capability or credential. The platform `inventory.json`
carries the privacy-scan result as `privacy_scan`. A case's `expected` record
holds the status code, the §6 compared headers plus `date`, the body and the
frames, all from upstream, or exactly `{"outcome": "closed"}` (§6). An
`upstream_crashed` case has no case file (§6).

Files are deterministic, sorted by platform and case ID. Valid UTF-8 text,
including every HTTP body and WS text frame, is stored as text so
`scripts/check-brand.py` and reviewers see upstream names. Text is never
base64-encoded or compressed to avoid brand or privacy review.

Size budget per platform: at most **8 MiB** of committed JSON/CBOR-envelope
files plus at most **4 MiB** of approved binary assets, each asset a separate
file with SHA-256 and license in `assets.json`. Exceeding it needs owner
approval or a smaller scenario, never compression that hides content.

`golden` rejects incomplete, wrong-pin or wrong-platform captures, unattested
binaries, mixed attempts, unknown files or binary text, unredacted capability
or credential data, private paths and over-budget output. It refuses a
candidate directory that is non-empty, contains a git-tracked path or lies
anywhere under `tests/fixtures/upstream/`.

Before captures exist, `adapter_golden` mechanics tests use tiny
vosu-generated fixtures with `provenance: "synthetic"`. They are never
labeled upstream and never count as pilot or full evidence.

## 10. Divergence subtraction (A4b.11)

`tests/fixtures/upstream/<pin>/divergence-rules.json`:

```json
{
  "format": "vosu-golden-divergence-rules-v1",
  "rules": [
    {
      "ledger_id": "legacy-calculate-declared-roots",
      "platforms": ["windows-stable", "windows-lazer", "linux-lazer"],
      "cases": ["calculation.absolute-outside-root"],
      "part": "body",
      "vosu_replacements": [{"pointer": "/error", "value": "..."}]
    }
  ]
}
```

- `ledger_id` names an existing row of [DIVERGENCES.md](DIVERGENCES.md) whose
  class is `security`, `contract` or `licensing`; `temporary` or unknown
  classes fail.
- `cases` lists exact case IDs; `platforms` lists the platforms it applies to.
- `part` is one of `status`, `headers.<name>` (a §6 header), `body`,
  `ws.handshake` or `ws.frame`.
- Exactly one of `vosu_value` (the exact expected vosu value of that part) or
  `vosu_replacements` (minimal JSON Pointer replacements inside a JSON body or
  frame) is present.
- Every rule must match at least one recorded case; an unused rule fails.
- A rule that replaces a whole status, body, handshake or frame, or lists
  every case of a route, is a whole-route subtraction. It is admitted only
  when the ledger row's scope names that route verbatim. A header issue is
  never subtracted through `body`.
- Every comparison not covered by a rule is exact. A security rule proves
  vosu's stricter result; it never hides an upstream body mismatch elsewhere.
- Part details: `headers.<name>` takes a string `vosu_value`, or `null` for
  an absent header, and never names `date`. `ws.handshake` replaces the status
  and the whole compared header list. `ws.frame` applies to every server text
  frame of the listed cases. A route with a single recorded case is listed
  whole by any rule on that case. Every listed case must exist on every
  listed platform, and a rule whose vosu value equals the upstream part, or
  two rules on one part of one case, fail. `divergence-rules.json` is
  optional: without it nothing is subtracted, and when present it is
  inventoried like every other file.

## 11. Pin policy (A4b.10)

`adapter_golden` reads only `tests/fixtures/upstream/<Cargo source-commit>/`
and fails unless the fixture manifest pin, Cargo `source-commit` and
`PINNED_SOURCE_REVISION` agree. A pin bump is a separate owner-approved change
that, together, updates Cargo, the live-parity pin, the source-oracle
provenance through its owning workflow, `DIVERGENCES.md`, this contract, the
fixture directory name and the fixtures, and deletes the previous behavioral
directory (history preserves it). A pin move before M8, a fixture-format
break or an owner-rejected capture requires a full three-platform recapture.
A vosu-only fix after a valid capture needs only replay.

## 12. Brand allowlist (A4b.14)

Fixture text that spells the upstream name needs exact per-file entries in
`scripts/brand-allowlist.txt`: category `parity` for upstream wire and
source-revision manifests, `fixture` only for synthetic or replay content. No
directory or revision-tree wildcard. Counts are added or raised only in the
reviewed fixture commit, from `golden`'s generated brand inventory. Upstream
literal values are never renamed.

## 13. Exit commands (A4b.9)

`tests/adapter_golden.rs` is declared with
`[[test]] required-features = ["test-harness"]`, because the exact production
listener composition is feature-gated. The exit command is:

```sh
cargo test --locked --features test-harness --test adapter_golden
```

Record the commit, features, platform and nonzero case counts by platform and
family. A featureless, zero-case, unpinned or synthetic-only run is not
pilot or full evidence. L1 tooling exit additionally runs the live-parity
Python suites, the `adapter_capture` endpoint tests, `state_api_compat`,
`server_compat`, the brand, doc-link and file-length checks, and the native
Windows replay through `python scripts/vm-test.py --test adapter_golden`.

Until the reviewed fixture commit of the Windows lazer pilot,
`tests/fixtures/upstream/PENDING_PILOT` holds exactly the Cargo pin on its
first line and a one-line reason on its second. With it,
`golden_fixtures_replay_at_the_cargo_pin` passes and prints that it compared
zero upstream cases; that run is not evidence. The test fails with "no golden
fixtures for pin `<pin>`" when neither `<pin>/` nor a marker naming that pin
exists, and it also fails when the marker names another pin or holds more than
the reason line, when a marker sits beside `<pin>/`, or when another 40-hex
fixture directory exists. X0 deletes the marker in the fixture commit.

The comparator's own coverage comes from `tests/fixtures/golden-synthetic/`
and the counterexample tests over it. That tree is written by G0's writer with
`provenance: "synthetic"` from R0 replays of vosu's corpus source. It holds a
`windows-stable` platform and a lazer-shaped `linux-lazer` platform with the
same case IDs. Route bytes depend on the replaying OS (upstream `game.focused`
exists only on Windows), so a synthetic platform's name also names the host OS
it is exported and compared on: `windows-*` only on Windows and `linux-*` only
on Linux, and each OS must have at least one. Regenerate the tree with
`python tests/adapter_golden/generate_synthetic.py --foreign-export <path>`
after running the exporter natively on the other OS (the script's docstring
has the `vm-test` step). A synthetic tree is refused under
`tests/fixtures/upstream/<pin>/`. A committed capture also
needs a review receipt, distinct source and vosu attestations, and brand
inventory lines that are exact allowlist entries. Each replay asset file gets
the declared modification time 2026-01-01T00:00:00Z, and `header-last-modified`
holds vosu's `Last-Modified` to it.

Replay inputs follow what the platform's production host records:

- **Resource references.** A recorded `vosu.chart` `/resources/<kind>`
  reference (`file`, `audio`, `background`) is backed by the approved asset
  that the osu! plugin would resolve: the asset at the recorded `vosu.osu`
  `/internal/paths` `current` leaf (`beatmapFile`, `audio` or `background`),
  joined under the recorded `beatmapFolder` on stable, in the variant's songs
  slot (`songs` on stable, `files` on lazer). Its approved `media_type` must
  equal the recorded `type`. Two approved assets of one media type on a
  platform are therefore told apart by their recorded location, not by type.
  The recorded string only selects among approved assets: it is never opened,
  so an absolute or private recorded path matches nothing and the case is
  refused. A reference with no matching asset is refused, and a recorded path
  never falls back to file authority.
- **Foreground.** Core has an OS-foreground source only on Windows
  (`docs/v2/plugin-abi.md` §6.2). A `windows-*` case records `true`/`false`
  for every session, and a `linux-*` case records `null`. Any other fact is
  refused when the fixture loads. Replay takes the source's availability from
  the recording, never from the machine it runs on. A recorded `null` uses a
  test-harness foreground source reporting unavailable through the same
  `ForegroundSource::available` seam as the production host off Windows. A
  roster mixing known and unknown facts is refused.
- **Trees.** Tree references (`vosu.skin` `/tree`, `vosu.chart`
  `/resources/set`) are outside replay scope. No adapter selects one, so no
  capture envelope can record one, and adapter file routes use declared roots
  instead (lazer `/files/skin/` is the legacy 404). Replay refuses a recorded
  tree reference with "adapter selections never include tree refs".
  `adapter_selections_never_include_tree_refs` fails as soon as an adapter's
  selectors, or the production composition's dumped selection, could contain
  one. Tree replay must be added before that change lands.
- **Derived status.** A recorded `pending` derived status is not a stable
  input and is refused.

Each capture session runs, on its platform:

```sh
python tools/live-parity/run.py check-config --config "$PARITY_CONFIG"
python tools/live-parity/run.py golden-capture --config "$PARITY_CONFIG" --scenario-set "$APPROVED_SCENARIO" --out "$PRIVATE_CAPTURE"
python tools/live-parity/run.py golden --capture "$PRIVATE_CAPTURE" --candidate-dir "$CANDIDATE_DIR"
cargo test --locked --features test-harness --test adapter_golden
python scripts/check-brand.py
```

Capture evidence names the owner authorization, VM/service status and
lifecycle actions, client SHA-256/version/install/profile identity, source
pin and harness binary hash, scenario, case and frame counts, private evidence
location (a label, not content), candidate SHA-256 inventory, privacy-scan
result, brand inventory, replay counts by platform and the classification of
every mismatch.

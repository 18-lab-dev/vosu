# Upstream compatibility divergences

ADR 0021 defines four classes: `security` (permanent sandbox and access-policy
changes), `contract` (permanent differences in separately specified contracts),
`licensing` (permanent removal of an upstream surface vosu cannot ship without
upstream's LGPL assets), and
`temporary` (pre-release gaps with an explicit closing task; forbidden at day 0).
Only the entries below authorize compatibility exemptions. A2b's explicit state
schema contract changes are permanent `contract` differences, not blanket skips.

For M4a, the normative [source and endpoint inventory](ADAPTER_CONTRACT.md)
defines exact removals and required fidelity proofs. Entries scoped to
`vosu.osu` below apply only to the state schema, never to adapter wire:
adapters restore upstream names and distinguish pending from missing-score
failure. A4 approval is not implementation evidence and closes no temporary
entry by itself.

The former vosu 1.x-only wire members and routes removed by the approved M4a
extension cutover are the `adr0021-*` and `lazer-legacy-skin-404` rows below,
with exact route and member scope. Their accurate state-bus facts remain; these
rows are removals that restore the pinned upstream wire, never an exemption
allowing extra fields in an upstream adapter. `state_api_compat` proves every
removed member absent at every nesting level while nonempty source facts are
present on the bus (`seeded_sources_change_every_schema_while_extensions_stay_off_the_wire`).

The four `vosu.osu` rows authorize no adapter wire difference. The adapters
restore `bestOF` and `roomID`, fail `/json` and `/json/v2` with HTTP 500 for a
lazer spectator without score while `/json/v2/precise` and its feed stay
ready with `tourney: []` exactly as 1.x (whose precise builder never read
lazer spectators), and keep `sliderBreaks` out of v2
`resultsScreen.hits` (`state_schema_contract_entries_do_not_authorize_adapter_wire_differences`,
`stable_aggregate_maps_distinct_manager_and_spectators_on_every_session`).

The former stable-tournament temporary exemption is closed: no ledger row
exists for it, the committed `stable-tournament` oracle matches byte for byte
for all four schemas, and distinct manager/spectator fixtures prove IPC order,
per-spectator play, keys and hit errors, manager-only controls and chat, and an
identical aggregate whichever stable session is focused.

| id | class | scope | reason | closed by |
| --- | --- | --- | --- | --- |
| tourney-null-without-score | contract | vosu.osu.tourney on a tick with an IPC client lacking score | A2b publishes null instead of failing the entire 1.x v2 response with HTTP 500 | permanent A2b state-schema contract |
| tourney-bestOf-rename | contract | vosu.osu.tourney.bestOf | A2b renames 1.x bestOF to bestOf without changing the value | permanent A2b state-schema contract |
| room-roomId-rename | contract | vosu.osu.room.roomId | The state schema renames 1.x roomID to roomId without changing the value | permanent state-schema contract |
| result.statistics.sliderBreaks | contract | vosu.osu.result.statistics.sliderBreaks | A2b specifies constant 0, following 1.x v1; the result source has no measured slider-break count and 1.x v2 omits this member | permanent A2b state-schema limitation |
| legacy-calculate-declared-roots | security | `/api/calculate/pp` explicit `path`, relative to the captured session's declared songs slot (lazer: files slot), or an absolute spelling inside that session's declared roots | A4.4 rejects arbitrary outside-root absolute access; retained-root opening replaces canonicalize-and-reopen; canonical `.osu` extension and engine/query/mod/result semantics unchanged | permanent A4.4 contract; R1 injected provider, native Windows evidence pending |
| legacy-game-file-confinement | security | `/files/beatmap/*`, `/files/skin/{path}`, `/Songs/{path}`, `/backgroundImage` | ADR 0005 retained declared-root confinement, single decoding, symlink/reparse escape rejection; Windows ADS rejected and trailing dot/space spelling not aliased | permanent ADR 0005 / A4 root authority |
| legacy-directory-listing-budget | security | Legacy HTML directory indexes under `/Songs/{path}`, `/files/beatmap/{path}`, stable `/files/skin/{path}` | Bound one listing to 131,072 entries / 16 MiB of names; larger listings carry an explicit truncated marker. This accommodates heavy 20k–50k Songs collections while bounding attacker-controlled directory allocation. Stable case lookup pages to completion independently, never inferring absence from a truncated HTML listing | approved M4a R1 listing budget |
| legacy-game-file-sandbox | security | game file responses on the routes above | sandbox CSP and nosniff; no active-content type escalation | permanent ADR 0005 / ADR 0021 |
| adapter-listener-policy | security | adapter HTTP and WebSocket endpoints; route-matrix case `origin-peer.host.malformed-port` (a `Host` with a non-numeric port) | overlay-listener peer, Origin/API-key policy and existing request/frame limits; no dashboard-listener bypass. The pinned upstream never answers a request whose `Host` has a non-numeric port (its router's `new URL` throws before any response byte, on every attempt); vosu answers within the case deadline. Captures record upstream's `timeout` as that case's expected outcome and `golden` writes no case file for it (GOLDEN_CONTRACT §6) | permanent ADR 0016 / ADR 0021; upstream no-response: Main's M4b decision of 2026-11-26 |
| adapter-surface-request-budget | security | Every surface-registered adapter route on the overlay listener: `/json`, `/json/sc`, `/json/v2`, `/json/v2/precise`, `/api/calculate/pp`, `/ws`, `/tokens`, `/websocket/v2`, `/websocket/v2/precise`, `/backgroundImage`, `/Songs/{path}`, `/files/beatmap/*`, `/files/skin/{path}` | Under `adapter-listener-policy`: a request carrying more than 128 decoded query pairs (`MAX_QUERY_PAIRS`), more than 16 KiB of query plus forwarded-header metadata (`MAX_METADATA_BYTES`), more than 32 forwarded headers (`MAX_HEADERS`), or a control byte in a forwarded `range`, `if-none-match`, `if-modified-since` or `if-range` value never reaches a provider and gets the ordinary `Not Found` 404, with no upgrade. Source: `crates/vosu-surface/src/routes.rs` (`SurfaceRequest::valid_metadata`). 1.x ignored the query on the state, feed and game-file routes and parsed `/api/calculate/pp` without any pair or byte budget, so such requests were still served | approved M4a X0 bounded-request security divergence; `server_compat` `mounted_origin_peer_limits_and_api_v1_privacy_are_enforced_on_adapter_routes` |
| legacy-calculation-engine | contract | `/api/calculate/pp` engine/query/result behavior | ADR 0004 keeps LegacyRosuEngine separate from the live lazer engine | permanent ADR 0004 |
| legacy-calculate-mod-inputs | contract | `GET /api/calculate/pp`; golden cases `calculation.mods-acronyms` and `calculation.mods-json` | Well-formed acronym pairs and valid structured JSON mod inputs are decoded as mods (including settings), rather than coercing a query string to `NaN` and answering an internal error. Legacy numeric bitmasks are unchanged. | permanent ADR 0004 / A4b |
| legacy-calculate-client-errors | contract | `GET /api/calculate/pp`; golden cases `calculation.invalid-accuracy`, `calculation.invalid-clock-rate`, `calculation.mods-invalid`, `calculation.invalid-count`, and `calculation.invalid-mode` | Malformed accuracy, out-of-range clock rate, malformed acronym, invalid unsigned count, and invalid ruleset reject as 400 with the documented `{"error": string}` response; the pinned upstream may calculate from its coerced argument or emit a WASM-decoder 500. Valid inputs retain the existing calculation contract. | permanent ADR 0004 / A4b |
| adr0021-v2-extension-removal | contract | `/json/v2`, `/websocket/v2`: `/beatmap/time/rate`, `/beatmap/time/running`, `/play/source`, `/keys` entirely, `/play/pressHitErrors`, `/play/releaseHitErrors`, `/play/keyHitErrors`, `/play/hitOffsets`, `/play/catcher`, and the same play members under `/tourney/clients/*/play` | vosu 1.x-only extensions are absent, not null or empty; accurate facts stay on the bus as `vosu.osu.beatmap.time.rate`, `vosu.osu.beatmap.time.running`, `vosu.osu.play.source`, `vosu.play.rate`, `vosu.osu.keys`, `vosu.osu.play.hitErrors`; `/play/hitErrorArray` and all pinned members unchanged | permanent ADR 0021 / A4.6; removed by V0 and D0 |
| adr0021-precise-extension-removal | contract | `/json/v2/precise`, `/websocket/v2/precise`: `/keys/all`, `/pressHitErrors`, `/releaseHitErrors`, `/keyHitErrors`, `/hitOffsets`, `/catcher`, `/tourney/*/keys/all`, `/tourney/*/pressHitErrors`, `/tourney/*/releaseHitErrors` | vosu 1.x-only extensions are absent; upstream `/keys/k1`, `/keys/k2`, `/keys/m1`, `/keys/m2`, `/hitErrors` and `/tourney/*/ipcId`, `/tourney/*/keys`, `/tourney/*/hitErrors` keep upstream order | permanent ADR 0021 / A4.6; removed by V0 |
| adr0021-gosumemory-extension-removal | contract | `/json`, `/ws`: `/gameplay/hits/pressHitErrors`, `/gameplay/hits/releaseHitErrors`, and the same members under `/tourney/ipcClients/*/gameplay/hits` | vosu 1.x-only extensions are absent; `/gameplay/hits/hitErrorArray` and `/gameplay/keyOverlay` remain | permanent ADR 0021 / A4.6; removed by G0 |
| adr0021-sc-extension-removal | contract | `/json/sc`, `/tokens`: `pressHitErrors`, `releaseHitErrors` (Rust source names `press_hit_errors`, `release_hit_errors`), including inside embedded JSON string tokens | vosu 1.x-only extensions are absent; `hitErrors` and the `keyOverlay` JSON string remain | permanent ADR 0021 / A4.6; removed by C0 |
| adr0021-removed-selected-v2-feed | contract | `/websocket/v2/subscribe`, every method, with or without an upgrade request | vosu 1.x-only route: exact `Not Found` 404, never an upgrade or static/overlay fallthrough; near misses stay 404; attribution records only an installed validated overlay identity (A4.5); API v1 session subscriptions replace it. The pinned upstream never answers the upgrade request (its `upgrade` handler owns only exact WebSocket paths): route-matrix case `ws.near.v2-subscribe` records upstream's `timeout` as the expected outcome and `golden` writes no case file for it (GOLDEN_CONTRACT §6) | permanent ADR 0021 / A4.5; N0; upstream no-response: Main's M4b decision of 2026-11-26 |
| adr0021-removed-legacy-skin-index | contract | `/files/skin/` exact path, every method, with or without an upgrade request | vosu 1.x-only metadata index: exact `Not Found` 404, never an upgrade or listing; attribution as above; retained-root private lookup and API v1 skin trees remain. The pinned upstream never answers the upgrade request (as above): route-matrix case `files.skin.index.upgrade` records upstream's `timeout` as the expected outcome and `golden` writes no case file for it (GOLDEN_CONTRACT §6) | permanent ADR 0021 / A4.5; N0, R1; upstream no-response: Main's M4b decision of 2026-11-26 |
| adr0021-removed-ingame-page | licensing | `/api/ingame` exact path, every method, with or without an upgrade request | Upstream serves its LGPL in-game overlay page here; vosu cannot ship it, and the owner dropped the route from 2.0 without a clean-room replacement. Exact `Not Found` 404, never an upgrade, redirect, page, `Deprecation` or `Link` header; attribution as above under `ingame-page`; vosulay is the in-game overlay path | permanent ADR 0021 2026-11-25 amendment / A7.5; N1 |
| lazer-legacy-skin-404 | contract | `/files/skin/{path}` when the captured session is lazer | The legacy lookup and its lazer delegation are removed and return 404; API v1 lazer skin trees and resources remain functional | permanent ADR 0021 / A4.6; R1 |
| upstream-malformed-uri-crash | security | Adapter HTTP requests whose target has a malformed percent escape (a `%` not followed by two hex digits, or escapes that are not UTF-8); route-matrix cases `state-http.{json,json-sc,json-v2,json-v2-precise}.near.malformed-percent` and `.near.malformed-percent-hex` | The pinned upstream answers, then its response logger's `decodeURIComponent(req.url)` throws an uncaught `URIError` and the daemon exits; vosu answers and keeps serving. Found by the first M4b pilot; every route-matrix request was then sent to a client-less pinned upstream and only these eight killed it. Captures record them as `upstream_crashed` (upstream's bytes and exit code), run after the no-client barrier with an upstream restart between them; `golden` writes no case file for them (GOLDEN_CONTRACT §6) | permanent; owner amendment and Main's contract decision of 2026-11-25 (M4b O0) |
| upstream-unanswered-near-upgrade | security | WebSocket upgrade requests to `/websocket/v2/`, `/websocket/v2/precise/`, `/ws/` and `/tokens/`; route-matrix cases `ws.near.{v2,v2-precise,ws,tokens}-trailing-slash` | The pinned upstream's `upgrade` handler answers only the exact paths it owns and leaves every other upgrade unanswered: no byte before the 30 s case deadline, on every attempt and in both sandwich records (client-less rehearsal and the first M4b full pilot run, 2026-11-26). vosu does not copy the hang: it answers the bounded `Not Found` 404 at once and never holds the connection open. Captures record upstream's `timeout` as these cases' expected outcome; `golden` lists them with vosu's status and writes no case file (GOLDEN_CONTRACT §6) | permanent; Main's M4b decision of 2026-11-26 |
| upstream-no-file-validators | contract | Conditional requests on the game file routes whose validator comes from the daemon's own response; route-matrix cases `files.conditional.{if-none-match-etag,if-modified-since-last-modified,if-range-etag,if-range-last-modified}` | The pinned upstream never sends `ETag` or `Last-Modified` on any route (no such code in its source; its `/files/beatmap/file` answered 200 without either on every attempt of the second M4b full pilot run, 2026-11-26), so a client cannot condition a request on its validators. vosu adds both validators and honours `If-None-Match`, `If-Modified-Since` and `If-Range` against them: an additive improvement, not a removal. Captures record upstream's 2xx priming response without a validator as `validator-missing`, these cases' expected outcome; `golden` lists them with vosu's status and writes no case file (GOLDEN_CONTRACT §6) | permanent; Main's M4b decision of 2026-11-26 |
| upstream-private-path-echo | security | Windows absolute drive and device spellings on `/Songs/{path}` and `/files/beatmap/{path}`; route-matrix cases `path-security.{songs,beatmap}.{absolute-drive,absolute-drive-slash,unc-extended,unc-device}` | The pinned upstream joins the decoded absolute spelling under its songs folder (`path.win32.join`), the stat fails, and its router answers 5xx echoing the server-side path in its status line and JSON body. Found by the pre-run-3 static audit (2026-11-26; node emulation of upstream's URL and path handling plus golden's privacy scan). vosu refuses cleanly: a 3xx/4xx answer that never serves the sentinel and carries no absolute path or capture root in its status line, headers or body. Captures keep upstream's raw records private; `golden` checks the echo structurally, asserts vosu's refusal, lists the cases with vosu's status and writes no case file (GOLDEN_CONTRACT §6) | permanent; Main's M4b decision of 2026-11-26 |

`adr0021-removed-ingame-page` records the owner's 2026-11-25 decision: 2.0
serves no `/api/ingame`, and no clean-room replacement is scheduled. M4b never
captures the route ([golden contract §8](GOLDEN_CONTRACT.md)), so no
`divergence-rules.json` rule names that row. This freeze does not claim that
the security contract rows added above have completed R1/X0 native evidence.

## Fixture-input fact: explicit zero calculation

The 1.x source-oracle inputs `sc-equal-bpm` and `sc-range-bpm` were
calculated with the replay's named `ZeroEngine`. Their expected SC bytes
therefore contain maxCombo 0 and an empty strain object despite a one-object
chart. Replay preserves that declared calculation input through a constructor
available only under tests/`test-harness`; no other case opts into it and the
production default remains the concrete live engine. This is not a production
divergence and authorizes no expected-byte normalization. A separate real-engine
resource/worker test requires maxCombo 1 and all 26 zero-padded bins 0..10000
for the same bytes, so the fixture cannot conceal a production regression.

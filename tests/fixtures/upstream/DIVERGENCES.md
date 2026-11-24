# Upstream compatibility divergences

ADR 0021 defines four classes: `security` (permanent sandbox and access-policy
changes), `contract` (permanent differences in separately specified contracts),
`licensing` (permanent functional compatibility without upstream assets), and
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
| adapter-listener-policy | security | adapter HTTP and WebSocket endpoints | overlay-listener peer, Origin/API-key policy and existing request/frame limits; no dashboard-listener bypass | permanent ADR 0016 / ADR 0021 |
| adapter-surface-request-budget | security | Every surface-registered adapter route on the overlay listener: `/json`, `/json/sc`, `/json/v2`, `/json/v2/precise`, `/api/calculate/pp`, `/ws`, `/tokens`, `/websocket/v2`, `/websocket/v2/precise`, `/backgroundImage`, `/Songs/{path}`, `/files/beatmap/*`, `/files/skin/{path}` | Under `adapter-listener-policy`: a request carrying more than 128 decoded query pairs (`MAX_QUERY_PAIRS`), more than 16 KiB of query plus forwarded-header metadata (`MAX_METADATA_BYTES`), more than 32 forwarded headers (`MAX_HEADERS`), or a control byte in a forwarded `range`, `if-none-match`, `if-modified-since` or `if-range` value never reaches a provider and gets the ordinary `Not Found` 404, with no upgrade. Source: `crates/vosu-surface/src/routes.rs` (`SurfaceRequest::valid_metadata`). 1.x ignored the query on the state, feed and game-file routes and parsed `/api/calculate/pp` without any pair or byte budget, so such requests were still served | approved M4a X0 bounded-request security divergence; `server_compat` `mounted_origin_peer_limits_and_api_v1_privacy_are_enforced_on_adapter_routes` |
| legacy-calculation-engine | contract | `/api/calculate/pp` engine/query/result behavior | ADR 0004 keeps LegacyRosuEngine separate from the live lazer engine | permanent ADR 0004 |
| adr0021-v2-extension-removal | contract | `/json/v2`, `/websocket/v2`: `/beatmap/time/rate`, `/beatmap/time/running`, `/play/source`, `/keys` entirely, `/play/pressHitErrors`, `/play/releaseHitErrors`, `/play/keyHitErrors`, `/play/hitOffsets`, `/play/catcher`, and the same play members under `/tourney/clients/*/play` | vosu 1.x-only extensions are absent, not null or empty; accurate facts stay on the bus as `vosu.osu.beatmap.time.rate`, `vosu.osu.beatmap.time.running`, `vosu.osu.play.source`, `vosu.play.rate`, `vosu.osu.keys`, `vosu.osu.play.hitErrors`; `/play/hitErrorArray` and all pinned members unchanged | permanent ADR 0021 / A4.6; removed by V0 and D0 |
| adr0021-precise-extension-removal | contract | `/json/v2/precise`, `/websocket/v2/precise`: `/keys/all`, `/pressHitErrors`, `/releaseHitErrors`, `/keyHitErrors`, `/hitOffsets`, `/catcher`, `/tourney/*/keys/all`, `/tourney/*/pressHitErrors`, `/tourney/*/releaseHitErrors` | vosu 1.x-only extensions are absent; upstream `/keys/k1`, `/keys/k2`, `/keys/m1`, `/keys/m2`, `/hitErrors` and `/tourney/*/ipcId`, `/tourney/*/keys`, `/tourney/*/hitErrors` keep upstream order | permanent ADR 0021 / A4.6; removed by V0 |
| adr0021-gosumemory-extension-removal | contract | `/json`, `/ws`: `/gameplay/hits/pressHitErrors`, `/gameplay/hits/releaseHitErrors`, and the same members under `/tourney/ipcClients/*/gameplay/hits` | vosu 1.x-only extensions are absent; `/gameplay/hits/hitErrorArray` and `/gameplay/keyOverlay` remain | permanent ADR 0021 / A4.6; removed by G0 |
| adr0021-sc-extension-removal | contract | `/json/sc`, `/tokens`: `pressHitErrors`, `releaseHitErrors` (Rust source names `press_hit_errors`, `release_hit_errors`), including inside embedded JSON string tokens | vosu 1.x-only extensions are absent; `hitErrors` and the `keyOverlay` JSON string remain | permanent ADR 0021 / A4.6; removed by C0 |
| adr0021-removed-selected-v2-feed | contract | `/websocket/v2/subscribe`, every method, with or without an upgrade request | vosu 1.x-only route: exact `Not Found` 404, never an upgrade or static/overlay fallthrough; near misses stay 404; attribution records only an installed validated overlay identity (A4.5); API v1 session subscriptions replace it | permanent ADR 0021 / A4.5; N0 |
| adr0021-removed-legacy-skin-index | contract | `/files/skin/` exact path, every method, with or without an upgrade request | vosu 1.x-only metadata index: exact `Not Found` 404, never an upgrade or listing; attribution as above; retained-root private lookup and API v1 skin trees remain | permanent ADR 0021 / A4.5; N0, R1 |
| lazer-legacy-skin-404 | contract | `/files/skin/{path}` when the captured session is lazer | The legacy lookup and its lazer delegation are removed and return 404; API v1 lazer skin trees and resources remain functional | permanent ADR 0021 / A4.6; R1 |

ADR 0021's clean-room `/api/ingame` replacement is a separately scheduled
licensing change, not an exemption declaring the present implementation
complete. Its actual functional differences and implementation evidence must
be recorded by that work's owner. Likewise this freeze does not claim that
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

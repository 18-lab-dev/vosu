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

The former vosu-only v2 clock/source wire fields (`beatmap.time.rate`,
`beatmap.time.running`, `play.source`, including nested play copies) are removed
by the approved M4a extension cutover. Their accurate state-bus facts remain;
this is not an exemption allowing extra fields in an upstream adapter.

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
| legacy-calculation-engine | contract | `/api/calculate/pp` engine/query/result behavior | ADR 0004 keeps LegacyRosuEngine separate from the live lazer engine | permanent ADR 0004 |

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

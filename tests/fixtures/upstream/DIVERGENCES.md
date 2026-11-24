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

| id | class | scope | reason | closed by |
| --- | --- | --- | --- | --- |
| tourney-null-without-score | contract | vosu.osu.tourney on a tick with an IPC client lacking score | A2b publishes null instead of failing the entire 1.x v2 response with HTTP 500 | permanent A2b state-schema contract |
| tourney-bestOf-rename | contract | vosu.osu.tourney.bestOf | A2b renames 1.x bestOF to bestOf without changing the value | permanent A2b state-schema contract |
| room-roomId-rename | contract | vosu.osu.room.roomId | The state schema renames 1.x roomID to roomId without changing the value | permanent state-schema contract |
| result.statistics.sliderBreaks | contract | vosu.osu.result.statistics.sliderBreaks | A2b specifies constant 0, following 1.x v1; the result source has no measured slider-break count and 1.x v2 omits this member | permanent A2b state-schema limitation |
| legacy-calculate-declared-roots | security | `/api/calculate/pp` explicit `path`, relative to the captured current session's declared songs root or an absolute spelling inside that session's declared roots | A4.4 rejects arbitrary outside-root absolute access; retained-root opening replaces canonicalize-and-reopen; engine/query/mod/result semantics unchanged | permanent A4.4 contract; R1 implementation evidence required |
| legacy-game-file-confinement | security | `/files/beatmap/*`, `/files/skin/{path}`, `/Songs/{path}`, `/backgroundImage` | ADR 0005 retained declared-root confinement, single decoding, symlink/reparse escape rejection; Windows ADS rejected and trailing dot/space spelling not aliased | permanent ADR 0005 / A4 root authority |
| legacy-game-file-sandbox | security | game file responses on the routes above | sandbox CSP and nosniff; no active-content type escalation | permanent ADR 0005 / ADR 0021 |
| adapter-listener-policy | security | adapter HTTP and WebSocket endpoints | overlay-listener peer, Origin/API-key policy and existing request/frame limits; no dashboard-listener bypass | permanent ADR 0016 / ADR 0021 |
| legacy-calculation-engine | contract | `/api/calculate/pp` engine/query/result behavior | ADR 0004 keeps LegacyRosuEngine separate from the live lazer engine | permanent ADR 0004 |

ADR 0021's clean-room `/api/ingame` replacement is a separately scheduled
licensing change, not an exemption declaring the present implementation
complete. Its actual functional differences and implementation evidence must
be recorded by that work's owner. Likewise this freeze does not claim that
the security contract rows added above have completed R1/X0 native evidence.

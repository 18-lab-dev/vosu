# Upstream compatibility divergences

ADR 0021 defines four classes: `security` (permanent sandbox and access-policy
changes), `contract` (permanent differences in separately specified contracts),
`licensing` (permanent functional compatibility without upstream assets), and
`temporary` (pre-release gaps with an explicit closing task; forbidden at day 0).
Only the entries below authorize compatibility exemptions. A2b's explicit state
schema contract changes are permanent `contract` differences, not blanket skips.

| id | class | scope | reason | closed by |
| --- | --- | --- | --- | --- |
| stable-tournament/tourney | temporary | v1, v2, v2-precise members `tourney` | stable tournament-manager aggregate deferred (D6) | plan M4a "Stable tournament-manager aggregate" task |
| tourney-null-without-score | contract | vosu.osu.tourney on a tick with an IPC client lacking score | A2b publishes null instead of failing the entire 1.x v2 response with HTTP 500 | permanent A2b state-schema contract |
| tourney-bestOf-rename | contract | vosu.osu.tourney.bestOf | A2b renames 1.x bestOF to bestOf without changing the value | permanent A2b state-schema contract |
| room-roomId-rename | contract | vosu.osu.room.roomId | The state schema renames 1.x roomID to roomId without changing the value | permanent state-schema contract |
| result.statistics.sliderBreaks | contract | vosu.osu.result.statistics.sliderBreaks | A2b specifies constant 0, following 1.x v1; the result source has no measured slider-break count and 1.x v2 omits this member | permanent A2b state-schema limitation |

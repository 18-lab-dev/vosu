# M4a adapter source fidelity and removal inventory

Normative A4 freeze, approved 2026-11-24 under the owner's delegated spec
authority. Baseline `e1df38f`; upstream pin
`2359305a63f2aaebb201bc8d2eee58177f52590e`. This is an implementation contract,
not evidence that P0/P1/R0 or the adapters have landed. The committed source
oracle remains `tests/fixtures/oracle/inventory.json`; do not regenerate it.

## Reading the source table

`O` means namespace `vosu.osu`, `C` means `vosu.client`, `P` means
`vosu.play`, `H` means `vosu.chart`, `S` means `vosu.skin`. Paths after a
namespace are JSON Pointers. `R`/`Q`/`F` mean regular capture, precise capture,
and core/session lifecycle publication. Sampling demand is independent:
`rg` = regular/gameplay, `ra` = regular/always, `qg` = precise/gameplay,
`qa` = precise/always, `bg` = both/gameplay, `-` = no accelerated demand.
Each comma-separated member in a row is a separate leaf mapping, not an
exemption for its containing object. Arrays retain order and each element's
listed leaf rules. An identity subtree mapping expands recursively to every
leaf of the existing schema, including optional members and mod settings;
no unlisted source leaf may be silently defaulted.

Types are Rust view types: `i64`, `f64`, `bool`, immutable UTF-8 `str`,
`Option<T>`, immutable slices, and typed records with the named members.
CBOR numeric decoding preserves the original number, not a JSON stringify /
parse round trip. Nullable and absent are distinct in the view. An absent
required selection is not-ready, not numeric zero; wire defaults below are
only the pinned builder's explicit defaults. Every row requires a real
producer-to-bus fixture and raw adapter-byte oracle assertion. Test labels
below name required discriminating cases, not claims of existing tests.

## Approved accurate additions (P1/T0)

A4.2 approves these public facts, not an opaque compatibility payload. Publish
optional additions in `vosu.osu` **1.3.0**, updating the JSON Schema, provider
version, field partition, dirty dependencies, covers where applicable, and
sampling annotations together with their producer. Until P1 lands, the shipped
schema and manifest stay 1.2.0. Existing accurate fields keep their meanings.
All additions are nullable when the actual source is unavailable; no made-up
zero, empty history or logical-key alias is an acceptable producer.

| Approved pointer | Typed value / actual producer | Publication / demand | Required proof |
|---|---|---|---|
| O `/keys/overlay` | nullable record `k1,k2,m1,m2`, each nullable `{isPressed: bool,count: i64}`; independent `precise.key_overlay` slots 0,1,2,3 | Q/qg | Four distinct counts/states and a different logical `keys.all`; missing slot stays null until the adapter applies its pinned default |
| O `/play/score` | `f64?`; `regular.gameplay.score`, without common-layer activity gating | R/rg | Menu/play/pause/result retain actual captured value, not common null |
| O `/play/combo` | nullable `{current: i64,max: i64}`; gameplay combo/max_combo | R/rg | Distinct current/max outside and inside play |
| O `/play/accuracyPercent` | `f64?`; gameplay.accuracy before division by 100 | R/rg | IEEE boundary and oracle values; never reconstruct by multiply if it changes bytes |
| O `/play/healthRaw` | nullable `{normal: f64,smooth: f64}`; gameplay.player_hp/player_hp_smooth in client's 0-200 units | R/rg | Distinct raw/smooth, fraction boundaries; existing healthBar remains its 0-100 projection |
| O `/play/judgements` | nullable `{miss,meh,ok,good,great,perfect: i64}`; gameplay.statistics raw counts before legacy ruleset conversion | R/rg | All six nonzero distinct counts; every ruleset and legacy tick conversion |
| O `/play/retries` | `i64?`; gameplay.retries | R/rg | Nonzero retries, no substitution of session.playCount |
| O `/play/preciseUnstableRate` | `f64?`; precise.unstable_rate, not gameplay.unstable_rate | Q/qg | Distinct regular/precise estimates and rate other than 1 |
| O `/beatmap/time/firstHitObject`, `/beatmap/time/preview` | nullable `i64` parsed chart times `first_object`/`preview`, independently of captured `first_object_time` | R/- | Distinct parsed/captured first-object times and nonzero preview; missing parsed map stays null |
| O `/beatmap/selectionMode` | `i64?`; menu.gamemode, not original chart mode or gameplay mode | R/- | All three modes deliberately different |
| O `/session/watchingReplay` | `bool?`; global.is_watching_replay independent of current status | R/rg | SC replay/status mapping outside active play |
| O `/leaderboardState` | nullable `{visible: bool,player: <typed main-player record>}`; gameplay.leaderboard_visible/leaderboard_player | R/rg | Empty slots with visible main player; main player differs from every slot |
| O `/tourneyAvailability` | enum `pending`, `ready`, `missingScore`; actual lobby-read readiness / missing-score detection, stable aggregate readiness | R/rg | Early no lobby differs from lobby containing a score-less client; transition invalidates views even when tourney stays null |

The main-player record has `name: str`, `score: f64`, `combo: i64`,
`maxCombo: i64`, `mods` (the existing typed CalculateMods value),
`judgements: {miss,meh,ok,good,great,perfect: i64}`, `team: i64`,
`position: i64` and `isPassing: bool`, sourced directly from
`gameplay.leaderboard_player`; `visible` is its independent sibling.
T0 publishes each spectator's corresponding accurate play facts, independent
overlay slots and precise all-hit history under its typed tourney client
record. Stable aggregate records preserve IPC id/order, manager controls,
team/points/chat, user, settings, beatmap stats and play data. These additions
follow the same 1.3.0 transaction/version rule; API v1 does not acquire HTTP
500 semantics. No parsed chart, strain/break/kiai/timing payload enters
`internal`, which remains paths only.

## Per-source field fidelity

| Old builder source leaves (all four builders) | Bus pointer / view type | Publication / demand | Absence, transformation / upstream quirk | Required case |
|---|---|---|---|---|
| selected client variant | C `/variant`: str | F/- | Preserve stable/lazer spelling | Distinct variants |
| selected identity, generation, focused selection | Coherent surface capture's session/owner/generation/revisions | F/- | No PID-keyed cache or mixed-focus document | Focus switch with equal numeric revisions |
| manager.game_focused | Identified roster's per-owned-session foreground: bool? | F/- | Windows OR over all owned osu! sessions; omit game.focused elsewhere, never use Focus rule's selected boolean | Nonselected foreground session, priority manager |
| global.paused, game_time | O `/session/paused`: bool; `/session/playTime`: f64 | R/rg,ra | Preserve game time conversion; playCount remains pinned literal 0 | Nonzero time and pause |
| devserver_endpoint | O `/session/server`: str? | R/- | Producer preserves ppy.sh default and lossy OS-string spelling | Custom server |
| precise.status | O `/session/state/{number,name}`: i64,str | Q/qa | Raw enum, not inversion of coarse status | Edit/result/lobby/unknown |
| ranked_play.info | O `/rankedPlay`: typed nullable identity subtree | R/rg | Integer stage; retain optionality | Nonempty ranked play |
| global.show_interface, is_replay_ui_hidden, chat_status | O `/settings/interfaceVisible`, `/settings/replayUIVisible`, `/settings/chatVisibilityStatus` | R/rg,rg,- | Invert replay visibility only where old builder did; chat comparisons !=0 (v1) vs >0 (SC) need raw numeric status | Negative/nonzero chat, hidden replay UI |
| bass_density.density | O `/settings/bassDensity`: f64 | R/ra | No recomputation from audio | Nonzero density |
| settings.values resolution, client, scoreMeter, cursor, mouse, tablet, mania, sort, group, mode, audio, background, keybinds, visual | O `/settings/<same group>`: typed recursive schema records | R/- | Each schema leaf maps to its existing settings producer; retain platform/client omissions, enum number/name and false/0/empty defaults only at adapter boundary | Stable/lazer settings with all groups distinct |
| settings skin toggles | O `/settings/skin/{useDefaultSkinInEditor,ignoreBeatmapSkins,tintSliderBall,useTaikoSkin}`: bool | R/- | Keep switches separate from asset identity | Four differing booleans |
| global skin identity/name | S `/name`; O `/skin/{builtin,base,variant,files}` | R/- | No folder inferred from lazer tree | Imported/built-in/stable skin |
| user.profile raw_login_status,raw_bancho_status,play_mode,country_code | O `/profile/{userStatus,banchoStatus,mode,countryCode}/{number,name}` | R/- | Raw numbers select SC login==65793 and country conversion; null profile yields pinned omissions | Missing user vs logged-out user |
| user.profile id,name,ranked_score,level,accuracy,performance_points,play_count,rank | O `/profile/{id,name,rankedScore,level,accuracy,pp,playCount,globalRank}` | R/- | f64/i64/str option leaves; profile accuracy already percent | Nonzero profile |
| user.profile background_colour,matchmaking | O `/profile/{backgroundColour,matchmaking}` typed optional records | R/- | Signed lowercase hex color; absent profile vs present-null matchmaking retained | Negative color, absent matchmaking |
| menu artist,artist_original,title,title_original,creator,difficulty,checksum,map_id,set_id,ranked_status | O `/beatmap/{artist,artistUnicode,title,titleUnicode,mapper,version,checksum,id,set,status}` | R/- | Optional UTF-8/i64/status number; SC embedded text uses literal undefined for missing strings | Unicode and absent metadata |
| menu.gamemode | O `/beatmap/selectionMode`: i64? (addition) | R/- | Never substitute chart original mode | Converted chart |
| beatmap.mode, parsed.metadata.source,tags | O `/beatmap/{mode,source,tags}` | R/- | Mode number/name; pinned missing-map values only in adapter | Missing/parsed map |
| beatmap.event_status kiai,break_time | O `/beatmap/{isKiai,isBreak}`: bool | R/rg | SC isBreakTime remains pinned 0, not this fact | True break/kiai |
| original mode vs effective mode | O `/beatmap/isConvert`: bool | R/- | Preserve original/effective distinction; common keyCount not a mode source | osu! to mania conversion |
| precise.play_time | O `/beatmap/time/live`: f64 | Q/qa | ms; SC divides by 1000 or formats text | Fractional nonzero time |
| render.clock rate,running | O `/beatmap/time/{rate,running}`: f64?,bool? | R/rg | Lazer null is not stable mod rate; retain client branch | Null lazer clock |
| beatmap.first_object_time, parsed.times.full, menu.mp3_length; parsed.times.first_object,preview | O `/beatmap/time/{firstObject,lastObject,mp3Length,firstHitObject,preview}` | R/- | Existing firstObject is capture-derived; additions retain independently parsed first-object/preview values; no assumption these clocks agree | Deliberately different capture/parsed time |
| parsed engine original_difficulty and converted_difficulty AR,CS,OD,HP | O `/beatmap/stats/{ar,cs,od,hp}/{original,converted}`: f64 | R/- | Already rounded projection usable only with exact final-byte proof; raw chart derivation if a consumer needs pre-round precision | Rate/mod/conversion boundaries |
| parsed engine difficulty stars,aim,speed,flashlight,slider_factor,stamina,rhythm,color,reading | O `/beatmap/stats/stars/{total,aim,speed,flashlight,sliderFactor,stamina,rhythm,color,reading}` | R/- | Unsupported optional components omitted; zero/NaN truthiness is adapter quirk, not arbitrary null fill | Zero, negative zero, nonfinite and optional component |
| beatmap.attributes.stars | O `/beatmap/stats/stars/live`: f64 | R/rg | Same pinned rounding | Changing live stars only |
| engine hit_windows miss,meh,ok,good,great,perfect | O `/beatmap/stats/hitWindow/<same leaf>` | R/- | Optional ms; catch absent windows stay absent; stars.hitWindow uses pinned truthy/double-round path | Catch and fraction boundary |
| event_status.bpm; parsed.bpm common,minimum,maximum | O `/beatmap/stats/bpm/{realtime,common,min,max}` | R/- | Four-decimal projection; SC BPM range/text derives raw map values if inversion is lossy | Equal/range BPM, rate !=1 |
| parsed object_counts circles,sliders,spinners,holds; difficulty.max_combo | O `/beatmap/stats/{objects,maxCombo}` | R/- | Integers; objects.total sum only | Four distinct counts |
| gameplay source/render spectating/replay/autoplay | O `/play/source`; O `/session/watchingReplay` addition | R/rg | Preserve watching vs replay client distinction; null outside play | Replay/autoplay/spectator |
| gameplay failed,player_name,mode | O `/play/{failed,playerName,mode}` | R/rg | Raw gameplay mode even when selected mods come from menu/result | Three-way mode/mod selection |
| gameplay score,combo,max_combo | P `/score,combo,maxCombo` during play; O `/play/score,combo` additions elsewhere | R/rg | No common null-to-zero conversion | Captured nonzero menu values |
| gameplay accuracy | P `/accuracy` normalized; O `/play/accuracyPercent` addition | R/rg | Use percent source for parity; division/multiplication must not silently round | Full oracle plus IEEE property/boundary cases |
| gameplay player_hp,player_hp_smooth | O `/play/healthBar`; `/play/healthRaw` addition | R/rg | v2 uses (raw/200)*100; v1/SC consume raw; common health cannot recover smooth | Distinct fractional raw/smooth |
| gameplay statistics miss,meh,ok,good,great,perfect | P `/judgements`; O `/play/judgements` addition for raw counts/all states | R/rg | Apply to_legacy_hits once with correct mode; never convert an already-converted count twice | Catch and lazer tick bookkeeping |
| gameplay statistics small_tick_miss,small_tick_hit,large_tick_miss,large_tick_hit,small_bonus,large_bonus,ignore_miss,ignore_hit,combo_break,slider_tail_hit,legacy_combo_increase; slider_breaks | O `/play/statistics/{smallTickMiss,smallTickHit,largeTickMiss,largeTickHit,smallBonus,largeBonus,ignoreMiss,ignoreHit,comboBreak,sliderTailHit,legacyComboIncrease,sliderBreaks}` | R/rg | Preserve nullable legacy combo increase and distinct sliderEndHits mapping | Distinct tick/tail counts |
| gameplay retries | O `/play/retries` addition | R/rg | SC retries != session.playCount | Nonzero retries |
| gameplay grade_current,grade_expected | O `/play/rank/{current,maxThisPlay}`: str | R/rg | SC numeric grade conversion stays adapter-owned | Silver/max grades differ |
| global.menu_mods, gameplay.mods, result.raw.mods | O `/play/mods` selected record; O `/result/mods` result record | R/rg | Number/name/rate/checksum/array/settings preserved; status 2 gameplay, 7 result, otherwise menu | Three distinct mod sets |
| beatmap.attributes pp,fc_pp,max_achieved,max_achievable,current/fc accuracy,aim,speed,difficulty,flashlight | O `/play/pp` typed recursive record | R/rg | Preserve rounding order; SC noChokePP is fc_pp, not maxAchievable | Distinct detailed totals |
| gameplay.unstable_rate | O `/play/unstableRate`: f64 | R/bg | v1/v2 regular estimate, never precise estimate | Distinct estimators |
| precise.unstable_rate | O `/play/preciseUnstableRate` addition | Q/qg | SC unstableRate multiplies selected mods rate; convertedUnstableRate does not | Rate !=1 |
| precise.key_overlay[0..3].is_pressed,count | O `/keys/overlay/{k1,k2,m1,m2}` addition | Q/qg | Independent slots; SC keyOverlay remains JSON string; old feature flag retired, actual feed demand opts in | Four different slots/logical buttons |
| precise.keypresses[].name,is_pressed,count,pressed_at,released_at | O `/keys/all`: typed ordered slice | Q/qg | Retained API-v1 facts only; not emitted by adapters | Nonempty source and raw key absence |
| precise.hit_errors.all,press,release,key; hit_offsets; active_catcher | O `/play/hitErrors/{all,press,release}`, `/play/keyHitErrors`, `/play/hitOffsets`, `/play/catcher` | Q/qg | Preserve legitimate all history; remove extension wire siblings per inventory below, not bus facts | Nonempty every history and catcher |
| gameplay.leaderboard_scores[] all source leaves | O `/leaderboard[]`: typed schema entries | R/rg | Preserve order, nullable identity, distinct statistics/mods | Two distinct slots |
| gameplay.leaderboard_visible,leaderboard_player all consumed leaves | O `/leaderboardState` addition | R/rg | v1/SC main player not slots[0]; SC nested JSON stays string | Main player differs from first slot |
| beatmap.pp_acc[90..100] | O `/performance/accuracy` entries | R/- | Preserve missing lookup's pinned zero only in adapter; SC 99.9 placeholders stay -1 | Distinct accuracy pp points |
| beatmap.graph primary,series[].name/data,x_axis | O `/performance/graph` typed arrays | R/- | v1 strains/strainsAll and v2 embedded JSON retain spelling and sharing | Multiple nonempty series |
| result.raw player_name,score,accuracy,max_combo,mods,statistics,date; result.grade,pp | O `/result` typed recursive record | R/- | Result counts do not use gameplay counts; v1 result mode retains old gameplay.mode quirk; sliderBreaks=0 is schema-only limitation | Different play/result counts |
| room.info room_id,users[].id,username,country_code,avatar_url | O `/room/{roomId,users}` | R/ra | Adapter restores roomID; absence/null preserved | Nonempty room |
| manager and spectator IPC id,team,controls,best_of,points,chat,user,settings,beatmap/play | O `/tourney` recursive typed aggregate plus availability | R/rg and spectator Q/qg | Restore bestOF; stable aggregate on every stable session; precise clients IPC sorted, no arbitrary session order | Manager replacement and score-less spectator |
| lazer_multi_spectating data/chat/clients user,team,score and score mods/statistics | O `/tourney/clients[]`, `/tourneyAvailability` | R/rg | Pending omits tourney; missingScore errors in adapter only; ready empty is not pending | Three-state readiness and stale completion |
| global.game_folder,songs_folder,skin_folder; menu.folder,filename,background_filename,audio_filename | O `/internal/paths/{stable,lazer,current,skinFolder}`: nullable str records | R/- | JSON echoes/undefined joins only, never root authority; explicit subtree grant | Wrong-root sentinel plus authorized positive privacy control |
| parsed.times first_object,preview,full; raw BPM; strains/breaks/kiai/timing data | H `/resources/file` selected reference + surface bounded bytes + osu calc | R/-; owned derived generation | Resource-derived typed map, not private payload; SC mapKiaiPoints remains [] despite nonempty computed kiai; v1 preserves parsed first-object | Equal filename/different bytes; raw-vs-rounded BPM |
| SC literal placeholders: isRunning,isBreakTime,songSelectionScores,songSelectionMainPlayerScore,songSelectionTotalScores,osu99_9PP,osuM99_9PP,mania1000000PP,maniaM1000000PP,plays,tags,source,starsNomod,comboLeft,localTime,localTimeISO,sl,sv,threadid,test | Adapter constants, no producer read | -/- | Exact pinned constants/empty JSON strings, not fabricated source facts | Raw source-oracle bytes |

For every lossy projection (division, rounding, clamping, truthy omission,
legacy hit conversion), use the original accurate fact above or derive from
the authorized chart; never assume an inverse. Required equivalence evidence
includes the complete oracle, signed zero, values either side of decimal
rounding ties, all representable source integer boundaries, nonfinite source
handling and randomized finite source values. A failing proof blocks that
consumer's parity exit; it does not authorize a new divergence.

## Endpoint-by-endpoint removal inventory (A4.6)

Paths below are wire JSON Pointers, not bus pointers. `*` matches every array
element, including every stable spectator and every lazer client. Apply to
DTO fields, direct writers and filtered writers alike. Removed means absent,
not null, empty, gated behind a retired flag, or stripped by a test helper.

| Endpoint / feed | Remove | Preserve / replacement |
|---|---|---|
| `/json/v2`, `/websocket/v2` | `/keys` entirely; `/play/pressHitErrors`, `/play/releaseHitErrors`, `/play/keyHitErrors`, `/play/hitOffsets`, `/play/catcher`; same extension leaves under `/tourney/clients/*/play` | Legitimate `/play/hitErrorArray`; other pinned fields, errors/order/filter behavior |
| `/json/v2/precise`, `/websocket/v2/precise` | `/keys/all`, `/pressHitErrors`, `/releaseHitErrors`, `/keyHitErrors`, `/hitOffsets`, `/catcher`; `/tourney/*/keys/all`, `/tourney/*/pressHitErrors`, `/tourney/*/releaseHitErrors`; any extension keyHitErrors/hitOffsets/catcher nested copy | `/keys/{k1,k2,m1,m2}`, `/hitErrors`, `/tourney/*/{ipcId,keys,hitErrors}` with upstream order |
| `/json`, `/ws` (gosuMemory) | `/gameplay/hits/pressHitErrors`, `/gameplay/hits/releaseHitErrors`; copies under `/tourney/ipcClients/*/gameplay/hits` | `/gameplay/hits/hitErrorArray`, `/gameplay/keyOverlay/{k1,k2,m1,m2}` and legitimate tournament equivalents |
| `/json/sc`, `/tokens` | `/pressHitErrors`, `/releaseHitErrors` (Rust source `press_hit_errors`, `release_hit_errors`) | `/hitErrors`, `/keyOverlay` embedded JSON string and its real upstream contents |
| `/websocket/v2/subscribe` | Entire route, transport/classification and selected-field-only projector machinery | Exact ordinary removed-route 404, no upgrade/static success; API-v1 subscription remains |
| `/files/skin/` | Legacy metadata index only | Exact 404; retained-root private lookup index and API-v1 trees remain |
| `/files/skin/{path}` on lazer | Legacy successful lookup/delegation | 404; API-v1 lazer skin tree/resource stays functional |
| `/files/skin/{path}` on stable; `/files/beatmap/*`; `/Songs/{path}`; `/backgroundImage` | No additional removal | Pinned selection/error/method/range policy over declared roots; no naked path opens |
| `/api/calculate/pp` | Arbitrary external absolute-path authority only | Current selected session: relative child, or absolute spelling canonicalizing within one of its declared roots; otherwise existing invalid-path rejection envelope; `legacy-calculate-declared-roots` security divergence |
| `/api/ingame` | No route removal and no upstream page assets | Existing endpoint remains functional; clean-room replacement and D20 page UX are separate scheduled work |
| Every adapter configuration reader/reload/settings UI | `ENABLE_KEY_OVERLAY`, `ENABLE_MANIA_KEY_HIT_ERRORS`, `ENABLE_HIT_OFFSETS`, `V2_SUBSCRIPTION_DEBOUNCE` | Actual WS selectors drive demand; retain `API_V1_SUBSCRIPTION_DEBOUNCE` and unrelated unknown keys; M8 one-time migration deletes exactly the four retired inputs |

Filters selecting a removed key observe normal pinned absent-key behavior;
they cannot resurrect extension data. X0 deletes `strip_vosu_extensions` and
ad hoc regular-keys normalization. Nonempty extension source values must be
present in the bus fixture while their raw emitted keys are absent.

State-schema differences (`bestOf`, `roomId`, accurate null rather than whole
response failure) authorize **no** adapter-wire deviation: restore bestOF /
roomID and use availability to distinguish omission from failure. The stable
manager temporary entry remains until T0 supplies its actual evidence; A4
does not mark it closed. No other temporary exemption is approved.

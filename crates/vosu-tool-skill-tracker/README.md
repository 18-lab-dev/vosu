# Skill Tracker

First-party vosu tool (`skill-tracker`, protocol v1). Runs continuously while enabled, requires `difficulty` and host `context` permission. No account, cloud storage, browser build step, or runtime beyond the native service. Develop with `cargo build -p vosu-tool-skill-tracker --locked` and `vosu --tool crates/vosu-tool-skill-tracker`.

## Command line

Use `vosux skill-tracker` (or `vosu x skill-tracker`) with:

- `profile [--json]`: ratings, dan estimate, play counts and model notices.
- `history [--last N] [--mode osu|taiko|fruits|mania] [--json]`: newest plays, default 50, maximum 200.
- `last [--json]`: latest persisted play, including MSD/SSR, accuracy and Wife model; JSON `null` if there are no plays.
- `export [--format json|csv]`: stream full history, newest first, to stdout. JSON is an array; CSV includes every play field, with nested values encoded as JSON cells and RFC-style quoted/escaped strings. Empty CSV exports still include the same header row. Export is paginated rather than loading the whole database.
- `import [--json]`: run the existing stable import job with stderr progress; Ctrl+C sends cancellation and exits 130, failure exits 1.

All commands support `--help`, use the CLI token through the vosu proxy, and leave database writes to the service. Human profile output includes a two-decimal skill rating table, dan and play counts. History/last show aligned date/map/rate/accuracy/MSD/SSR/result tables with each play's accuracy model. No ANSI color is emitted; progress updates in place only on a terminal. Errors use `vosux skill-tracker: <message>` with service explanations and HTTP context. `--json` preserves service response shapes. Without the `cli` subcommand the binary retains its continuous service behavior.


## Recording and persistence

The service subscribes to `/websocket/v2/subscribe` with the ordered selectors `/state`, `/client`, `/beatmap`, `/play`, `/resultsScreen`, `/game/paused`, `/session/playCount`, `/rankedPlay`. It reconstructs both initial `[0,revision,pairs]` and delta `[1,revision,pairs,removed_ids]` frames. Stable and lazer use the normalized v2 state: play `2`, results `7`. Pause does not finish a play. Leaving play records a quit, an observed `play.failed` remains failed, results without a failure records a pass, map change and retry record the prior attempt as a quit. A clock rewind of >500 ms with reduced score, or increased session play count, identifies a retry within play state. A lost subscription records an `interrupted` attempt, excluded from ratings; it never guesses a pass across missing frames. Empty attempts are discarded.

At play start the selected host context must match the active MD5. Its `.osu` is read immediately (maximum 1 MiB) and retained for the attempt. Difficulty's returned MD5 must match too. Calculation uses `POST /tools/difficulty/api/v1/calculate?rate=<actual rate>&score=<wife>` with the tool token and raw bytes. Unsupported maps, missing files, changed checksums, and provider failures remain visible in history, with `calculation_error` rather than fabricated ratings. A bounded 16-attempt calculation queue keeps slow calculations from blocking the live subscription; saturation retains the play without difficulty and explicitly labels the error. Reconnection backs off 1, 2, 4, 8, 16, 30 seconds, reset after a valid frame. Native star rating and PP are copied from v2 for non-mania plays, which never enter MSD ratings.

`VOSU_TOOL_DATA_DIR/plays.sqlite3` uses bundled SQLite, WAL, synchronous FULL, a versioned transaction migration (`user_version=1`), and a five-second busy timeout. Disabling the tool leaves all data untouched. A newer unknown schema is rejected rather than overwritten. `DELETE /v1/plays/<id>` permanently removes one local record and recomputes the affected chart's contributions. Historical aggregate trend points remain historical observations, not rewritten retroactively. A session means one service process lifetime; tool restart starts a new session, not a new history.

## Accuracy: Wife3, not osu! accuracy

Etterna SSR takes a Wife-style fraction, not osu!mania's displayed accuracy. Every play exposes both `accuracy` (osu! percentage, 0–100) and `wife: {value, model}` (0–1). This is an experimental cross-game conversion, not an official Etterna score.

The timing curve is the J4 / timing-scale 1 Wife3 function from [Etterna 0.75.1 `RageUtil.h`](https://github.com/etternagame/etterna/blob/2d706147cc4bcf95a8ceb24dc4f4129be207f803/src/RageUtil/Utils/RageUtil.h), revision `2d706147cc4bcf95a8ceb24dc4f4129be207f803`, under MIT; see `LICENSE-ETTERNA`.

For absolute offset `x` in **milliseconds**, the tap points are:

- `x ≤ 5`: 2;
- `5 < x ≤ 65`: `2 * erf_approx((65-x)/22.7)`;
- `65 < x ≤ 180`: `-5.5 * (x-65)/115`;
- `x > 180`: -5.5.

`erf_approx` is Etterna's Abramowitz–Stegun 7.1.26 polynomial, including its small numerical residue at zero. Wife is `(sum(tap points) - 5.5*misses)/(2*judgment count)`, clamped to [0,1] for the difficulty API. Negative raw Wife therefore saturates to zero. Complete finite `play.hitOffsets` (enabled by the daemon's `ENABLE_HIT_OFFSETS`) must contain exactly one value per successful judgment. When that invariant holds the model is `wife3-j4-offsets`; misses are penalized separately. No partial rolling hit-error array is passed off as a full replay. Long-note hold/drop penalties and osu!'s extra release judgments are not separately reconstructed: if count agreement fails, the tool uses the approximation below.

Without complete offsets, each judgment is represented by the midpoint of its timing bin (`geki`, `300`, `katu`, `100`, `50`), bounded by v2 `beatmap.stats.hitWindow.{perfect,great,good,ok,meh}`. The model is `wife3-j4-judgment-midpoints`. Imported scores have no replay timings or original hit windows; their explicit model is `wife3-j4-judgment-midpoints-od8`, using [16,40,73,103,127] ms upper boundaries (stable mania OD8). This is not an inverse of osu! accuracy: two plays with identical counts but different timing distributions can have different true Wife. The UI labels offset vs approximate conversion on every mania row. Dan qualification deliberately uses **osu! accuracy ≥96%**, not approximated Wife.

Rates prefer v2's normalized `mods.rate` (including lazer custom rates), then lazer `settings.speed_change`, then DT/NC = 1.5 and HT/DC = 0.75, otherwise 1.0. Provider rate-range/rounding limitations are returned as calculation errors or visible in `difficulty.rate`; the recorded actual `rate` is never rounded. Auto/relax/autopilot, chart-altering key/random/mirror/difficulty mods, and variable-speed mods are excluded from ratings; unsupported converts remain in history. Raw mods are retained. Only passed, eligible mania plays with provider SSRs contribute.

## Player aggregation (Etterna 0.75.1)

Source: pinned [`MinaCalcHelpers.h::aggregate_skill`](https://github.com/etternagame/etterna/blob/2d706147cc4bcf95a8ceb24dc4f4129be207f803/src/Etterna/MinaCalc/MinaCalcHelpers.h), [`ScoreManager.cpp::CalcPlayerRating`, `ScoresForChart::SetTopScores`, `SortTopSSRPtrs`](https://github.com/etternagame/etterna/blob/2d706147cc4bcf95a8ceb24dc4f4129be207f803/src/Etterna/Singletons/ScoreManager.cpp). Note that this version uses the **arithmetic mean of seven skillsets** for player overall, not the older overall aggregation with 1.125 multiplier.

For each chart MD5, keep the highest-Wife eligible score at each rate, then keep the two with largest overall SSR across rates (Etterna's PB/top-score rule). For each of seven skillsets, search for rating `r` at the threshold

`2^(0.1*r) >= sum(max(0, 2/erfc(0.1*(SSR-r)) - 2))`.

The upstream float-resolution search starts at `r=0`, step 10.24, advances until the threshold, steps back, halves the step, and repeats for eleven iterations. Add twice the final half-step and multiply by 1.05; clamp to [0,100]. No contributing scores produces exactly zero. Overall is the mean of stream, jumpstream, handstream, stamina, jackspeed, chordjack and technical. The affected chart's best-score index is updated after insert/delete; rating recomputation runs only on history changes, never on live polling frames. Startup reconstructs the index from durable history.

Worked references from the independent C++ port `tests/aggregation-reference.cpp`:

| Per-skillset input SSRs | Aggregate |
|---|---:|
| [20] | 15.077999115 |
| [20,20] | 16.779001236 |
| [10,20,30] | 23.729999542 |

For seven skillsets each with one SSR 20, overall is 15.077999115. These values are exercised by Rust unit tests. Different supported mania key counts currently share one profile; they are not asserted equivalent to Etterna dance-single chart validation, chord-cohesion checks, or online leaderboard eligibility. The UI/API label the model `etterna-0.75.1-aggregate-skill`, with the cross-game disclaimer. Dan is the highest provider `dan.value` among eligible passed mania plays at ≥96%, with the provider's system/label and `experimental=true`; it is a map estimate, **not an official dan course clear**.

## Stable import and limitations

`POST /v1/import` reads `scores.db` and `osu!.db` from the current stable Songs directory's parent, never writes either file, and imports mania records only. Format source: [ppy/osu wiki, Legacy database file structure](https://github.com/ppy/osu/wiki/Legacy-database-file-structure). The bounded reader supports pre-20140609 byte difficulties, pre-20191106 entry-size fields, and 20250107 float star-pair layout, UTF-8/ULEB128 strings, Windows ticks, and Target Practice extra data. It rejects truncation, oversized collections/strings, unsafe paths/symlinks outside Songs, and unrecognized trailing bytes rather than silently misparsing. The original file sizes are limited to 512 MiB each. A concurrently rewritten source database can produce a failed job; retry after osu! finishes saving.

Lookup is by MD5; every found native mania `.osu` is submitted to difficulty. Missing maps are still imported, uncalculated, and counted in job results. Stable mania converts are recorded but excluded from ratings because the raw map does not represent the converted chart. Imports deduplicate by map, timestamp, score, combo and mods. Existing records are retained on cancellation; check progress/result for `inserted`, `missing_maps`, `calculation_errors`, and `total_scores`. Import cancellation is checked between scores, so one active HTTP calculation may take up to its 30-second timeout. A single import can run at a time (409 otherwise); the process retains at most 32 job records. Jobs are process-local; play history is durable.

Lazer's Realm is not opened or modified: there is no reliable documented stable read-only export path implemented here. **Lazer import is unavailable**, explicitly shown in the UI. New lazer plays are recorded live through the same normalized daemon API.

## API v1 for overlays

Prefix `/tools/skill-tracker/api`. All service-to-service/direct requests require `X-Vosu-Tool-Token`; the vosu proxy adds it for browser/overlay requests. Read prefixes below are public to allowed overlay peers. Mutations require a bridge UI session or authorized dependency token. UI consumers use `await vosu.ready` then `vosu.fetch("v1/profile")`, and gate write controls on `vosu.canWrite`.

### GET `/v1/profile`

```json
{"ratings":{"overall":15.078,"stream":15.078,"jumpstream":15.078,"handstream":15.078,"stamina":15.078,"jackspeed":15.078,"chordjack":15.078,"technical":15.078},"dan":null,"dan_accuracy_threshold":96.0,"dan_experimental":true,"play_counts":{"mania":1},"rated_scores":1,"updated_at":"2026-10-21T10:00:00Z","trend":[{"timestamp":"2026-10-21T10:00:00Z","overall":15.078}],"rating_model":"etterna-0.75.1-aggregate-skill","accuracy_notice":"Wife3 J4 from complete offsets; otherwise judgment-midpoint approximation. Not official Etterna ratings."}
```

`play_counts` omits absent modes. `updated_at` is null before the first history change. `trend` contains newest-first last 60 insert/delete observations. `dan` is null or the dependency's `{system,label,value,experimental}`.

### GET `/v1/history?limit=50&before=<id>&mode=mania`

`{"plays":[Play],"before":123}`. Newest **insertion ID** first, exclusive integer `before` cursor; omit on first request. `limit` caps at 200. `mode` is `osu|taiko|fruits|mania`, or omit for all; invalid mode is 400. The next cursor is the final returned ID, null when empty. `GET /v1/plays/<id>` returns one full `Play`, 404 if absent.

```json
{"id":123,"checksum":"md5","title":"Title [Difficulty]","mode":"mania","keys":4,"client":"lazer","mods":{"number":0,"name":"NM","array":[],"rate":1.0},"rate":1.0,"judgments":{"geki":100,"300":2,"katu":0,"100":0,"50":0,"0":0},"accuracy":100.0,"wife":{"value":0.999,"model":"wife3-j4-offsets"},"score":1000000.0,"max_combo":102,"outcome":"passed","timestamp":"2026-10-21T10:00:00Z","source":"live","difficulty":null,"calculation_error":null,"stars":4.5,"pp":100.0,"eligible":true}
```

`difficulty` is null or the complete Difficulty API v1 object, including `msd`, `ssr`, `dan`, `calculator`, and `unsupported`. `stars`/`pp` are null if unavailable. `keys` is zero if unavailable. `outcome` is `passed|failed|quit|interrupted`; `source` is `live|stable-import`. `eligible` describes chart/mod eligibility, not qualification on its own; ratings additionally require passed mania + non-null SSR. No local map bytes or filesystem paths are returned.

### GET `/v1/current`

`{"plays":8,"rating_deltas":{"overall":0.18,"stream":0.20,"jumpstream":0.12,"handstream":0.15,"stamina":0.18,"jackspeed":0.16,"chordjack":0.17,"technical":0.20},"last_play":Play|null,"connection":"connecting|connected|reconnecting"}`. Plays/last_play are this process's recorded live attempts, not imports. Rating deltas are relative to startup, and reflect imports/deletions too. Deleting the most recent play clears `last_play`; it does not erase the session attempt counter.

### WebSocket `/v1/stream`

Full `{"profile":Profile,"current":Current}` snapshot immediately on connect and after each history/session/connection change. Latest-state delivery coalesces slow clients; consumers replace their whole snapshot. Same-origin browser example:

```js
const url = new URL('/tools/skill-tracker/api/v1/stream', location.href);
url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
const socket = new WebSocket(url);
socket.onmessage = event => render(JSON.parse(event.data));
```

### Mutations and jobs

- `POST /v1/import` (no body): 202 `{"job":"<id>"}`; requires selected stable client and available Songs context; 400 when unavailable, 409 for another active import.
- `GET /v1/jobs/<id>`: `{"state":"queued|running|done|failed|cancelled","progress":0.5,"message":"Importing mania scores","result":{"inserted":40,"missing_maps":2,"calculation_errors":2}}`; result can be null. Unknown ID →404.
- `DELETE /v1/jobs/<id>`: requests cancellation and returns the current job shape; cancellation becomes observable at the next boundary. Completed jobs are unchanged.
- `DELETE /v1/plays/<id>`: 204; 404 if absent. This deletes only the tool record, never an osu! score/file.
- Application errors: `{"error":"message"}` with non-2xx status. Authentication is enforced by the SDK and host, not by browser UI controls alone.

## Verification

```sh
cargo fmt -p vosu-tool-skill-tracker
cargo clippy -p vosu-tool-skill-tracker --all-targets --locked -- -D warnings
cargo test -p vosu-tool-skill-tracker --locked
cargo build -p vosu-tool-skill-tracker --locked
python crates/vosu-tool-skill-tracker/tests/smoke.py
env -u C_INCLUDE_PATH cargo xwin clippy -p vosu-tool-skill-tracker --all-targets --locked --target x86_64-pc-windows-msvc -- -D warnings
```

The smoke requires Python `aiohttp`, binds only test port 25550, launches the built service with private HOME/XDG/data directories, replays a selected-field v2 play/result sequence, checks authenticated health/profile/history/live snapshots/deletion, and saves `target/skill-tracker-smoke.json`. It optionally probes the user's existing `:24050` WebSocket **read-only** for selector presence; no daemon restart/configuration/input. UI fixture `python crates/vosu-tool-skill-tracker/tests/ui_fixture.py` binds 25551 and serves mock bridge/data. Open it only in a **headless** browser, use `?write=1` for the write-gated preview, and save screenshots under `target/`. This test fixture is not a production API fallback.

# Difficulty provider provenance

## Calculator selection

The compiled calculator is **MinaCalc 527** from Etterna revision
[`2d706147cc4bcf95a8ceb24dc4f4129be207f803`](https://github.com/etternagame/etterna/tree/2d706147cc4bcf95a8ceb24dc4f4129be207f803/src/Etterna/MinaCalc),
retrieved 2026-10-21. License: MIT; copyright (c) 2016–2023 Etterna.
`vendor/LICENSE` retains the full grant. `vendor-files.txt` lists every imported
file and SHA-256 of the vendored bytes. The source paths are
`src/Etterna/MinaCalc/**`, `src/Etterna/Models/NoteData/NoteDataStructures.h`,
and root `LICENSE`, mapped to `vendor/`.

One local patch surrounds the `load_calc_params_from_disk(true)` call in
`Calc::InitializeHands` with the same `!STANDALONE_CALC && !PHPCALC` guard
used elsewhere upstream. Without this guard upstream standalone compilation
fails because the method is not declared. No difficulty arithmetic is modified.
`src/minacalc.cpp` is a project-authored exception-safe C ABI; `build.rs` uses
`cc` and a C++20 compiler, with no bindgen, libclang, Git, or downloads at build time.

Alternatives evaluated:

* [Glubus/minacalc-rs](https://github.com/Glubus/minacalc-rs/tree/0e39361919080e8f883ea9fd0ce99b16a78cb647)
  is MIT, wraps version **515**, and its sys build script invokes bindgen/libclang
  and Git to patch a temporary source tree. It is neither the latest upstream
  calculator nor suitable for our compiler-only build contract. None of its code
  is vendored.
* [`etterna` 0.1.0](https://docs.rs/etterna/0.1.0/etterna/) exposes a player skill
  timeline, not a current chart calculator. The suggested `minacalc` Rust crate
  could not be established at docs.rs (404); no verified pure-Rust 527 port was
  found. We do not substitute an unverified port for upstream algorithm fidelity.

## Input semantics

`rosu-map =0.2.1` parses native mania. Only 4K, 6K and 7K are exposed, even though
this upstream revision also contains 5K logic. Keys must be integral CS.
Column is `floor(x * keys / 512)` with x=512 clamped to the last column, matching
osu's right-edge behavior. Equal float-second timestamps are merged with OR;
input timestamps remain unscaled and playback rate is passed separately.

**Long notes contribute heads only.** MinaCalc's `NoteInfo` has only a column
bitmask and time, not a hold duration or release flag. Etterna's serialized input
uses tap/hold-head/roll-head rows, not release events. Artificial tail taps would
invent jack/stream difficulty; this adapter does not do so. MSD/SSR therefore
measure the chart's *rice/head pattern*, not LN release, overlap, or holding
strain. This limitation also applies to the experimental dan estimate.

MSD calls `CalcMain` with `ssr=false` and canonical 0.93 goal. SSR uses upstream
`MinaSDCalc` with the supplied Wife-style score goal, including upstream goal
caps and low-accuracy/grind scaling; this is not osu!mania accuracy. Notes are
limited to 0–24000 seconds, ensuring the slowest allowed 0.5 rate remains below
upstream's 100000 half-second interval bound. Upstream dense/joke-chart skipping
behavior is retained, including all-zero results for rejected interval density.

## Experimental Reform model

Model id: **`reform-v1-course-msd527@1`**. This is a deliberately transparent
piecewise-linear interpolation of overall MSD against ten *actual course*
measurements, not fabricated thresholds or a trained player-performance model.
`calibration.json` contains computed 1.0x overall MSD for Reform v1 dans 1–10.
Dans 1–6 use the 1st Pack; 7–10 use the 2nd Pack. The duplicate 6th in 2nd Pack
is deliberately not used. Values are rounded to six significant decimal digits.
`calibration-sources.json` records archive URL, exact filenames, MD5 and SHA-256.
Charts/audio are **not redistributed**; their authors retain their rights.

Source: [Emik's DDMythical-authorized archive](https://sites.google.com/view/danreform/home),
[archive ZIP](https://cdn.emik.dev/archive/dan/all.zip),
[original project thread](https://osu.ppy.sh/community/forums/topics/597350).
The archive explicitly warns that metadata is modified and course versions are
not interchangeable. This model describes only these v1 course charts.

Reproduce: download `all.zip`, extract `1st Pack - v1.osz` and `2nd Pack - v1.osz`
as ZIPs, verify each selected `.osu` against `calibration-sources.json`, then run
`cargo run --locked -p vosu-tool-difficulty --example calibrate -- <chart.osu> ...`
in ascending dan order. The example uses the exact production parser and native
calculator and prints computed values without changing model files. Calibration
was independently computed with a standalone C++20 reference driver calling
upstream `Calc::CalcMain(notes, 1.F, 0.93F)`, `keycount=4`, `ssr=false`, head-only
rows grouped by float seconds; production computations reproduce these anchors.

Between anchor MSDs A[n] and A[n+1], value = n + (MSD-A[n])/(A[n+1]-A[n]).
The label is `Reform floor(value)`. No extrapolation: below dan 1, above dan 10,
non-finite values, and 6K/7K return `dan:null`. Estimation uses rate-specific MSD,
not the requested score goal. `experimental:true` is always set. A single map
cannot certify a course clear: stamina distribution, breaks, LN strain and
osu!mania's 96% course-clear accuracy rule are not modeled. These are similarity
estimates, not player ranks or clear guarantees.

Research alternatives: [Companella](https://github.com/Leinadix/companella/tree/1f0af45835d044d98ac164bc0c0e879df757051b)
is MIT (copyright 2025 Leinadix). Its `DanModelService` loads an ONNX gradient
boosting regressor requiring eight MSD features plus Interlude and Sunny ratings;
its shipped `dans.json` contains -1 unset anchors. We reuse neither its model nor
its code. Its Daniel implementation credits TheBagelOfMan under MIT, but adds a
separate strain calculator, not a simple calibration of current MinaCalc.

## Golden fixture

`tests/fixtures/cyclic.osu` is project-authored synthetic data dedicated to CC0:
1200 cyclic single notes, 100 ms spacing, starting at 1 second, lanes 0,1,2,3.
The golden **overall MSD = 6.11** at 1.0x was produced by the standalone upstream
C++ reference invocation above (not by an alternate formula). The Rust golden
checks absolute tolerance 0.0001 to accommodate compiler floating-point variation.

## Service and cache

`tool.toml` is service-only; there is no pretend settings file or UI. SQLite
(`rusqlite` bundled SQLite) in `VOSU_TOOL_DATA_DIR/difficulty.sqlite3` retains
submitted bytes and JSON results. Cache identity includes body MD5, rounded rate
in 0.05 ticks, exact f32 score bits or absent score, calculator revision/adapter
id, and dan model id. Tables survive process restarts and model revisions.
GET lookup can compute new rates from the stored bytes. Missing MD5 is 404.
The `/rates` table uses MinaCalc's 14 standard 0.1-spaced rates 0.7–2.0.

One blocking worker bounds native working sets; sixteen admitted requests bound
queued work, with 503 beyond that. Every worker rechecks SQLite after acquiring
the shared permit, so concurrent identical requests calculate once. Dropping an
HTTP request does not abandon a blocking computation before its cache write.
Parsing, disk reads, SQLite and native calculation are all off the async runtime.

Verification: `cargo fmt -p vosu-tool-difficulty`, native and Windows cross-Clippy,
`cargo test -p vosu-tool-difficulty --locked`; protocol smoke:
`python crates/vosu-tool-difficulty/tests/smoke.py target/debug/vosu-difficulty [chart.osu]`.
The smoke uses a temporary copied map/data directory and mock host on 25350,
checks token rejection, calculate, cached/current lookup, rates, errors,
concurrent dense-chart timing and stdin-close shutdown without touching live vosu.

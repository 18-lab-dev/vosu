# vosu interview evidence: authorship, history, and a debugging case study

Prepared 2026-10-09 from repository and GitHub records with AI assistance (Claude). It is an evidence summary, not a contribution statement: every personal claim below remains unconfirmed until the maintainer answers [the questions at the end](#maintainer-confirmation-required).

Evidence revision: `f075408209ee98cdd780f3a4d386d2a9850f40b0` (real HEAD, 2026-10-08). Commit `9106c65e627479971d33fbcbeaf735ffb308407f` is a synthetic snapshot of uncommitted checkout changes (5 files, +234/−23) created for this review; it is excluded from all commit, author, and size attribution below.

## Short account (verified facts only)

- vosu is a native Rust daemon targeting API and behavioral compatibility with upstream [tosu](https://github.com/tosuapp/tosu). The README describes it as "vibe-coded" and as "designed to work well with coding agents" (`README.md:14`, `README.md:32`). That disclosure has been present since the first commit, `7a49fda2b1da0169ee59862a11310df69c3ab3ff`, and must stay in any account.
- The first commit, titled `Initial commit`, records a large initial tree: 811 files, 269,615 inserted lines, 125,276 Rust lines. It already contains agent guidance, the product rename and a historical cutover changelog entry. Recovered authoring sessions show an agent initializing Git and committing preceding work on 2026-07-16; this was not an imported repository history. Pre-commit development is absent from Git but partially recoverable from those sessions.
- That snapshot originally called the project a "ground-up Rust rewrite of upstream tosu". `608ca287f0e639cee49082a6e1f4a1a5a4638ef6` changed this to "targeting API and behavioral compatibility". Do not reuse "ground-up rewrite" wording; `THIRD_PARTY_NOTICES.md:10` and `docs/provenance-audit.md` record imported tosu dashboard material, an LGPL calculator wrapper, and unresolved provenance questions.
- Every commit uses the same GitHub noreply address, `212748636+qt715@users.noreply.github.com`, under two display names: `qt715` and `Sxlvia`.

## History and size counts

All counts use `R=f075408209ee98cdd780f3a4d386d2a9850f40b0` and author dates. Commands run from the repository root.

| Measure | Value at `R` | Command |
|---|---:|---|
| Commits reachable from `R` | 227 (213 non-merge, 14 merges) | `git rev-list --count $R`; add `--merges` / `--no-merges` |
| Commits after the initial snapshot | 226 (212 non-merge) | `git rev-list --count 7a49fda..$R` |
| Author names | `qt715` 224, `Sxlvia` 3 — **one email** | `git shortlog -sne $R` |
| Committer names | `qt715` 224, `GitHub` 2 (web merges), `Sxlvia` 1 | `git shortlog -sne -c $R` |
| `Co-authored-by` trailers | 0 | `git log $R --format=%b \| grep -ci co-authored-by` |
| Span from first to last commit | 84.1 days (12.0 weeks): 2026-07-16 → 2026-10-08 | `git log --reverse --format=%aI $R \| sed -n '1p;$p'` |
| Commits in first 30 days (from the snapshot's timestamp, snapshot included) | 149 total, 142 non-merge | count `%aI` in [2026-07-16T19:32:17-04:00, +30 days) |
| Rust lines (physical, all tracked `*.rs`) | 144,851 in 482 files | `git grep -c '' $R -- '*.rs' \| awk -F: '{s+=$NF} END {print s}'` |
| Rust lines in the initial snapshot | 125,276 | the same command with `7a49fda` |
| Rust lines at `R` last touched by the snapshot (`git blame`) | 103,325 (71.3%) | `git blame --porcelain $R -- <file>`, summed over every `*.rs` file |
| Rust churn after the snapshot | +50,237 / −30,634 | `git log --no-merges --numstat --format= 7a49fda..$R -- '*.rs'` |

Rust lines at `R` by path class. The classification is a path heuristic: inline `#[cfg(test)]` modules in ordinary files remain under "other".

| Class | Lines | Path rule |
|---|---:|---|
| Vendored MIT calculator forks (including their own tests) | 42,210 | `crates/vosu-pp/rosu-pp-api/`, `crates/vosu-pp/rosu-map-api/` |
| Test, benchmark, example, and harness files | 42,217 | other files under `test/`, `tests/`, `benches/`, `examples/` directories; files named `test.rs`, `tests.rs`, `test_*.rs`, `tests_*.rs`; `crates/vosu-test-harness/` |
| Other Rust | 60,424 | everything else |

### Reconciling the earlier figures

Claude's earlier numbers were correct for a stale revision. They should not be cited for current `R`.

| Earlier claim | Reproduced? |
|---|---|
| About 145k Rust lines | Yes: 144,851 physical lines. |
| 224 commits; authors 221/3 | Exactly matches `85d053b` (2026-10-04, `R~3`). At `R`, the counts are 227 and 224/3. |
| About 11 weeks | 79.9 days (11.4 weeks) to `85d053b`; 12.0 weeks to `R`. |
| 142 commits in the first month | Matches non-merge commits in the first 30 days, snapshot included. The total including merges is 149. |
| History begins in an imported snapshot | Git begins with a large initial tree, but recovered sessions show an agent committing preceding work rather than importing a repository history. |

### What these counts do and do not establish

- **Git identity is not authorship.** Author/committer fields record the configured name and email, not whether a human typed the code, an agent generated it, or a person reviewed it. No trailers record AI assistance. The 224/3 split is two display names on one GitHub account, not two people.
- **Commit counts measure granularity, not effort.** Fourteen commits are merges, and several are bulk integrations, such as `e0bb23e` "Snapshot verified repository cleanup…", which changed +11,502/−16,289 lines.
- **Line counts are not authored-code counts.** 29% of current Rust lines are vendored forks. A similar share is tests, benchmarks, and harness code, and 71% of current lines are attributed by `git blame` to the initial snapshot. Blame records the last commit to touch a line; it does not establish who authored it or which source was used.
- **Git omits pre-commit development.** Recovered sessions document preceding agent work, including translations and later rewrite attempts. Neither that evidence nor post-snapshot commit activity establishes personal authorship or "built from scratch in 12 weeks".

## Documented AI assistance

These records show that AI tooling was present. They do not identify which agent or person produced a particular change.

- `README.md:14` says "vibe-coded"; `README.md:32` says the repository is designed so automated changes stay reviewable.
- The snapshot contains `CLAUDE.md` (content: `@AGENTS.md`), `.claude/skills/` links to `.agents/skills/` (Rust and systematic-debugging skills), and `skills-lock.json`.
- `248916c` merges a branch named `codex/additional-logging`.
- `690ca55` "WIP: checkpoint before autoresearch", `a3136cb`/`feb0703`/`9211854` "autoresearch: harness setup", plus `f298b9e`/`1ab55ae` "Integrate … autoresearch lanes/histories". `9211854` names `autoresearch.sh` as the benchmark entrypoint, with an optimization goal. The tooling that generated these commits is not recorded.
- `f075408`'s message says it retires "stale Claude guidance".

## Case study: osu!lazer mania hold releases classified as presses

This is the best-documented difficult bug in the inspected record. It has a public symptom report, an explicit list of competing hypotheses, a first fix that passed its tests but failed live measurement, a quantitative live counter-check, and a structurally different final fix.

### Symptom

[Issue #1](https://github.com/shiruviaa/vosu/issues/1) (opened 2026-07-23): on osu!lazer `2026.711.0` mania plays, `play.hitErrorArray.release` stayed empty and every timing error was published as a press. Readings were `all=827, press=827, release=0` and `all=845, press=845, release=0` on a map with 285 holds. A freshly started daemon re-reading the full event list gave the same result, so the fault was not reducer accumulation alone.

### Competing hypotheses (recorded in issue #1)

1. The hold-aggregate marker was not recognized. `read_hit_event` identified mania objects by hard-coded managed `BaseSize` values, `0x58` for timing and `0x88` for the hold aggregate.
2. The event-order assumption was wrong. `read_hit_error_suffix` labelled a timing as a release only when the **next** decoded event was a hold aggregate.

### First fix, accepted on synthetic evidence, then disproved live

- `21a371df90aced324ec70d20ae8f48794928af7c` (via [PR #2](https://github.com/shiruviaa/vosu/pull/2) / `b6e7ea9b555734524a4fe91e46724428ec907528`) acted on hypothesis 1. It also accepted `BaseSize 0x90` as a hold aggregate, with a comment attributing that size to 2026.711.0's HoldNote layout, and added a `mania-hold` live-parity scenario.
- PR #2 records fmt, clippy, workspace tests ("1029 passed, 2 ignored"), `process_memory`, and Python parity tests. It also states that **no post-fix live read** was possible because `CAP_SYS_PTRACE` could not be granted non-interactively. The tests used `0x90` fixtures built from the hypothesis.
- PR #3 (`3fecc9c`, merged by `3317ee6`) corrected the new parity gate. Its fixtures had modelled upstream hit errors as partition objects instead of flat arrays.
- On 2026-07-24, the live retest recorded in issue #1 completed a replay of a map with 1203 circles, 1154 holds, and 70 misses: `all=3441, press=3373, release=68`. The `all == press + release` invariant passed, but two bounds failed:
  - `press <= circles + holds`: 3373 ≤ 2357 fails by 1016.
  - `release >= holds − misses`: 68 ≥ 1084 fails by 1016.

  The identical excess and deficit showed that 1016 tail timings were still misclassified. The partition was internally consistent but wrong.

### Decisive diagnosis and change

`81a4b56ffb28a77afaf93522274da4715f251130` ("classify mania hold releases by object identity", 2026-07-28) replaced both earlier assumptions:

- **Measured layout instead of an inferred size.** The code comment records a `DynamicMethod` `ldflda` measurement on installed osu! 2026.711/CoreCLR 8.0.28 and 2026.726/CoreCLR 8.0.29. HoldNote has `BaseSize 0x88`; its `Tail` backing field is at `+0x60`, and `+0x68` is `Body`. `0x90` acceptance was removed: a `0x90` object now decodes to `None`.
- **Identity instead of adjacency or value.** Each timing event keeps its `HitObject` pointer, and each hold aggregate keeps its `tail` pointer. A timing is a release exactly when a hold aggregate in the same suffix names that timing's object as its tail. This works for chords and interleaved columns, where other events can separate a tail judgement from its hold aggregate.
- **Polling boundaries.** If an aggregate arrives in a later poll than its tail timing, the reader scans backward for the tail's object. It emits a `ReleaseCorrection { value, press_offset }`, giving the position among subsequently published presses. The reducer then moves that exact element from `press` to `release`. Before this, the reducer popped the **last** press only when its `f64` bits matched, which can select the wrong one of several equal timing values.
- **Gate strengthened.** `tools/live-parity/parity_compare.py` now enforces both structural bounds from the live retest on completed mania-hold captures. It also rejects missing or malformed object/miss counts.

Current code: `src/memory/lazer/scores.rs:438-496` (`read_hit_event`), `src/memory/lazer/reader.rs:798-` (`read_hit_error_suffix`), `src/state/bundle/reducer.rs:737-` (correction application), and the regression test `mania_suffix_classifies_interleaved_duplicate_timings_by_object_identity` in `src/memory/lazer/tests_core.rs:389`. `CHANGELOG.md:110` records the user-visible change.

### Verification actually on record

- Pre-fix live failures: issue #1 body and PR #2.
- Failed post-first-fix live result: issue #1 comment, 2026-07-24.
- Final fix: unit tests for interleaved duplicate timings, non-mania press-only behavior, and suffix-boundary corrections, plus parity-gate tests whose fixtures encode the 70-miss/1084-release case. All are in `81a4b56`; this review did not run them.
- **Not found:** a recorded live capture after `81a4b56` showing the bounds pass. Issue #1 was closed on 2026-09-07 without a closing comment. No retained capture under `~/.cache/vosu-release-gates-fuobodv5/` declares `"scenario": "mania-hold"`. The `ldflda` measurement program and output are not in the repository; only the code comment remains.
- **Unexplained in the record:** which object type has `BaseSize 0x90`, and why the original adjacency rule produced exactly zero releases before the first fix.

### What to be able to explain (technical summary)

lazer stores judgements in `ScoreProcessor.HitEvents`, a list of `0x40`-byte entries. Each entry has a `HitObject` pointer, a timing offset (`f64`, `+0x10`), and a result (`i32`, `+0x18`). vosu reads the object's CLR `MethodTable` to obtain `BaseSize` as a cheap type tag. A mania hold produces a timing for its tail and a separate whole-hold judgement without a usable timing.

The first design inferred "the timing just before a hold judgement is its release". The first fix assumed a layout change. Both were proxies, and the live structural bounds showed exactly how many tails the proxies missed. The final fix uses the object graph the game itself uses, `HoldNote.Tail`, to match tail timings by pointer identity across polls.

Lesson: when a heuristic marker fails, first measure the real layout and relationship. Encode invariants that a wrong classification cannot satisfy (`press ≤ circles + holds`, `release ≥ holds − misses`), not only invariants that every partition satisfies (`all = press + release`).

**Attribution:** the record shows the GitHub account `qt715` filing the issue and live retest, authoring the PRs and commits, and the README's vibe-coded disclosure. It does not show who formed each hypothesis, ran the `ldflda` measurement, or wrote the code, or whether an agent did so. Do not present this as a personally diagnosed bug until the maintainer confirms it.

### Other candidates considered, not selected

- `26127db683711568baa43398ea1b10645c75eef6` "preserve hit history across play clock rewinds": removes clock-rewind/index-drop hit-history resets and gates hit-error appends on a fresh `Play` status. There is no issue, message body, or recorded symptom or live reproduction.
- `5537f9a90088960b6daaa9848b15a62e1be9c925` "use the client's CLR for offset generation": stages the generator against the game's own runtime. There is no message body or linked failure record. The synthetic baseline contains further uncommitted edits to the same files.

Either would need the missing symptom, hypotheses, and live-verification evidence before serving as a case study.

## Hand-written versus delegated work

| Category | What can be said |
|---|---|
| Verified | Commits, issues, and PRs come from one GitHub account (`qt715`/`Sxlvia`). The project is disclosed as vibe-coded. Git starts with a large initial tree; recovered sessions document preceding agent work. 29% of current Rust is vendored forks. |
| Documented AI assistance | Agent configuration in the snapshot, a `codex/` branch merge, autoresearch commits, and a retired Claude guidance file. Per-change attribution is absent. |
| Requires maintainer confirmation | Any statement that code, diagnosis, design, or measurement was personally hand-written, personally reviewed, or delegated, including in the case study above. |

Evidence-only framing until confirmed: "vosu is an AI-assisted Rust compatibility daemon. Its first commit records a large initial tree; authoring sessions document preceding agent work and subsequent rewrite attempts. Issue #1 documents a bug and how its diagnosis evolved." This describes project records, not personal contribution. First-person claims require the confirmation questions below. Avoid commit or line counts as productivity claims.

## Maintainer confirmation required

1. Where and when did the content of `7a49fda` originate? Is there earlier history, such as a private repository or a tosu fork, that you can describe or reference?
2. Are `qt715`, `Sxlvia`, `shiruviaa`, and the `sxlvia` copyright notice all you? Did anyone else contribute code or reviews?
3. For issue #1, who identified the two hypotheses, ran the 2026-07-24 live retest, performed the `ldflda` measurement, and wrote `81a4b56`? Which tool or agent was used, if any, and what did you review or change yourself?
4. Did you run a live mania-hold capture after `81a4b56`? If yes, where is the report, and on which client version?
5. Which parts would you describe as written by hand, directed through agents with review, or accepted with little review? Give examples with commit IDs.
6. What tooling produced the `autoresearch` commits and the `codex/additional-logging` branch?

## Evidence inspected

- Git history at `f075408209ee98cdd780f3a4d386d2a9850f40b0`: `rev-list`, `shortlog -sne` (author and committer), commit bodies, the root tree `7a49fda`, per-file `git blame --porcelain` for all 482 `*.rs` files, `numstat`, `-S` searches for README wording and `ldflda`, and `git diff --shortstat f075408 9106c65`.
- Commits: `7a49fda`, `21a371d`, `b6e7ea9`, `3fecc9c`, `3317ee6`, `81a4b56`, `608ca28`, `85d053b`, `26127db`, `5537f9a`, `248916c`, `9211854`, `e0bb23e`.
- Files at `f075408`: `README.md`, `CHANGELOG.md`, `THIRD_PARTY_NOTICES.md`, `docs/provenance-audit.md` (header and ownership section), `docs/testing/verification.md` (mania/scenario search), `src/memory/lazer/scores.rs`, `src/memory/lazer/reader.rs`, `src/state/bundle/reducer.rs`, `src/memory/lazer/tests_core.rs`, `autoresearch.sh` (header), and the root-snapshot `CLAUDE.md`/`README.md`/`CHANGELOG.md`.
- GitHub records, read-only: [issue #1](https://github.com/shiruviaa/vosu/issues/1), [PR #2](https://github.com/shiruviaa/vosu/pull/2), [PR #3](https://github.com/shiruviaa/vosu/pull/3), and the issue list (only #1 and #6 exist; #6 concerns attribution).
- Local retained gate artifacts: `~/.cache/vosu-release-gates-fuobodv5/` and `~/.cache/incidents/` searched for `"scenario": "mania-hold"` captures; none found.

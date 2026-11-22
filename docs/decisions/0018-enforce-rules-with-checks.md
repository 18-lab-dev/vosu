# Enforce repository rules with checks, not prose

- Status: Accepted
- Date: 2026-11-19

## Context

Most changes here are written by coding agents. Agents copy the patterns they
find, and they follow written rules unreliably. Before this decision, many
rules in `AGENTS.md` and the maintained docs had no check behind them, and the
tree had drifted from several of them:

- "Push only after every required local check passes" was prose. The `main`
  ruleset blocked only deletion and force-pushes, so CI ran after code had
  already landed.
- The 1,000-line file limit in the source map was exceeded by 28 files.
- The old-brand scan, bounded queues, atomic configuration writes, and module
  ownership relied on each agent re-reading and re-judging the tree.
- Lint suppressions were justified in `//` comments that nothing checked, and
  the native Windows gate script was rewritten from a skill's prose for every
  push.
- Bugs in ZIP end-record handling, updater handoff, and API-state selection
  were each found by hand, and a lazer release broke offset generation until a
  user noticed.

## Decision

1. When an agent is corrected, or a written rule is broken, prefer a
   deterministic check whose failure message says how to fix the root cause.
   Add or keep prose only for judgment that cannot be checked.
2. Lint layer: workspace `[lints]` and `clippy.toml`. Suppressions must be
   `#[expect(lint, reason = "...")]`; `unsafe` blocks need `// SAFETY:`
   justifications. `disallowed-methods` encode the atomic-write, bounded-queue,
   and blocking-sleep invariants, with reasons that name the replacement.
3. Repository checks in CI's `changes` job:
   `scripts/check-file-length.py` (a ratchet whose baseline may only shrink),
   `scripts/check-brand.py` (old-brand occurrences need an allowlist entry with
   a category and reason), `scripts/check-module-boundaries.py` (each crate's
   own module dependency edges are recorded in
   `scripts/module-boundaries/<crate>.txt`; a new edge is a deliberate,
   reviewed change), and `scripts/check-crate-boundaries.py` (the allowed
   dependency edges between workspace crates are recorded in
   `scripts/crate-boundaries.txt` and checked against `cargo metadata`).
4. Landing: `scripts/prepush.py` runs the local Linux, Windows cross-lint, and
   native Windows gates against the exact commit and records evidence.
   `scripts/land.py` refuses incomplete evidence, pushes a `ci/<topic>` branch,
   waits for hosted CI on that commit, and fast-forwards `main`. The `main`
   ruleset requires the `CI required` status check, with no bypass actors.
   A drift test fails when `ci.yml` gains a check that the local gate lacks.
5. Property tests (`prop_*`, proptest) cover the ZIP preflight, updater crash
   recovery, API-state selection, and daemon-owned untrusted-input parsers.
   They use a fixed seed in CI. The nightly workflow runs them with random
   seeds and many more cases.
6. The nightly workflow also runs the lazer offset canary against the newest
   lazer release and the dashboard browser smokes.

## Consequences

Landing takes longer: every change waits for hosted CI on a `ci/` branch
before `main` moves, and emergency fixes take the same path. A new
dependency edge between modules, a new long file, or a new old-brand mention
fails until it is either fixed or recorded with a reason, so reviewers see
these decisions as data diffs. Checks can be wrong; fix a wrong check in the
same change instead of adding an exception without a reason. Agent-facing
prose in `AGENTS.md` now points to the commands that enforce a rule instead of
restating it.

## References

- [Testing and verification](../testing/verification.md)
- [Source map](../architecture/source-map.md)
- [ADR 0011: live parity is optional](0011-live-parity-optional.md)

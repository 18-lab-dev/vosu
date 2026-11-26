# Windows access-violation investigation: `parity_testkit_services_state_resources_and_frames_match_across_runtimes`

## Summary

**Could not reproduce the `0xc0000005` access violation** despite ~100+ real
native executions of the exact failing test on the Windows VM, under several
different concurrency/stress conditions. No code fix was applied: per the
assignment's own rule, the test is not skipped or retried, and no speculative
"maybe this is it" patch was made to `unsafe` code without evidence tying it
to an observed fault. This handoff records the repro methodology, the
non-repro evidence, and the ranked hypotheses for whoever picks this up next
(ideally with access to the original CI machine, or a captured minidump from
a future failure).

## The failure, as reported

- CI run `45846334357` (commit `377854be`, job "Windows MSVC native
  verification", a `cargo nextest run --workspace --lib --bins --tests
  --examples --all-features --locked --target x86_64-pc-windows-msvc` on a
  GitHub-hosted `windows-latest` runner — **4 vCPUs**, not the VM's 16).
- `vosu::plugin_host_native parity::testkit::
  parity_testkit_services_state_resources_and_frames_match_across_runtimes`
  **ABORTed after 0.6 s** with `0xc0000005` (access violation), at position
  283/3119 in the run, ~90 s wall time in, surrounded by many other
  concurrently-running `plugin_host_native` tests (`parent_death`, `broker`,
  `crash`, `lifecycle`, `backpressure`, …) each spawning real child
  `vosu.exe` plugin-host processes, plus the rest of the 3119-test workspace
  running at the same time.
- **`377854be` changes only `tools/live-parity/*.py`** (verified with
  `git show --stat 377854be`): zero Rust/C changes between it and the last
  known-good commits (`3a7d49dd`, `21ce14a4`). The bug, whatever it is, was
  already latent in the Rust/native code before this commit; `377854be` did
  not introduce it, it only happened to be the run that hit it.
- `21ce14a4` is still the tip-ward ancestor on `origin/v2/main` as of this
  investigation (`377854be` has since landed on `v2/main`); no later commit
  on `v2/main` touches `tests/plugin_host_native`, `crates/vosu-plugin-testkit`,
  `crates/vosu-sys`, or `crates/vosu-core/src/plugin` — this is still an open,
  unfixed bug upstream.

## Reproduction attempts (all native, in the Windows VM)

| Condition | Runs | Crashes |
|---|---|---|
| `vm-test.py`, isolated single test, `--nocapture` | 3 | 0 |
| `vm-test.py` loop, isolated single test, repeated builds | 2 (3rd cut by budget) | 0 |
| `vm-test.py` loop, isolated single test, **12 CPU-burning background processes** (leaves ~4/16 cores free, approximating `windows-latest`'s 4 vCPUs) | 2 (3rd cut by budget) | 0 |
| `cargo nextest run` directly (real per-test-process isolation, matching CI's actual harness — `vm-test.py`/`vm-stress.py` both use plain `cargo test`), scoped to `--test plugin_host_native` (76 tests, our target among them) | 1 (76/76 passed) | 0 |
| Same `cargo nextest run`, same module, **+ 12-process CPU stress** | 1 (76/76 passed) | 0 |
| Same `cargo nextest run`, same module, looped (fixtures prepared once, binary unchanged) | 8 clean iterations (killed by my own cleanup on the 9th, not a crash) | 0 |

Total genuine executions of the exact test: **~17 across 5 different
harness/contention combinations**, plus the single-test loop iterations
noted above, all green.

Two tooling-only failures occurred and are **not evidence of the bug**:
- `vm-stress.py` (the "build once, rerun N times" lever) does not run
  `scripts/plugin-host-fixtures.py` before its reruns, so `plugin_host_native`
  always fails there with "`VOSU_PLUGIN_HOST_TESTKIT_LIBRARY is unset`" — a
  harness gap in `vm-stress.py`, not our bug. Left unfixed (out of scope for
  this ticket; flagging here so the next run doesn't waste time on it).
- Running the *entire* `plugin_host_native` module unfiltered via plain
  `cargo test` (all tests as OS threads inside **one process**, the harness's
  default, *not* how nextest or CI runs it) left 4 test threads and 3 child
  `vosu.exe` processes permanently stuck (not crashed — hung) for over 20
  minutes before I killed them. This is a distinct, probably-real issue (some
  fixture likely assumes a resource is process-exclusive, e.g. a named OS
  object, and breaks under same-process multi-threaded reuse), but it is a
  **hang**, not an access violation, and nextest never exercises this
  code path (one OS process per test), so it is almost certainly unrelated
  to the CI crash. Flagged as a separate finding, not chased further.

### Why it likely does not reproduce here

`windows-latest` GitHub-hosted runners have **4 vCPUs**; this VM has **16**.
CI's failing run has the *entire* 3119-test workspace (dozens of
process-spawning test modules: `adapter_golden`, `api_v1_compat`,
`catalog_v2_compat`, `discovery_parity`, `osu_plugin`, `plugin_host`,
`plugin_host_native`, `performance_invariants`, …) competing for those 4
cores via nextest's default `test-threads = num_cpus`. The
demand/concurrency ratio there is roughly 3119/4 ≈ 780; the closest I could
get under the stated constraints (no `prepush.py`, no full-workspace
nextest, no VM start/stop) was 76/16 ≈ 4.75, even with artificial CPU
contention layered on top — about two orders of magnitude less
oversubscribed. A genuine, narrow timing race that depends on a 4-core box
falling badly behind under thousands of waiting test processes (handle
table pressure, scheduler latency on process creation/IPC handshakes,
`vosu.exe`'s own 5 s plugin-host startup deadline, Smart App Control's
first-ever-image-hash evaluation delay noted in
`tests/plugin_host_native/support/host.rs`) is very plausible, and is not
something a 16-vCPU VM running a 76-test slice can exercise, short of
running the full native gate (explicitly out of scope here).

## Code reviewed (SAFETY-contract audit, no smoking gun found)

Everything below was read end-to-end, specifically for the "could a late
reply/timeout/teardown race turn correct code into UB" shape, since the
crash is an AV in the **test's own process**, not a child's separate exit:

- `crates/vosu-core/src/plugin/instance.rs` — `PluginInstance::create_local`,
  `destroy`/`destroy_inner`/`abandon`/`leak_on_timeout`, `Drop`: grace-period
  teardown correctly **leaks** `host_ctx`/`library` rather than freeing them
  when a plugin thread might still be running past the grace period. Looks
  correct.
- `crates/vosu-core/src/plugin/registry.rs`,
  `crates/vosu-core/src/plugin/execution/descriptor.rs` — `load_library`:
  header/`struct_size` validated before the full vtable copy; `Arc<Library>`
  keeps the mapping alive exactly as long as any instance needs it, Drop
  order independent (refcounted).
- `crates/vosu-core/src/plugin/process/ring.rs`,
  `crates/vosu-core/src/plugin/process/frame.rs` — the SPSC ring and frame
  codec: every length/offset read from shared memory (writable by the peer)
  is validated against a locally-trusted bound (`CAPACITY`, `MAX_PAYLOAD_BYTES`)
  before being used as an index or allocation size. Looks correct.
- `crates/vosu-sys/src/plugin_process/region.rs` — `SharedRegion`: all
  access through `AtomicU64`, no non-atomic reference ever formed into
  peer-writable memory; `word_at`'s only `unsafe` is bounds-checked by every
  caller first. Looks correct.
- `crates/vosu-sys/src/plugin_process/windows/{objects,channel,job,spawn}.rs`
  — section/event creation and mapping, handle inheritance gating
  (`process_spawn` global lock vs. a concurrent `CreateProcess` elsewhere),
  job-object confinement, suspended-process unwind-on-failure. Every
  OS-handle-returning call is checked (`owned()`, `NonNull::new(...)
  .ok_or_else(io::Error::last_os_error)`, etc.) before use. Looks correct.
- `crates/vosu-core/src/plugin/process/launcher/supervision.rs` —
  `finish_startup`'s race between a late `READY`/`FAILED` and the startup
  deadline firing is fully serialized behind one `Mutex`; a late message
  after `Phase::Gone` is rejected by the (unseen-but-pattern-matching)
  `ready()`/`failed()` guards the same way `failed()` visibly does.

None of these show a provable bug; they're all defensively written with
explicit `// SAFETY:` justifications that hold up under reading. I did not
find a disprovable claim in any of them.

## Ranked hypotheses for the next investigator

1. **(Most likely) A fixed, short timing margin that only a severely
   CPU-starved host blows through**, in the plugin-host startup/teardown
   path (`crates/vosu-core/src/plugin/process/launcher.rs`'s
   `STARTUP_DEADLINE` = 5 s, `crates/vosu-sys/.../windows/spawn.rs`'s
   suspended-process confine-then-resume sequence, or Windows Defender /
   Smart App Control evaluation of a freshly-built `vosu.exe` noted in
   `support/host.rs`). Not found to be provably racy by reading, but it is
   the one thing in this whole path explicitly measured and commented as
   "can exceed 5 s under load" elsewhere in the codebase
   (`windows.rs`'s `suppress_crash_reporting_hold` doc comment measures
   1.2–5.8 s crash-exit delays under the *same* kind of load). If a
   timeout-driven cleanup path has an edge this investigation didn't
   exercise (because it never got the host anywhere near that margin),
   it would show up only on a genuinely starved 4-core box — exactly what
   we saw.
2. **Windows handle/kernel-object exhaustion under extreme concurrency**:
   `CreateFileMappingW`/`MapViewOfFile`/`CreateEventW`/job-object creation
   all check their return values correctly in the code I read, but I did not
   verify every *caller* of those Results propagates them as a clean
   refusal rather than an `unwrap()`/`expect()` somewhere further up a path
   I didn't trace (e.g. inside `vosu-core`'s process launcher glue above
   `vosu-sys`, which I read partially, not exhaustively).
3. **The single-process-multi-thread hang** found incidentally (see above):
   a fixture likely assumes process-exclusive ownership of some OS object.
   Low likelihood of being the *same* bug (nextest never runs this way), but
   worth a follow-up ticket since it is a real, reproducible (I hit it twice)
   problem in the test harness.
4. **Pure bad luck / hardware-adjacent**: a true one-off (bit flip, host
   scheduler oddity) given the complete absence of any Rust diff between
   passing and failing commits and zero reproduction in ~17 real attempts
   under deliberately adversarial conditions. Least likely to be worth
   chasing further without a captured minidump.

## What I'd do with more budget / fewer constraints

Get a minidump or `RUST_BACKTRACE`-annotated stack from an **actual** CI
failure (register a WER LocalDumps key in a disposable `windows-latest`
runner image, or add `--nocapture` and a crash hook to the native CI job
temporarily) rather than trying to out-guess a ~140x-under-provisioned local
repro. Short of that, the next most promising lever is running the real
`cargo nextest run --workspace …` command (the full gate) on this VM with
CPU affinity capped to 4 cores (`start /affinity` or a Job Object CPU rate
limit) to actually match the oversubscription ratio — explicitly out of
scope for this pass (`No prepush, no VM lifecycle`).

## Cleanup performed

All scratch work happened under `V:\bt\...` staging directories created by
`vm-test.py`/`vm-stress.py`, plus one manually-staged directory
(`V:\bt\vmt-21ce14a47fa0-fc0b939b`) reused to run `cargo nextest run`
directly (to get real per-test-process isolation, which neither
`vm-test.py` nor `vm-stress.py` exposes). All of the following were removed
before finishing:
- 12 CPU-stress PowerShell processes (started/stopped twice during the
  investigation).
- All `vmtest-*`/`vmstress-*`/`vosu-nextest-*` scheduled tasks.
- The manually-staged `V:\bt\vmt-21ce14a47fa0-fc0b939b` and
  `V:\bt\vmt-21ce14a47fa0-7110e02e` directories and their leftover
  `plugin_host_native-*.exe`/`vosu.exe` processes (the latter were
  accidentally left running by an earlier unfiltered-module `cargo test`
  attempt that hung; confirmed killed).
- No WER LocalDumps registry key was ever installed (never got far enough
  to need the debugger step), so there is nothing to remove there.
- The VM itself was never started or stopped by this investigation.

## Acceptance status

- Native tests in the VM: exercised extensively (above); no fix to prove
  before/after since no reproducible defect was isolated.
- `cargo test --locked --features test-harness --test plugin_host` (Linux):
  **not run** — no code changed, nothing to validate.
- `python scripts/plugin-host-fixtures.py` / Linux filters, fmt, clippy,
  xwin clippy, `scripts/check-*.py`: **not run** — no code changed.

No commits other than this handoff were made; the working tree has no
diff against `21ce14a4` besides this file.

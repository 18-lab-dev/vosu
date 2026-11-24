//! F2 cadence/exit-confirmation/stats tests (parity rows 13-17,
//! `m1-remainder-plan.md` §7): split out of `super` to keep that file
//! under the 1,000-line limit. Shares every helper
//! (`SyntheticSource`/`test_host`/`create_session_instance`/`test_process`/
//! `expect_attached`/`expect_detached`) with `super`'s own tests.

use super::*;

/// Drains every event already queued without blocking, panicking with
/// `message` if any is a `Detached` -- tolerates an unrelated event
/// already on the channel (e.g. `SESSION_IDENTIFY`'s own promised first
/// `FOCUS_CHANGED`, `tests::expect_event`'s own doc comment), unlike a
/// bare `try_recv().is_err()` check.
fn assert_no_detach(events: &Receiver<SessionEvent>, message: &str) {
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, SessionEvent::Detached(_)), "{message}");
    }
}

/// Parity row 13 (`src/instance/tests.rs:618`,
/// `failed_full_scans_retries_early`): a failed `enumerate()` is never
/// counted toward [`DiscoveryStats::full_scans`], and the next attempt
/// is scheduled at [`DiscoveryCadence::empty`] regardless of how many
/// processes are already tracked -- not whatever interval the normal
/// tracked-set-size formula would otherwise pick (`cadence_interval`'s
/// own doc comment), which this proves by tracking a process first so
/// the normal formula would instead pick [`DiscoveryCadence::active`].
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_full_scan_retries_early_and_counts_only_success() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.failed-scan",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    source.insert(test_process(500, "osu!.exe"));

    let mut tracked = HashMap::new();
    let mut pending_exits = HashMap::new();
    let mut last_filter: Option<Prefilter> = None;
    let mut last_generation: Option<u64> = None;
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    // First pass: a real success, tracking pid 500 -- so the normal
    // tracked-set formula would pick `cadence.active` (1s) next, not
    // `cadence.empty`.
    let next = run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t0,
        t0,
    );
    assert_eq!(counters.full_scans.load(Ordering::Relaxed), 1);
    assert_eq!(next, t0 + cadence.active);

    // Second pass, due: `enumerate()` fails. `tracked` is left unchanged
    // (pid 500 stays, non-empty) by `full_reconcile`'s own early return,
    // so a buggy implementation using the normal formula would schedule
    // `cadence.active` again; the special-cased retry must use
    // `cadence.empty` instead.
    source.fail_next_enumerate();
    let retry = run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        next,
        next,
    );
    assert_eq!(
        counters.full_scans.load(Ordering::Relaxed),
        1,
        "a failed enumerate must never be counted as a successful full scan"
    );
    assert_eq!(
        retry,
        next + cadence.empty,
        "a failed full scan must retry at the empty cadence, not the active one"
    );

    // Third pass, due again: `enumerate()` succeeds once more.
    let after_retry = run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        retry,
        retry,
    );
    assert_eq!(counters.full_scans.load(Ordering::Relaxed), 2);
    assert_eq!(after_retry, retry + cadence.active);
}

/// Parity row 14 (`src/instance/tests.rs:653`,
/// `failed_targeted_inspection_is_not_counted`): a failed
/// [`ProcessSource::inspect`] forces a reconcile but is never counted
/// toward [`DiscoveryStats::targeted_inspections`]; a later successful
/// inspect for the same pid does count, proving the counter itself
/// works and specifically excludes the failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_targeted_inspection_is_not_counted() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.failed-inspect",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    source.insert(test_process(501, "osu!.exe"));

    let mut tracked = HashMap::new();
    let mut pending_exits = HashMap::new();
    let mut last_filter: Option<Prefilter> = None;
    let mut last_generation: Option<u64> = None;
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    let next = run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t0,
        t0,
    );
    assert_eq!(counters.full_scans.load(Ordering::Relaxed), 1);

    // Well before `next` is due, so this pass takes the targeted-event
    // branch, not another full reconcile.
    let t1 = next - cadence.active / 2;
    source.fail_next_inspect(501);
    source.request_inspect(501);
    let after_failure = run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t1,
        next,
    );
    assert_eq!(
        counters.targeted_inspections.load(Ordering::Relaxed),
        0,
        "a failed inspect must never be counted"
    );
    assert_eq!(
        counters.reconciliation_requests.load(Ordering::Relaxed),
        2,
        "a failed inspect must force exactly one additional reconcile (plus the first pass's own forced sync)"
    );
    assert_eq!(
        counters.full_scans.load(Ordering::Relaxed),
        2,
        "the forced reconcile after a failed inspect must itself still run and succeed"
    );

    // A later successful inspect for the same pid, well before the new
    // `after_failure` deadline, must count.
    let t2 = after_failure - cadence.active / 2;
    source.request_inspect(501);
    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t2,
        after_failure,
    );
    assert_eq!(counters.targeted_inspections.load(Ordering::Relaxed), 1);
}

/// Parity row 15 (`src/instance/tests.rs:703`,
/// `full_reconcile_replaces_a_dead_same_pid_in_one_pass`): already
/// covered functionally by `reused_pid_with_a_new_start_time_is_reported_as_exit_then_a_fresh_appearance`
/// (same pid, a new occurrence, one pass); this proves the same
/// behavior additionally counts as exactly one successful full scan
/// under the new [`DiscoveryCadence`]/[`DiscoveryStats`] plumbing, not
/// two.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_reconcile_replaces_a_dead_same_pid_in_one_pass() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.one-pass-replace",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    let mut first = test_process(502, "osu!.exe");
    first.start_time = 1;
    source.insert(first);

    let mut tracked = HashMap::new();
    let mut pending_exits = HashMap::new();
    let mut last_filter: Option<Prefilter> = None;
    let mut last_generation: Option<u64> = None;
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t0,
        t0,
    );
    let attached = expect_attached(&events, 502);
    assert_eq!(counters.full_scans.load(Ordering::Relaxed), 1);

    // A reused pid with a new occurrence, inserted directly (bypassing
    // `appear`'s own `inspect` hint): only the next full reconcile's own
    // `enumerate()` can see it, exactly one pass reporting the old
    // occurrence's exit immediately followed by the new one's
    // appearance.
    let mut reused = test_process(502, "osu!.exe");
    reused.start_time = 2;
    source.insert(reused);

    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t0 + cadence.active,
        t0 + cadence.active,
    );
    expect_detached(&events, attached.handle);
    let reattached = expect_attached(&events, 502);
    assert_ne!(reattached.handle, attached.handle);
    assert_eq!(
        counters.full_scans.load(Ordering::Relaxed),
        2,
        "the reused-pid exit+appear pair must still count as exactly one full scan"
    );
}

/// Parity row 16 (`src/instance/tests.rs:764`,
/// `live_exit_is_retried_until_the_process_dies_without_another_event`):
/// an `exit` hint for a pid [`ProcessSource::is_alive`] still reports
/// alive is neither trusted outright nor ignored -- it is re-checked
/// with backoff, detaching only once the pid is actually confirmed
/// dead, and specifically *without* another event ever arriving for it
/// (the backoff recheck itself discovers the death).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_exit_is_retried_until_the_process_dies_without_another_event() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.live-exit",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    source.insert(test_process(700, "osu!.exe"));

    let mut tracked = HashMap::new();
    let mut pending_exits = HashMap::new();
    let mut last_filter: Option<Prefilter> = None;
    let mut last_generation: Option<u64> = None;
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    let next = run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t0,
        t0,
    );
    let attached = expect_attached(&events, 700);

    // An exit hint arrives, but the pid is still alive (never removed
    // from `source`'s own ground truth): `handle_exit_hint` must
    // schedule a backoff recheck, not detach immediately.
    let t1 = t0 + Duration::from_millis(1);
    source.report_exit_hint(700);
    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t1,
        next,
    );
    assert_no_detach(
        &events,
        "an exit hint for a still-alive pid must not detach it",
    );

    // The first backoff deadline (10ms) is due, and the pid is still
    // alive, and no new event arrived: the recheck must retry again
    // (longer backoff), still not detaching.
    let t2 = t1 + Duration::from_millis(11);
    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t2,
        next,
    );
    assert_no_detach(
        &events,
        "a still-alive pid must still not be detached at the first backoff retry",
    );

    // Now the pid actually dies, with no further event -- only the next
    // due backoff recheck can discover this.
    source.declare_dead(700);
    let t3 = t2 + Duration::from_millis(21);
    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t3,
        next,
    );
    expect_detached(&events, attached.handle);
}

/// Parity row 17 (`src/instance/tests.rs:827`,
/// `process_event_wakes_are_targeted_and_loss_reconciles_once`): an
/// `inspect` event hint is handled through the targeted path alone --
/// counting toward [`DiscoveryStats::targeted_inspections`] without
/// another [`DiscoveryStats::full_scans`] -- while a single
/// `needs_reconcile` loss flag forces exactly one additional full scan.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn process_event_wakes_are_targeted_and_loss_reconciles_once() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.event-wakes",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    source.insert(test_process(800, "osu!.exe"));

    let mut tracked = HashMap::new();
    let mut pending_exits = HashMap::new();
    let mut last_filter: Option<Prefilter> = None;
    let mut last_generation: Option<u64> = None;
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    let next = run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t0,
        t0,
    );
    assert_eq!(counters.full_scans.load(Ordering::Relaxed), 1);

    // A targeted `inspect` event, well before `next` is due: handled
    // through `apply_batch` alone -- no `enumerate()`, so `full_scans`
    // must not move, only `targeted_inspections`.
    let t1 = next - cadence.active / 2;
    source.request_inspect(800);
    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t1,
        next,
    );
    assert_eq!(counters.targeted_inspections.load(Ordering::Relaxed), 1);
    assert_eq!(
        counters.full_scans.load(Ordering::Relaxed),
        1,
        "a targeted inspect must never force a full scan on its own"
    );

    // A loss flag (no inspect/exit hints at all): must force exactly one
    // additional full reconcile.
    let full_scans_before = counters.full_scans.load(Ordering::Relaxed);
    let reconciliation_requests_before = counters.reconciliation_requests.load(Ordering::Relaxed);
    let t2 = next - cadence.active / 4;
    source.request_reconcile();
    run_pass(
        &host,
        source.as_ref(),
        &mut tracked,
        &mut pending_exits,
        &mut last_filter,
        &mut last_generation,
        &cadence,
        &counters,
        t2,
        next,
    );
    assert_eq!(
        counters.full_scans.load(Ordering::Relaxed),
        full_scans_before + 1,
        "a loss flag must reconcile exactly once"
    );
    assert_eq!(
        counters.reconciliation_requests.load(Ordering::Relaxed),
        reconciliation_requests_before + 1
    );
}

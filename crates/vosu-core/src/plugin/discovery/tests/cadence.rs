//! F2 cadence/exit-confirmation/stats tests (parity rows 2, 13-17,
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

/// Collects `pid_a`'s and `pid_b`'s own `Attached` events regardless of
/// which arrives first (AGENTS.md: "order-agnostic collection when two
/// threads deliver"): `expect_attached` alone would discard the other
/// pid's event while searching, permanently losing it if it happened to
/// arrive first.
fn expect_two_attached(
    events: &Receiver<SessionEvent>,
    pid_a: u32,
    pid_b: u32,
) -> (SessionAttached, SessionAttached) {
    let mut a = None;
    let mut b = None;
    while a.is_none() || b.is_none() {
        match expect_event(
            events,
            |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == pid_a || attached.pid == pid_b),
        ) {
            SessionEvent::Attached(attached) if attached.pid == pid_a => a = Some(attached),
            SessionEvent::Attached(attached) if attached.pid == pid_b => b = Some(attached),
            _ => unreachable!("expect_event's own predicate only matches pid_a or pid_b"),
        }
    }
    (a.unwrap(), b.unwrap())
}

/// Parity row 2 (`src/instance/tests.rs:206`,
/// `targeted_discovery_inserts_only_the_requested_pid`, F2 review P2): an
/// `inspect` event hint for one untracked pid is handled entirely
/// through the targeted path -- exactly one successful
/// [`ProcessSource::inspect`], zero [`ProcessSource::enumerate`] calls,
/// and only that pid attaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn targeted_discovery_inserts_only_the_requested_pid() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.targeted-only",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    // Startup pass: nothing in the source, so `tracked` stays empty.
    let next = run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t0,
        t0,
    );
    let full_scans_before_target = counters.full_scans.load(Ordering::Relaxed);

    // pid 9 is inserted directly into the source's own ground truth
    // (bypassing `appear`'s own inspect hint) and then targeted by its
    // own `inspect` event hint, exactly as a push-event source's single
    // appear notification does.
    source.insert(test_process(9, "osu!.exe"));
    source.request_inspect(9);
    let t1 = next - cadence.empty / 2;
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t1,
        next,
    );

    assert_eq!(
        counters.full_scans.load(Ordering::Relaxed),
        full_scans_before_target,
        "a targeted inspect for one pid must never run a full enumerate()"
    );
    assert_eq!(
        counters.targeted_inspections.load(Ordering::Relaxed),
        1,
        "exactly one inspect(9) must have run"
    );
    let attached = expect_attached(&events, 9);
    assert_eq!(attached.pid, 9, "only pid 9 may have attached");
}

/// Parity F2 review P2 (plan's own `cadence_follows_tracked_set_and_event_health`
/// acceptance test, not previously added): [`DiscoveryCadence::empty`]
/// while nothing is tracked, [`DiscoveryCadence::active`] once something
/// is, and [`DiscoveryCadence::events_healthy`] once
/// [`ProcessSource::events_healthy`] also reports healthy -- all driven
/// through the plain periodic-due path (no forced reconcile), proving
/// [`cadence_interval`]'s own formula end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cadence_follows_tracked_set_and_event_health() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let (source, _prefilter) = SyntheticSource::new();

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    // Nothing tracked, events unhealthy (the trait default): empty
    // cadence.
    let next = run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t0,
        t0,
    );
    assert_eq!(
        next,
        t0 + cadence.empty,
        "nothing tracked, unhealthy -> empty cadence"
    );

    // Tracking one process, still unhealthy: active cadence -- this
    // pass is a plain periodic-due reconcile (`filter_changed`/
    // `generation_changed` are already both `false` by now), so nothing
    // forces the fast cadence regardless of the formula under test.
    source.insert(test_process(900, "osu!.exe"));
    let next2 = run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        next,
        next,
    );
    assert_eq!(
        next2,
        next + cadence.active,
        "tracking one process, unhealthy -> active cadence"
    );

    // Still tracking, now healthy: the healthy-events cadence, even
    // though the tracked set is non-empty (health takes priority over
    // tracked-set size in the formula).
    source.set_events_healthy(true);
    let next3 = run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        next2,
        next2,
    );
    assert_eq!(
        next3,
        next2 + cadence.events_healthy,
        "tracking one process, healthy -> the healthy-events cadence"
    );
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

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    // First pass: a real success, tracking pid 500 -- so the normal
    // tracked-set formula would pick `cadence.active` (1s) next, not
    // `cadence.empty`.
    let next = run_pass(
        &host,
        source.as_ref(),
        &mut state,
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
        &mut state,
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
        &mut state,
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
/// works and specifically excludes the failure. Also proves the forced
/// reconcile's own interval is `degraded` (F2 review P3): a real
/// failure forces the fast cadence even while the source otherwise
/// reports a healthy event channel, unlike a bare loss flag
/// (`inspect_hint_is_targeted_and_a_loss_flag_reconciles_once_keeping_healthy_cadence`).
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
    source.set_events_healthy(true);

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    let next = run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t0,
        t0,
    );
    assert_eq!(counters.full_scans.load(Ordering::Relaxed), 1);

    // Well before `next` is due, so this pass takes the targeted-event
    // branch, not another full reconcile.
    let t1 = next - cadence.events_healthy / 2;
    source.fail_next_inspect(501);
    source.request_inspect(501);
    let after_failure = run_pass(
        &host,
        source.as_ref(),
        &mut state,
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
        1,
        "the startup pass's own first sync is never counted (it was already due); \
         only this failed inspect's own out-of-cadence reconcile is"
    );
    assert_eq!(
        counters.full_scans.load(Ordering::Relaxed),
        2,
        "the forced reconcile after a failed inspect must itself still run and succeed"
    );
    assert_eq!(
        after_failure,
        t1 + cadence.active,
        "a real inspect failure is `degraded`: it must drop to the fast cadence \
         even though the source otherwise reports a healthy event channel"
    );

    // A later successful inspect for the same pid, well before the new
    // `after_failure` deadline, must count.
    let t2 = after_failure - cadence.active / 2;
    source.request_inspect(501);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
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

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    run_pass(
        &host,
        source.as_ref(),
        &mut state,
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
        &mut state,
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
/// (the backoff recheck itself discovers the death). Doubling-sensitive
/// (F2 review P3): `t3` sits strictly between the un-doubled (10ms) and
/// the correctly-doubled (20ms) next deadline, so a backoff that failed
/// to double would already have resolved (and wrongly detached, since
/// the pid is declared dead before `t3`) by the time this checks it.
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

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    let next = run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t0,
        t0,
    );
    let attached = expect_attached(&events, 700);

    // An exit hint arrives, but the pid is still alive (never removed
    // from `source`'s own ground truth): `handle_exit_hint` must
    // schedule a backoff recheck (due at `t1 + 10ms`), not detach
    // immediately.
    let t1 = t0 + Duration::from_millis(1);
    source.report_exit_hint(700);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
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
    // alive, and no new event arrived: the recheck must double the
    // backoff to 20ms (next due at `t2 + 20ms`), still not detaching.
    let t2 = t1 + Duration::from_millis(11);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t2,
        next,
    );
    assert_no_detach(
        &events,
        "a still-alive pid must still not be detached at the first backoff retry",
    );

    // The pid actually dies now, with no further event. `t3` sits 15ms
    // past `t2` -- past an un-doubled 10ms deadline, but short of the
    // correctly-doubled 20ms one -- so this must NOT detach yet; only
    // `resolve_due_pending_exits`'s own backoff-doubling check at `t4`
    // may discover the death.
    source.declare_dead(700);
    let t3 = t2 + Duration::from_millis(15);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t3,
        next,
    );
    assert_no_detach(
        &events,
        "the backoff must have doubled to 20ms; resolving again at only 10ms \
         would wrongly detach the already-dead pid here",
    );

    let t4 = t2 + Duration::from_millis(21);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t4,
        next,
    );
    expect_detached(&events, attached.handle);
}

/// F2 review P3: the exit-confirmation backoff doubles 10, 20, 40, 80,
/// 160ms and then caps at 250ms (never 320, never 500) -- driven through
/// six consecutive still-alive backoff deadlines, then confirming the
/// seventh (post-cap) deadline is still only 250ms later, not the
/// uncapped 500ms a doubling bug would produce.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_confirmation_backoff_is_capped_at_250ms() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.backoff-cap",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    source.insert(test_process(750, "osu!.exe"));

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let far_future = Instant::now() + Duration::from_secs(3600);

    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        Instant::now(),
        far_future,
    );
    let attached = expect_attached(&events, 750);

    let t1 = Instant::now() + Duration::from_millis(1);
    source.report_exit_hint(750);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t1,
        far_future,
    );
    assert_no_detach(&events, "still alive; nothing may detach yet");

    // Due deadlines, still alive each time: 10, 30, 70, 150, 310
    // (cumulative backoff 10, 20, 40, 80, 160) -- after the fifth check
    // (at t1+310) the backoff must cap at 250ms (not double to an
    // uncapped 320ms), so the next due deadline is t1+560.
    let mut now = t1;
    for delta_ms in [10u64, 20, 40, 80, 160] {
        now += Duration::from_millis(delta_ms);
        run_pass(
            &host,
            source.as_ref(),
            &mut state,
            &cadence,
            &counters,
            now,
            far_future,
        );
        assert_no_detach(
            &events,
            "still alive; the backoff recheck must not detach it",
        );
    }

    // The pid dies now, with no further event. Checking exactly at
    // `t1+560` (F2fix review P3's own suggested timing) distinguishes
    // the cap: that deadline is reached only if the backoff capped at
    // 250ms after the fifth check; an uncapped doubling to 320ms would
    // instead schedule the next check at `t1+630`, still pending at
    // `t1+560` and so never detaching here.
    source.declare_dead(750);
    now += Duration::from_millis(250);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        now,
        far_future,
    );
    expect_detached(&events, attached.handle);
}

/// F2 review P3: [`MAX_PENDING_EXIT_CONFIRMATIONS`] (128) bounds the
/// pending set. Overflowing it evicts the entry with the *latest* due
/// deadline -- the most recently added one, 1.x's own `max_by_key`
/// choice (`src/instance/runtime.rs:158-165`) -- and forces a reconcile
/// that must not disturb any other still-pending entry (F2 review
/// finding 1's own fix: `apply_appeared` no longer clears a
/// same-occurrence pending entry).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_exit_overflow_evicts_the_latest_due_entry_and_keeps_the_rest() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    // Only pid 1 and pid 128 match this plugin's own `[match]` table and
    // so are observable through real session events; the other 126
    // fillers are still fully tracked/pending bookkeeping inside
    // `DiscoveryState` -- `apply_appeared`/`apply_exited` never consult
    // whether any plugin matched.
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.overflow",
        &["target.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let far_future = Instant::now() + Duration::from_secs(3600);
    let t0 = Instant::now();

    for pid in 1..=128u32 {
        let name = if pid == 1 || pid == 128 {
            "target.exe"
        } else {
            "filler.exe"
        };
        source.insert(test_process(pid, name));
    }
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t0,
        t0,
    );
    let (first, last) = expect_two_attached(&events, 1, 128);

    // Exit-hint pids 1..128 one at a time, each its own pass a
    // microsecond later than the last -- far short of the 10ms initial
    // backoff, so every pending entry gets its own strictly increasing
    // due deadline and none resolves early.
    let report_base = t0 + Duration::from_millis(1);
    let mut now = report_base;
    let mut pid1_reported_at = None;
    let mut pid128_reported_at = None;
    for pid in 1..=128u32 {
        source.report_exit_hint(pid);
        run_pass(
            &host,
            source.as_ref(),
            &mut state,
            &cadence,
            &counters,
            now,
            far_future,
        );
        if pid == 1 {
            pid1_reported_at = Some(now);
        }
        if pid == 128 {
            pid128_reported_at = Some(now);
        }
        now += Duration::from_micros(1);
    }
    let pid1_due = pid1_reported_at.unwrap() + Duration::from_millis(10);
    let pid128_due = pid128_reported_at.unwrap() + Duration::from_millis(10);
    assert_no_detach(&events, "every pid is still alive; none may detach yet");

    // One more process, tracked through its own `appear` (inspect path),
    // then exit-hinted: its arrival overflows the 128 bound, evicting
    // pid 128's own entry -- the one with the latest due deadline of the
    // 128 just created -- and forcing a reconcile.
    source.appear(test_process(129, "filler.exe"));
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        now,
        far_future,
    );
    now += Duration::from_micros(1);
    let full_scans_before_overflow = counters.full_scans.load(Ordering::Relaxed);
    source.report_exit_hint(129);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        now,
        far_future,
    );

    // F2fix review P3: assert directly on `state.pending_exits` instead
    // of only inferring eviction from later detach timing (the prior
    // round's assertions could not fail for the bug they targeted).
    assert!(
        !state.pending_exits.contains_key(&128),
        "pid 128's own entry -- the latest due of the 128 just created -- must have been evicted"
    );
    assert!(
        state.pending_exits.contains_key(&1),
        "pid 1's own entry must have survived the eviction-forced reconcile"
    );
    assert!(
        state.pending_exits.contains_key(&129),
        "pid 129's own entry, created by this same pass, must be present"
    );
    assert_eq!(
        state.pending_exits.len(),
        128,
        "the 128 bound must hold exactly: pid 128 evicted, pid 129 admitted"
    );
    assert!(
        counters.full_scans.load(Ordering::Relaxed) > full_scans_before_overflow,
        "the eviction must have forced a full reconcile"
    );

    // pid 1's own pending confirmation survived the eviction-forced
    // reconcile (F2 review finding 1): declaring it dead and reaching
    // its original backoff deadline (which arrives before pid 128's
    // own, since pid 1 was exit-hinted first) must still detach it.
    // Checked before pid 128's own deadline below: once that later
    // `now` is reached, pid 1's own still-pending entry would otherwise
    // also be swept by the same `resolve_due_pending_exits` pass,
    // extending its backoff past this test's own precomputed deadline.
    source.declare_dead(1);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        pid1_due + Duration::from_millis(1),
        far_future,
    );
    expect_detached(&events, first.handle);

    // pid 128's own pending confirmation was evicted: declaring it dead
    // and reaching its original backoff deadline must NOT detach it --
    // nothing is left pending to catch it.
    source.declare_dead(128);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        pid128_due + Duration::from_millis(1),
        far_future,
    );
    assert_no_detach(
        &events,
        "pid 128's exit confirmation was evicted; its own backoff must not fire",
    );
    // `last` (pid 128's own attach) is otherwise unused: the eviction
    // path is proven entirely by `assert_no_detach` above, not a second
    // `expect_detached` (nothing should ever detach it through this
    // path).
    let _ = last;
}

/// Parity row 17 (`src/instance/tests.rs:827`), renamed from
/// `process_event_wakes_are_targeted_and_loss_reconciles_once` (F2
/// review P3: the old name claimed to cover `event_wakes`, which only
/// [`DiscoveryService`]'s own background thread increments --
/// `event_wakes_counts_only_a_genuine_source_notifier_wake` covers that
/// instead through the real loop). An `inspect` event hint is handled
/// through the targeted path alone -- counting toward
/// [`DiscoveryStats::targeted_inspections`] without another
/// [`DiscoveryStats::full_scans`] -- while a single `needs_reconcile`
/// loss flag forces exactly one additional full scan *keeping* the
/// healthy-events cadence (F2 review P3): unlike a real failure
/// (`failed_targeted_inspection_is_not_counted`), a bare loss flag is
/// never `degraded`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspect_hint_is_targeted_and_a_loss_flag_reconciles_once_keeping_healthy_cadence() {
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
    source.set_events_healthy(true);

    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let t0 = Instant::now();

    let next = run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        t0,
        t0,
    );
    assert_eq!(counters.full_scans.load(Ordering::Relaxed), 1);
    assert_eq!(
        next,
        t0 + cadence.events_healthy,
        "tracked and healthy from the start -> the healthy-events cadence"
    );

    // A targeted `inspect` event, well before `next` is due: handled
    // through `apply_batch` alone -- no `enumerate()`, so `full_scans`
    // must not move, only `targeted_inspections`.
    let t1 = next - cadence.events_healthy / 2;
    source.request_inspect(800);
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
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
    // additional full reconcile, but -- unlike a real failure or an
    // eviction -- must keep the healthy-events cadence, not drop to the
    // fast one (F2 review P3: 1.x's own `lost` sets `full_reconcile` but
    // never `retry_reconcile`).
    let full_scans_before = counters.full_scans.load(Ordering::Relaxed);
    let reconciliation_requests_before = counters.reconciliation_requests.load(Ordering::Relaxed);
    let t2 = next - cadence.events_healthy / 4;
    source.request_reconcile();
    let after_loss = run_pass(
        &host,
        source.as_ref(),
        &mut state,
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
    assert_eq!(
        after_loss,
        t2 + cadence.events_healthy,
        "a bare loss flag must keep the healthy-events cadence, not drop to the fast one"
    );
}

/// F2 review P3: `event_wakes` is incremented only by
/// [`DiscoveryService`]'s own background thread, never by [`run_pass`]
/// directly -- driven through the real [`DiscoveryService::start`] loop
/// (unlike every other test in this module), so a genuine source
/// notifier wake (`SyntheticSource::appear`) is actually counted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_wakes_counts_only_a_genuine_source_notifier_wake() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.event-wakes-real",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    // Bounded poll (not a sleep-for-ordering) for the startup full
    // reconcile to finish: `DiscoveryService::start`'s first pass is
    // always a full reconcile regardless of any notifier, so an
    // `appear` racing ahead of it would be picked up by that pass
    // instead of the notifier-driven targeted path this test means to
    // prove -- `full_scans >= 1` only once that first pass has
    // returned.
    let deadline = Instant::now() + RECV_TIMEOUT;
    while service.stats().full_scans == 0 {
        assert!(
            Instant::now() < deadline,
            "the startup full reconcile never completed"
        );
        std::thread::yield_now();
    }

    source.appear(test_process(850, "osu!.exe"));
    expect_attached(&events, 850);

    assert!(
        service.stats().event_wakes >= 1,
        "a real source notifier wake must be counted"
    );

    service.stop();
}

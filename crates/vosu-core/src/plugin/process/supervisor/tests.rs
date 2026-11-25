//! The supervisor state machine with injected monotonic time: one crash
//! event per generation, stale timers and starts, stop, shutdown, manual
//! restart, and the crash-class split of startup failures. The host side
//! (`host::tests::supervision`) drives the same machine through real
//! instances.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    FailureReason, Next, ProcessPhase, Published, RestartRefused, StartGuard, Supervisor,
    Template,
};
use crate::identity::PluginInstanceId;
use crate::plugin::execution::LossCause;
use crate::plugin::process::control::{StartupFailure, StartupStage};
use crate::plugin::registry;
use crate::plugin::test_support::write_development_package;

const ID: PluginInstanceId = PluginInstanceId(7_001);

fn template() -> (tempfile::TempDir, Template) {
    let dir = tempfile::tempdir().unwrap();
    write_development_package(
        dir.path(),
        "test.c0.supervised",
        "c0-supervised",
        "process",
        "plugin.bin",
        b"never mapped by core",
    )
    .unwrap();
    let plugin = registry::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        dir.path(),
        true,
    )
    .unwrap();
    let template = Template {
        plugin,
        data_dir: dir.path().to_owned(),
        pool: None,
    };
    (dir, template)
}

/// A supervisor whose instance [`ID`] is `Running` after its initial load.
fn running() -> (tempfile::TempDir, Arc<Supervisor>) {
    let (dir, template) = template();
    let supervisor = Arc::new(Supervisor::default());
    let guard = supervisor.register(ID, template).unwrap();
    assert_eq!(supervisor.published(ID, guard.epoch), Published::Keep);
    drop(guard);
    (dir, supervisor)
}

fn phase(supervisor: &Supervisor) -> ProcessPhase {
    supervisor.status(ID).unwrap().phase
}

fn restart(next: Next) -> (u64, Duration) {
    match next {
        Next::Restart { epoch, delay } => (epoch, delay),
        Next::Nothing => panic!("expected a restart"),
    }
}

/// Starts the generation the delay under `epoch` was waiting for.
fn begin(supervisor: &Arc<Supervisor>, epoch: u64) -> StartGuard {
    supervisor.begin_restart(ID, epoch).expect("a current delay").1
}

fn crash(stage: StartupStage) -> FailureReason {
    FailureReason::Startup(StartupFailure::new(stage, "test", "test"))
}

#[test]
fn simultaneous_loss_signals_record_one_crash_event() {
    let (_dir, supervisor) = running();
    let now = Instant::now();
    // The exit watcher and the channel reader both observed the end of the
    // same generation: only the first reaches a `Running` instance.
    let (_, delay) = restart(supervisor.lost(ID, LossCause::Stalled, now));
    assert_eq!(delay, Duration::from_secs(1));
    assert_eq!(
        supervisor.lost(ID, LossCause::ProcessExited { status: Some(-9) }, now),
        Next::Nothing
    );
    let status = supervisor.status(ID).unwrap();
    assert_eq!(status.crash_events, 1);
    assert_eq!(
        status.last_failure,
        Some(FailureReason::Loss(LossCause::Stalled))
    );
}

#[test]
fn an_unexpected_clean_exit_is_a_crash_event_and_restarts() {
    let (_dir, supervisor) = running();
    let (epoch, delay) = restart(supervisor.lost(
        ID,
        LossCause::ProcessExited { status: Some(0) },
        Instant::now(),
    ));
    assert_eq!(delay, Duration::from_secs(1));
    assert!(matches!(phase(&supervisor), ProcessPhase::Backoff { .. }));
    let guard = begin(&supervisor, epoch);
    assert_eq!(phase(&supervisor), ProcessPhase::Starting);
    assert_eq!(supervisor.published(ID, guard.epoch), Published::Keep);
    let status = supervisor.status(ID).unwrap();
    assert_eq!((status.phase, status.restarts), (ProcessPhase::Running, 1));
}

#[test]
fn the_fifth_crash_inside_the_window_latches_failed_instead_of_retrying() {
    let (_dir, supervisor) = running();
    let start = Instant::now();
    let mut delays = Vec::new();
    for event in 0..4 {
        let (epoch, delay) = restart(supervisor.lost(
            ID,
            LossCause::ChannelFailed,
            start + Duration::from_secs(event),
        ));
        delays.push(delay);
        let guard = begin(&supervisor, epoch);
        assert_eq!(supervisor.published(ID, guard.epoch), Published::Keep);
    }
    assert_eq!(delays, [1, 2, 4, 8].map(Duration::from_secs));
    assert_eq!(
        supervisor.lost(ID, LossCause::ChannelFailed, start + Duration::from_secs(5)),
        Next::Nothing
    );
    let status = supervisor.status(ID).unwrap();
    assert_eq!(status.phase, ProcessPhase::Failed);
    assert_eq!(status.crash_events, 5);
    assert_eq!(status.restarts, 4);
}

#[test]
fn a_stop_during_the_backoff_delay_never_respawns() {
    let (_dir, supervisor) = running();
    let (epoch, _) = restart(supervisor.lost(ID, LossCause::ChannelFailed, Instant::now()));
    assert!(supervisor.stop(ID));
    assert!(supervisor.begin_restart(ID, epoch).is_none());
    assert_eq!(phase(&supervisor), ProcessPhase::Stopped);
    // A late loss report of the stopped generation records nothing.
    assert_eq!(
        supervisor.lost(ID, LossCause::ChannelFailed, Instant::now()),
        Next::Nothing
    );
    assert_eq!(supervisor.status(ID).unwrap().crash_events, 1);
}

#[test]
fn a_stop_during_a_start_tears_the_new_generation_down() {
    let (_dir, supervisor) = running();
    let (epoch, _) = restart(supervisor.lost(ID, LossCause::ChannelFailed, Instant::now()));
    let guard = begin(&supervisor, epoch);
    assert!(supervisor.stop(ID));
    assert_eq!(supervisor.published(ID, guard.epoch), Published::TearDown);
    assert_eq!(phase(&supervisor), ProcessPhase::Stopped);
}

#[test]
fn shutdown_cancels_every_delay_and_refuses_every_start() {
    let (_dir, supervisor) = running();
    let (epoch, _) = restart(supervisor.lost(ID, LossCause::ChannelFailed, Instant::now()));
    supervisor.shutdown();
    assert!(supervisor.begin_restart(ID, epoch).is_none());
    assert_eq!(phase(&supervisor), ProcessPhase::Stopped);
    assert!(matches!(
        supervisor.manual_restart(ID),
        Err(RestartRefused::ShuttingDown)
    ));
    let (_other, template) = template();
    assert!(matches!(
        supervisor.register(PluginInstanceId(7_002), template),
        Err(RestartRefused::ShuttingDown)
    ));
}

#[test]
fn a_manual_restart_resets_the_window_and_the_latch() {
    let (_dir, supervisor) = running();
    let start = Instant::now();
    for event in 0..4 {
        let (epoch, _) = restart(supervisor.lost(
            ID,
            LossCause::ChannelFailed,
            start + Duration::from_secs(event),
        ));
        let guard = begin(&supervisor, epoch);
        supervisor.published(ID, guard.epoch);
    }
    supervisor.lost(ID, LossCause::ChannelFailed, start + Duration::from_secs(4));
    assert_eq!(phase(&supervisor), ProcessPhase::Failed);

    let (_, guard) = supervisor.manual_restart(ID).unwrap();
    assert_eq!(supervisor.status(ID).unwrap().crash_events, 0);
    assert!(matches!(
        supervisor.manual_restart(ID),
        Err(RestartRefused::Busy)
    ));
    assert_eq!(supervisor.published(ID, guard.epoch), Published::Keep);
    let (_, delay) = restart(supervisor.lost(
        ID,
        LossCause::ChannelFailed,
        start + Duration::from_secs(5),
    ));
    assert_eq!(delay, Duration::from_secs(1), "the backoff starts over");
}

#[test]
fn startup_failures_split_by_their_structured_stage() {
    // During an automatic restart a crash-class stage is one crash event.
    for stage in [StartupStage::Create, StartupStage::Timeout, StartupStage::Crash] {
        let (_dir, supervisor) = running();
        let (epoch, _) =
            restart(supervisor.lost(ID, LossCause::ChannelFailed, Instant::now()));
        let guard = begin(&supervisor, epoch);
        let (_, delay) = restart(supervisor.start_failed(
            ID,
            guard.epoch,
            crash(stage),
            true,
            Instant::now(),
        ));
        assert_eq!(delay, Duration::from_secs(2), "{stage:?}");
        assert_eq!(supervisor.status(ID).unwrap().crash_events, 2, "{stage:?}");
    }
    // A deterministic refusal is never retried and never a crash.
    for stage in [
        StartupStage::Spawn,
        StartupStage::Job,
        StartupStage::Generation,
        StartupStage::Security,
        StartupStage::Approval,
        StartupStage::Library,
        StartupStage::Abi,
    ] {
        let (_dir, supervisor) = running();
        let (epoch, _) =
            restart(supervisor.lost(ID, LossCause::ChannelFailed, Instant::now()));
        let guard = begin(&supervisor, epoch);
        assert_eq!(
            supervisor.start_failed(ID, guard.epoch, crash(stage), true, Instant::now()),
            Next::Nothing,
            "{stage:?}"
        );
        let status = supervisor.status(ID).unwrap();
        assert_eq!(status.phase, ProcessPhase::Failed, "{stage:?}");
        assert_eq!(status.crash_events, 1, "{stage:?}");
    }
    // The initial load waits for a manual restart whatever the stage.
    let (_dir, template) = template();
    let supervisor = Arc::new(Supervisor::default());
    let guard = supervisor.register(ID, template).unwrap();
    assert_eq!(
        supervisor.start_failed(
            ID,
            guard.epoch,
            crash(StartupStage::Crash),
            false,
            Instant::now()
        ),
        Next::Nothing
    );
    let status = supervisor.status(ID).unwrap();
    assert_eq!((status.phase, status.crash_events), (ProcessPhase::Failed, 0));
}

#[test]
fn an_active_instance_cannot_be_registered_twice() {
    let (_dir, supervisor) = running();
    let (_other, template) = template();
    assert!(matches!(
        supervisor.register(ID, template.clone()),
        Err(RestartRefused::Busy)
    ));
    assert!(supervisor.stop(ID));
    assert!(supervisor.register(ID, template).is_ok());
}

#[test]
fn shutdown_waits_for_starts_in_progress() {
    let (_dir, supervisor) = running();
    let (epoch, _) = restart(supervisor.lost(ID, LossCause::ChannelFailed, Instant::now()));
    let guard = begin(&supervisor, epoch);
    supervisor.shutdown();
    assert!(!supervisor.wait_idle(Duration::from_millis(50)));
    let finisher = std::thread::spawn(move || {
        assert_eq!(supervisor.published(ID, guard.epoch), Published::TearDown);
        drop(guard);
        supervisor
    });
    let supervisor = finisher.join().unwrap();
    assert!(supervisor.wait_idle(Duration::from_secs(5)));
}

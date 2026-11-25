//! Process supervision through a real `PluginHost` and a recording
//! launcher (`docs/v2/plugin-abi.md` §7.6 "Loss identity", "Restart",
//! "Destroy and shutdown"): a lost generation is retired before a new one
//! starts after its backoff, a stop or shutdown during the delay never
//! respawns, and the per-instance teardown seam destroys exactly one
//! instance. Native evidence with real children is `plugin_host_native`'s
//! `restart` and `crash` modules.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vosu_plugin_api::{Message, TimerFired, vosu_opcode};

use super::helpers::{RECV_TIMEOUT, test_host, test_process};
use crate::plugin::execution::LossCause;
use crate::plugin::host::*;
use crate::plugin::process::control::StartupStage;
use crate::plugin::process::supervisor::{FailureReason, ProcessPhase};
use crate::plugin::registry::{self, LoadedPlugin};
use crate::plugin::test_support::{LaunchBehavior, RecordingLauncher, write_development_package};

/// Longer than the first 1-second backoff, with margin.
const PAST_FIRST_BACKOFF: Duration = Duration::from_millis(1_600);

fn process_package(plugin_id: &str, process_name: &str) -> (tempfile::TempDir, LoadedPlugin) {
    let dir = tempfile::tempdir().unwrap();
    write_development_package(
        dir.path(),
        plugin_id,
        process_name,
        "process",
        "plugin.bin",
        b"never mapped by core",
    )
    .unwrap();
    let loaded = registry::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        dir.path(),
        true,
    )
    .unwrap();
    (dir, loaded)
}

fn wait_phase(host: &PluginHost, id: PluginInstanceId, accept: impl Fn(&ProcessPhase) -> bool) {
    let deadline = Instant::now() + RECV_TIMEOUT;
    loop {
        let phase = host.process_status(id).unwrap().phase;
        if accept(&phase) {
            return;
        }
        assert!(Instant::now() < deadline, "stuck in {phase:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fired() -> Message {
    Message::TimerFired(TimerFired { timer: 1 })
}

struct Scenario {
    runtime: tokio::runtime::Runtime,
    _logs: tempfile::TempDir,
    _package: tempfile::TempDir,
    host: Arc<PluginHost>,
    launcher: Arc<RecordingLauncher>,
    id: PluginInstanceId,
}

impl Scenario {
    fn running(plugin_id: &str, process_name: &str, id: u64) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let (logs, host) = test_host(runtime.handle().clone());
        let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
        host.install_process_launcher(launcher.clone()).unwrap();
        let (package, loaded) = process_package(plugin_id, process_name);
        let id = PluginInstanceId::new(id);
        host.create_instance(id, &loaded, std::env::temp_dir(), None)
            .unwrap();
        drop(_enter);
        assert_eq!(
            host.process_status(id).unwrap().phase,
            ProcessPhase::Running
        );
        Self {
            runtime,
            _logs: logs,
            _package: package,
            host,
            launcher,
            id,
        }
    }

    fn lose(&self, generation: usize) {
        let loss = self.launcher.launches()[generation].loss.clone();
        assert!(loss.report(LossCause::ProcessExited { status: Some(0) }));
    }
}

#[test]
fn supervision_a_lost_instance_restarts_after_backoff_as_a_new_generation() {
    let scenario = Scenario::running("test.c0.restart", "c0-restart", 9_801);
    let (host, id) = (&scenario.host, scenario.id);
    {
        let _enter = scenario.runtime.enter();
        host.process_appeared(test_process(9_890, "c0-restart"));
    }
    let old = Arc::clone(&scenario.launcher.launches()[0].endpoint);
    assert!(
        old.wait_for_frame(vosu_opcode::VOSU_OP_SESSION_ATTACHED as u32, RECV_TIMEOUT)
            .is_some()
    );

    let lost_at = Instant::now();
    scenario.lose(0);
    wait_phase(host, id, |phase| {
        matches!(phase, ProcessPhase::Backoff { delay, streak: 1 } if *delay == Duration::from_secs(1))
    });
    // The lost generation was retired, its child reaped, before the delay.
    assert_eq!(old.terminate_calls(), 1);
    assert!(
        host.sessions
            .lock()
            .unwrap()
            .handle_for_pid(9_890)
            .is_none()
    );

    assert_eq!(scenario.launcher.wait_launches(2, RECV_TIMEOUT), 2);
    let restarted_after = lost_at.elapsed();
    assert!(
        restarted_after >= Duration::from_millis(950),
        "{restarted_after:?}"
    );
    wait_phase(host, id, |phase| *phase == ProcessPhase::Running);
    let status = host.process_status(id).unwrap();
    assert_eq!((status.restarts, status.crash_events), (1, 1));
    assert_eq!(
        status.last_failure,
        Some(FailureReason::Loss(LossCause::ProcessExited {
            status: Some(0)
        }))
    );
    let new = Arc::clone(&scenario.launcher.launches()[1].endpoint);
    assert!(!Arc::ptr_eq(&old, &new), "a restart reused the endpoint");

    // The old generation's loss reporter can never touch the new one.
    assert!(
        !scenario.launcher.launches()[0]
            .loss
            .report(LossCause::ChannelFailed)
    );
    host.dispatch_to_instance(id, &fired(), 0)
        .unwrap()
        .recv_timeout(RECV_TIMEOUT)
        .unwrap();
    assert!(old.frames().iter().all(|frame| frame.opcode
        != vosu_opcode::VOSU_OP_TIMER_FIRED as u32));
    assert!(
        new.wait_for_frame(vosu_opcode::VOSU_OP_TIMER_FIRED as u32, RECV_TIMEOUT)
            .is_some()
    );
    // The new generation registered its matcher again and is offered the
    // still-running game afresh.
    {
        let _enter = scenario.runtime.enter();
        host.process_appeared(test_process(9_890, "c0-restart"));
    }
    assert!(
        new.wait_for_frame(vosu_opcode::VOSU_OP_SESSION_ATTACHED as u32, RECV_TIMEOUT)
            .is_some()
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn supervision_a_teardown_during_the_backoff_never_respawns() {
    let scenario = Scenario::running("test.c0.stopped", "c0-stopped", 9_802);
    let (host, id) = (&scenario.host, scenario.id);
    scenario.lose(0);
    wait_phase(host, id, |phase| matches!(phase, ProcessPhase::Backoff { .. }));
    host.teardown_instance(id).unwrap();
    assert_eq!(host.process_status(id).unwrap().phase, ProcessPhase::Stopped);
    std::thread::sleep(PAST_FIRST_BACKOFF);
    assert_eq!(scenario.launcher.launches().len(), 1, "a stopped instance respawned");
    assert_eq!(host.process_status(id).unwrap().phase, ProcessPhase::Stopped);
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn supervision_shutdown_during_the_backoff_never_respawns() {
    let scenario = Scenario::running("test.c0.shutdown", "c0-shutdown", 9_803);
    let (host, id) = (&scenario.host, scenario.id);
    scenario.lose(0);
    wait_phase(host, id, |phase| matches!(phase, ProcessPhase::Backoff { .. }));
    host.destroy_all(Duration::from_secs(5));
    std::thread::sleep(PAST_FIRST_BACKOFF);
    assert_eq!(scenario.launcher.launches().len(), 1, "respawned after shutdown");
    assert_eq!(host.process_status(id).unwrap().phase, ProcessPhase::Stopped);
}

#[test]
fn supervision_teardown_destroys_one_running_process_instance() {
    let scenario = Scenario::running("test.c0.teardown", "c0-teardown", 9_804);
    let (host, id) = (&scenario.host, scenario.id);
    let bystander = PluginInstanceId::new(9_805);
    let (_dir, other) = process_package("test.c0.bystander", "c0-bystander");
    {
        let _enter = scenario.runtime.enter();
        host.create_instance(bystander, &other, std::env::temp_dir(), None)
            .unwrap();
        host.process_appeared(test_process(9_891, "c0-teardown"));
    }
    let endpoint = Arc::clone(&scenario.launcher.launches()[0].endpoint);
    assert!(
        endpoint
            .wait_for_frame(vosu_opcode::VOSU_OP_SESSION_ATTACHED as u32, RECV_TIMEOUT)
            .is_some()
    );

    host.teardown_instance(id).unwrap();
    // Its session detached while it was still published, then `destroy`.
    assert!(
        endpoint
            .wait_for_frame(vosu_opcode::VOSU_OP_SESSION_DETACHED as u32, RECV_TIMEOUT)
            .is_some()
    );
    assert_eq!((endpoint.destroy_calls(), endpoint.terminate_calls()), (1, 0));
    assert!(matches!(
        host.dispatch_to_instance(id, &fired(), 0),
        Err(crate::plugin::HostError::UnknownInstance)
    ));
    assert!(host.action_state.lock().unwrap().descriptors.keys().all(|key| !key.starts_with("test.c0.teardown.")));
    assert_eq!(host.process_status(id).unwrap().phase, ProcessPhase::Stopped);
    // The bystander is untouched.
    host.dispatch_to_instance(bystander, &fired(), 0)
        .unwrap()
        .recv_timeout(RECV_TIMEOUT)
        .unwrap();
    std::thread::sleep(PAST_FIRST_BACKOFF);
    assert_eq!(scenario.launcher.launches().len(), 2, "a torn-down instance restarted");
    assert!(matches!(
        host.teardown_instance(PluginInstanceId::new(9_899)),
        Err(crate::plugin::HostError::UnknownInstance)
    ));
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn supervision_a_failed_initial_load_waits_for_a_manual_restart() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::FailStartup(StartupStage::Crash));
    host.install_process_launcher(launcher.clone()).unwrap();
    let (_dir, loaded) = process_package("test.c0.manual", "c0-manual");
    let id = PluginInstanceId::new(9_806);
    let error = host
        .create_instance(id, &loaded, std::env::temp_dir(), None)
        .unwrap_err();
    let crate::plugin::HostError::ProcessStartupFailed(failure) = error else {
        panic!("{error:?}");
    };
    assert_eq!(failure.stage, StartupStage::Crash);
    let status = host.process_status(id).unwrap();
    assert_eq!((status.phase, status.crash_events), (ProcessPhase::Failed, 0));
    std::thread::sleep(PAST_FIRST_BACKOFF);
    assert_eq!(launcher.launches().len(), 1, "an initial load was retried");

    launcher.set_behavior(LaunchBehavior::Ready);
    host.restart_process_instance(id).unwrap();
    let status = host.process_status(id).unwrap();
    assert_eq!((status.phase, status.restarts), (ProcessPhase::Running, 1));
    assert_eq!(launcher.launches().len(), 2);
    host.destroy_all(Duration::from_secs(5));
}

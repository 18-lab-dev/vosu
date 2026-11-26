//! `PluginHost` over the process execution seam (`docs/v2/plugin-abi.md`
//! §7.2) with a recording launcher: a process package reaches only its own
//! endpoint, readiness failure and loss release exactly the state a
//! published instance holds, and bundled packages never touch the launcher.

use std::sync::Arc;
use std::time::Duration;

use vosu_plugin_api::{
    Init, Message, TimerFired, TimerSet, VOSU_FRAME_MAX_BYTES, decode, vosu_opcode, vosu_status,
};

use super::helpers::{RECV_TIMEOUT, test_host, test_process};
use crate::plugin::execution::{LossCause, MessageRef};
use crate::plugin::host::*;
use crate::plugin::registry::{self, LoadedPlugin};
use crate::plugin::test_support::{LaunchBehavior, RecordingLauncher, write_development_package};

mod startup_watch;

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

/// Every piece of state a published instance of `plugin_id` registers.
fn registered(host: &PluginHost, plugin_id: &str) -> (bool, bool, bool) {
    let namespace = host
        .state()
        .registry
        .read()
        .unwrap()
        .get(plugin_id)
        .is_some();
    let matcher = !host.matcher.lock().unwrap().is_empty();
    let action = host
        .action_state
        .lock()
        .unwrap()
        .descriptors
        .contains_key(&format!("{plugin_id}.run"));
    (namespace, matcher, action)
}

fn wait_until_removed(host: &PluginHost, id: PluginInstanceId) {
    let instances = host.instances.lock().unwrap();
    let (instances, timeout) = host
        .instances_changed
        .wait_timeout_while(instances, RECV_TIMEOUT, |map| map.contains_key(&id))
        .unwrap();
    assert!(
        !timeout.timed_out(),
        "lost instance {id:?} was never retired"
    );
    drop(instances);
}

#[test]
fn process_instances_dispatch_only_through_their_own_endpoints() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    assert!(matches!(
        host.install_process_launcher(launcher.clone()),
        Err(crate::plugin::HostError::State(_))
    ));
    let (_dir_a, a) = process_package("test.h0.a", "h0-a");
    let (_dir_b, b) = process_package("test.h0.b", "h0-b");
    let (id_a, id_b) = (PluginInstanceId::new(9_101), PluginInstanceId::new(9_102));
    host.create_instance(id_a, &a, std::env::temp_dir(), None)
        .unwrap();
    host.create_instance(id_b, &b, std::env::temp_dir(), None)
        .unwrap();

    let (endpoint_a, endpoint_b, ingress_a) = {
        let launches = launcher.launches();
        assert_eq!(launches.len(), 2, "one child per instance");
        let [first, second] = &launches[..] else {
            unreachable!()
        };
        assert_eq!((first.instance, second.instance), (id_a, id_b));
        assert_eq!(first.plugin_id, "test.h0.a");
        let Init { plugin_id, .. } = decode(&first.init).unwrap();
        assert_eq!(plugin_id, "test.h0.a");
        let Init { plugin_id, .. } = decode(&second.init).unwrap();
        assert_eq!(plugin_id, "test.h0.b");
        assert_ne!(first.library, second.library);
        assert!(
            !Arc::ptr_eq(&first.endpoint, &second.endpoint),
            "instances must never share an endpoint"
        );
        (
            Arc::clone(&first.endpoint),
            Arc::clone(&second.endpoint),
            first.ingress.clone(),
        )
    };

    // Host → plugin: queued dispatch reaches exactly A's endpoint with the
    // encoded bytes unchanged, and reports the endpoint's status.
    let fired = Message::TimerFired(TimerFired { timer: 77 });
    let status = host
        .dispatch_to_instance(id_a, &fired, 41)
        .unwrap()
        .recv_timeout(RECV_TIMEOUT)
        .unwrap();
    assert_eq!(status, vosu_status::VOSU_OK);
    let frame = endpoint_a
        .wait_for_frame(vosu_opcode::VOSU_OP_TIMER_FIRED as u32, RECV_TIMEOUT)
        .unwrap();
    assert_eq!(frame.correlation_id, 41);
    assert_eq!(frame.payload, fired.encode_payload().unwrap());
    assert!(endpoint_b.frames().is_empty(), "B received A's delivery");

    // Plugin → host: A's ingress applies the in-process `send` policy, and
    // a timer it sets fires back through A's endpoint only.
    let oversized = vec![0_u8; VOSU_FRAME_MAX_BYTES as usize + 1];
    assert_eq!(
        ingress_a.deliver(MessageRef {
            opcode: vosu_opcode::VOSU_OP_TIMER_SET as u32,
            flags: 0,
            correlation_id: 0,
            payload: &oversized,
        }),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        ingress_a.deliver(MessageRef {
            opcode: u32::MAX,
            flags: 0,
            correlation_id: 0,
            payload: &[],
        }),
        vosu_status::VOSU_ERR_UNSUPPORTED
    );
    let set = Message::TimerSet(TimerSet {
        timer: 5,
        interval_ms: None,
    });
    assert_eq!(
        ingress_a.deliver(MessageRef {
            opcode: set.opcode() as u32,
            flags: set.flags(),
            correlation_id: 0,
            payload: &set.encode_payload().unwrap(),
        }),
        vosu_status::VOSU_OK
    );
    let expected = Message::TimerFired(TimerFired { timer: 5 })
        .encode_payload()
        .unwrap();
    let timer = endpoint_a.wait_for(RECV_TIMEOUT, |frame| {
        frame.opcode == vosu_opcode::VOSU_OP_TIMER_FIRED as u32 && frame.payload == expected
    });
    assert!(
        timer.is_some(),
        "the timer A's ingress set never fired back to A"
    );
    assert!(endpoint_b.frames().is_empty());

    host.destroy_all(Duration::from_secs(5));
    for endpoint in [&endpoint_a, &endpoint_b] {
        assert_eq!(endpoint.destroy_calls(), 1);
        assert_eq!(endpoint.terminate_calls(), 0);
    }
}

#[test]
fn process_package_without_a_launcher_is_refused_and_leaves_nothing() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let (_dir, loaded) = process_package("test.h0.unlaunched", "h0-unlaunched");
    let id = PluginInstanceId::new(9_201);
    assert!(matches!(
        host.create_instance(id, &loaded, std::env::temp_dir(), None),
        Err(crate::plugin::HostError::ProcessRuntimeUnavailable)
    ));
    assert_eq!(
        registered(&host, "test.h0.unlaunched"),
        (false, false, false)
    );
    assert!(host.instances.lock().unwrap().is_empty());

    // Nothing stale blocks a later, launchable attempt.
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    host.create_instance(id, &loaded, std::env::temp_dir(), None)
        .unwrap();
    assert_eq!(registered(&host, "test.h0.unlaunched"), (true, true, true));
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn failed_process_readiness_unregisters_only_its_own_state() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    // An unrelated live provider whose registrations must survive.
    let (_bystander_dir, bystander) = process_package("test.h0.bystander", "h0-bystander");
    host.create_instance(
        PluginInstanceId::new(9_300),
        &bystander,
        std::env::temp_dir(),
        None,
    )
    .unwrap();

    let (_dir, loaded) = process_package("test.h0.unready", "h0-unready");
    launcher.set_behavior(LaunchBehavior::FailStartup(
        crate::plugin::process::control::StartupStage::Create,
    ));
    assert!(matches!(
        host.create_instance(
            PluginInstanceId::new(9_301),
            &loaded,
            std::env::temp_dir(),
            None
        ),
        Err(crate::plugin::HostError::ProcessStartupFailed(_))
    ));
    {
        let launches = launcher.launches();
        assert_eq!(launches.len(), 2);
        assert_eq!(launches[1].endpoint.terminate_calls(), 1);
        assert_eq!(launches[1].endpoint.destroy_calls(), 0);
    }
    let state = host.state().registry.read().unwrap();
    assert!(
        state.get("test.h0.unready").is_none(),
        "namespace survived failed readiness"
    );
    assert!(
        state.get("test.h0.bystander").is_some(),
        "failure unregistered another provider"
    );
    drop(state);
    let actions = host.action_state.lock().unwrap();
    assert!(!actions.descriptors.contains_key("test.h0.unready.run"));
    assert!(actions.descriptors.contains_key("test.h0.bystander.run"));
    drop(actions);
    // The failed plugin's process is never offered to anyone; the
    // bystander's still is.
    host.process_appeared(test_process(9_390, "h0-unready"));
    assert!(
        host.sessions
            .lock()
            .unwrap()
            .handle_for_pid(9_390)
            .is_none()
    );
    host.process_appeared(test_process(9_391, "h0-bystander"));
    assert!(
        host.sessions
            .lock()
            .unwrap()
            .handle_for_pid(9_391)
            .is_some()
    );
    assert!(
        !host
            .instances
            .lock()
            .unwrap()
            .contains_key(&PluginInstanceId::new(9_301))
    );

    // Readiness is retried by a new creation, which registers everything.
    launcher.set_behavior(LaunchBehavior::Ready);
    host.create_instance(
        PluginInstanceId::new(9_302),
        &loaded,
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    assert_eq!(registered(&host, "test.h0.unready"), (true, true, true));
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn loss_before_publication_fails_creation_and_terminates_the_child() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::LoseBeforeReady);
    host.install_process_launcher(launcher.clone()).unwrap();
    let (_dir, loaded) = process_package("test.h0.early", "h0-early");
    assert!(matches!(
        host.create_instance(
            PluginInstanceId::new(9_401),
            &loaded,
            std::env::temp_dir(),
            None
        ),
        Err(crate::plugin::HostError::InstanceLost)
    ));
    let launches = launcher.launches();
    assert_eq!(launches[0].endpoint.terminate_calls(), 1);
    assert_eq!(launches[0].endpoint.destroy_calls(), 0);
    assert!(
        !launches[0].loss.report(LossCause::ChannelFailed),
        "loss is reported once per instance"
    );
    drop(launches);
    assert_eq!(registered(&host, "test.h0.early"), (false, false, false));
    assert!(host.instances.lock().unwrap().is_empty());
}

#[test]
fn lost_process_instance_is_retired_exactly_once() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    let (_dir, loaded) = process_package("test.h0.lost", "h0-lost");
    let id = PluginInstanceId::new(9_501);
    host.create_instance(id, &loaded, std::env::temp_dir(), None)
        .unwrap();
    host.process_appeared(test_process(9_590, "h0-lost"));
    let (endpoint, loss) = {
        let launches = launcher.launches();
        (Arc::clone(&launches[0].endpoint), launches[0].loss.clone())
    };
    // The attach went through state delivery onto the endpoint.
    assert!(
        endpoint
            .wait_for_frame(vosu_opcode::VOSU_OP_SESSION_ATTACHED as u32, RECV_TIMEOUT)
            .is_some()
    );
    assert!(
        host.sessions
            .lock()
            .unwrap()
            .handle_for_pid(9_590)
            .is_some()
    );

    // The exit watcher and the channel reader race to report the loss.
    let reports: Vec<_> = [
        LossCause::ProcessExited { status: Some(0) },
        LossCause::ChannelFailed,
    ]
    .into_iter()
    .map(|cause| {
        let loss = loss.clone();
        std::thread::spawn(move || loss.report(cause))
    })
    .collect();
    let first_reports = reports
        .into_iter()
        .map(|report| report.join().unwrap())
        .filter(|first| *first)
        .count();
    assert_eq!(first_reports, 1);
    wait_until_removed(&host, id);

    assert_eq!(endpoint.terminate_calls(), 1);
    assert_eq!(
        endpoint.destroy_calls(),
        0,
        "a lost child gets no destroy round trip"
    );
    assert_eq!(registered(&host, "test.h0.lost"), (false, false, false));
    assert!(
        host.sessions
            .lock()
            .unwrap()
            .handle_for_pid(9_590)
            .is_none()
    );
    assert!(matches!(
        host.dispatch_to_instance(id, &Message::TimerFired(TimerFired { timer: 1 }), 0),
        Err(crate::plugin::HostError::UnknownInstance)
    ));

    // A replacement instance registers everything again and is offered the
    // still-running game process afresh.
    host.create_instance(
        PluginInstanceId::new(9_502),
        &loaded,
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    assert_eq!(registered(&host, "test.h0.lost"), (true, true, true));
    host.process_appeared(test_process(9_590, "h0-lost"));
    let replacement = Arc::clone(&launcher.launches()[1].endpoint);
    assert!(
        replacement
            .wait_for_frame(vosu_opcode::VOSU_OP_SESSION_ATTACHED as u32, RECV_TIMEOUT)
            .is_some()
    );
    host.destroy_all(Duration::from_secs(5));
    assert_eq!(endpoint.terminate_calls(), 1);
}

#[test]
fn bundled_packages_never_reach_the_process_launcher() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    let events = super::helpers::create_session_instance(
        &host,
        PluginInstanceId::new(9_601),
        "test.h0.bundled",
        &["h0-bundled"],
        vosu_plugin_testkit::session::SessionPluginConfig::default(),
    );
    host.process_appeared(test_process(9_690, "h0-bundled"));
    assert!(
        events.recv_timeout(RECV_TIMEOUT).is_ok(),
        "bundled code ran in-process"
    );
    assert!(launcher.launches().is_empty());
    host.destroy_all(Duration::from_secs(5));
}

/// Bundled-only authority follows the package's explicit origin. A process
/// package has no local library either, so inferring "bundled" from a
/// missing library handle (the pre-H0 rule) would hand an external plugin
/// the in-process `vosu-surface` reader grant (`docs/v2/plugin-abi.md`
/// §7.5) and official schema provenance (M6a Q3).
#[test]
fn process_package_never_receives_bundled_surface_authority() {
    use vosu_surface::state::{SessionTarget, SurfaceError};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_logs, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    let (_dir, loaded) = process_package("test.h0.surface", "h0-surface");
    host.create_instance(
        PluginInstanceId::new(9_701),
        &loaded,
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    let selector = vosu_plugin_api::Selector {
        ns: "vosu.client".into(),
        pointer: String::new(),
    };
    assert_eq!(
        host.surface_host()
            .read("test.h0.surface", SessionTarget::Focused, &selector),
        Err(SurfaceError::UnknownReader)
    );
    let entries = host.instances.lock().unwrap();
    let Some(InstanceSlot::Live(entry)) = entries.get(&PluginInstanceId::new(9_701)) else {
        panic!("process instance is live");
    };
    assert!(!entry.publisher.bundled);
    drop(entries);
    host.destroy_all(Duration::from_secs(5));
}

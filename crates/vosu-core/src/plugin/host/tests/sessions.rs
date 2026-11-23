//! Session lifecycle tests: decline re-offering, a failed `create`
//! unregistering its matcher, and the identification-timeout sweep.
//! Split out of `tests.rs` so that module stays under the maintained-file
//! line limit (`docs/architecture/source-map.md`).

use std::time::{Duration, Instant};

use vosu_plugin_api::DetachReason;
use vosu_plugin_testkit::session::{self, OnAttach, SessionEvent, SessionPluginConfig};

use crate::plugin::host::*;
use crate::plugin::{focus, registry};

use super::helpers::{
    RECV_TIMEOUT, create_session_instance, create_session_instance_from_manifest, session_manifest,
    session_manifest_by_path_suffix, test_host, test_host_with, test_process,
};

/// D6: a declined session is detached from the decliner
/// (`SESSION_DETACHED{reason: declined}`) and re-offered, same
/// generation, to the next candidate in install order, which then
/// identifies normally.
#[test]
fn decline_re_offers_to_next_candidate_and_detaches_decliner() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let decliner_events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.decliner",
        &["shared.exe"],
        SessionPluginConfig {
            focus_priority: false,
            on_attach: OnAttach::Decline,
            ..Default::default()
        },
    );
    let accepter_events = create_session_instance_from_manifest(
        &host,
        PluginInstanceId::new(2),
        "testkit.session.accepter",
        SessionPluginConfig::default(),
        &session_manifest_by_path_suffix("testkit.session.accepter", "shared.exe"),
    );

    host.process_appeared(test_process(300, "shared.exe"));

    let declined_handle = match decliner_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("decliner attach")
    {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match decliner_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("decliner detach")
    {
        SessionEvent::Detached(detached) => {
            assert_eq!(detached.handle, declined_handle);
            assert_eq!(detached.reason, DetachReason::Declined);
        }
        other => panic!("expected Detached, got {other:?}"),
    }
    assert!(
        decliner_events.try_recv().is_err(),
        "the decliner must not be re-offered its own declined session"
    );

    let reoffered_handle = match accepter_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("accepter attach")
    {
        SessionEvent::Attached(attached) => {
            assert_eq!(attached.pid, 300);
            attached.handle
        }
        other => panic!("expected Attached, got {other:?}"),
    };
    match accepter_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("accepter focus")
    {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, reoffered_handle);
            assert!(focus.focused);
        }
        other => panic!("expected Focus, got {other:?}"),
    }
}

/// P2-5: a plugin whose own `create` fails must not keep the `[match]`
/// table `register` installed before `create` ran — otherwise a process
/// matching it is handed to a plugin id with no live instance forever,
/// and a later-installed, shadowed plugin that would otherwise have
/// matched it never gets the chance.
#[test]
fn failed_create_unregisters_the_matcher_so_a_shadowed_plugin_still_gets_the_session() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());

    // The failing plugin: registered first (install order), its own
    // `create` always fails (`fail_create`).
    let failing_events = session::register_with_config(
        "testkit.session.failing",
        SessionPluginConfig {
            fail_create: true,
            ..Default::default()
        },
    );
    let failing_loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &session_manifest("testkit.session.failing", &["shared-p25.exe"]),
        vtable: session::vosu_plugin_vtable,
    }])
    .expect("loading the failing testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    let result = host.create_instance(
        PluginInstanceId::new(1),
        &failing_loaded[0],
        data_dir.path().to_path_buf(),
        None,
    );
    assert!(
        result.is_err(),
        "the plugin's own create always fails in this test"
    );

    // A second, shadowed plugin matching the same process by path
    // pattern instead of `process_names` (D18's static overlap check
    // only compares those, so this does not trip `register`'s own
    // refusal): if the first plugin's failed `create` left its matcher
    // registered, this process would still go to it and never reach
    // here.
    // `testkit.session.accepter.p25`: a distinct plugin id from
    // `decline_re_offers_to_next_candidate_and_detaches_decliner`'s own
    // `testkit.session.accepter` — the testkit's own event/config
    // registries are process-wide statics keyed by plugin id (module doc
    // comment), so two tests racing on the same id in parallel can
    // overwrite each other's event channel.
    let accepter_events = create_session_instance_from_manifest(
        &host,
        PluginInstanceId::new(2),
        "testkit.session.accepter.p25",
        SessionPluginConfig::default(),
        &session_manifest_by_path_suffix("testkit.session.accepter.p25", "shared-p25.exe"),
    );

    host.process_appeared(test_process(301, "shared-p25.exe"));

    match accepter_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("accepter attach")
    {
        SessionEvent::Attached(attached) => assert_eq!(attached.pid, 301),
        other => panic!("expected Attached, got {other:?}"),
    }
    assert!(
        failing_events.try_recv().is_err(),
        "the plugin whose create failed must never receive anything"
    );
}

/// §6.2: "If identification does not arrive within 5 seconds of
/// `SESSION_ATTACHED`, core logs once and keeps the session hidden" —
/// exercised with a short injected timeout rather than a real 5-second
/// wait. The pending session is never promoted: `SessionRegistry`
/// still reports it pending, not identified, once the sweep has run.
#[test]
fn identification_timeout_sweep_keeps_an_unidentified_session_hidden() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host_with(
        runtime.handle().clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
        Duration::from_millis(50),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.silent",
        &["proc1.exe"],
        SessionPluginConfig {
            focus_priority: false,
            on_attach: OnAttach::Ignore,
            ..Default::default()
        },
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let handle = match events.recv_timeout(RECV_TIMEOUT).expect("attach") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };

    // The sweep ticks every 1s regardless of `identification_timeout`
    // (`identification_sweep_loop`'s own fixed interval); poll with a
    // bounded deadline for it to have logged this handle's timeout,
    // rather than guessing a fixed sleep covers at least one tick.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let logged = host
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .timeout_logged(handle);
        if logged == Some(true) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "identification_sweep_loop did not log this handle's timeout within 10s"
        );
        #[expect(
            clippy::disallowed_methods,
            reason = "a short bounded poll (10s deadline above) for the background identification_sweep_loop's 1s tick to have run; there is no event to block on instead"
        )]
        std::thread::sleep(Duration::from_millis(20));
    }

    let sessions = host.sessions.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        sessions.is_pending(handle),
        "a timed-out session stays pending, not dropped"
    );
    assert!(
        !sessions.is_identified(handle),
        "a timed-out session is never promoted to identified"
    );
    drop(sessions);
    assert!(
        events.try_recv().is_err(),
        "a timed-out session receives no message at all"
    );
}

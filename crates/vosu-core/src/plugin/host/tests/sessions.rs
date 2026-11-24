//! Session lifecycle tests: decline re-offering, a failed `create`
//! unregistering its matcher, and the identification-timeout sweep.
//! Split out of `tests.rs` so that module stays under the maintained-file
//! line limit (`docs/architecture/source-map.md`).

use std::time::{Duration, Instant};

use vosu_plugin_api::{Bitness, DetachReason};
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
        settings_schema: None,
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
/// A plugin whose own `create` blocks on a test-supplied gate until
/// released — lets `matcher_registers_only_once_live_so_a_process_during_creating_goes_to_the_live_winner`
/// hold `create_instance` open across a `process_appeared` call, proving
/// F3: the `[match]` table is not yet registered while this id is still
/// `Creating`, so that process can never be attached to it.
mod gated_matcher_plugin {
    use std::collections::HashMap;
    use std::sync::{Arc, Condvar, Mutex};

    use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices};

    pub struct CreateGate {
        release: Mutex<bool>,
        release_cv: Condvar,
    }

    impl CreateGate {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                release: Mutex::new(false),
                release_cv: Condvar::new(),
            })
        }

        pub fn release(&self) {
            *self.release.lock().unwrap_or_else(|p| p.into_inner()) = true;
            self.release_cv.notify_all();
        }

        fn wait_for_release(&self) {
            let mut release = self.release.lock().unwrap_or_else(|p| p.into_inner());
            while !*release {
                release = self
                    .release_cv
                    .wait(release)
                    .unwrap_or_else(|p| p.into_inner());
            }
        }
    }

    fn gate_registry() -> &'static Mutex<HashMap<String, Arc<CreateGate>>> {
        static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<CreateGate>>>> =
            std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
        &REGISTRY
    }

    pub fn register_gate(plugin_id: impl Into<String>, gate: Arc<CreateGate>) {
        gate_registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(plugin_id.into(), gate);
    }

    pub struct GatedMatcherPlugin;

    impl Plugin for GatedMatcherPlugin {
        fn create(_host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let gate = gate_registry()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| panic!("no create gate registered for {:?}", init.plugin_id));
            gate.wait_for_release();
            Ok(Self)
        }

        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled GatedMatcherPlugin);
}

/// F3: a process discovered while a plugin id is still `Creating` must
/// never be attached to it — the `[match]` table is registered only once
/// `create_instance` is about to publish the instance `Live`. Gates
/// plugin A's own `create` open, confirms its `Creating` placeholder
/// exists, then matches a process against both A (installed first) and
/// B (installed second, already `Live`): since A's matcher is not yet
/// registered, B is the only live candidate and gets it directly — A
/// never receives anything, even once its own `create` finally finishes.
#[test]
fn matcher_registers_only_once_live_so_a_process_during_creating_goes_to_the_live_winner() {
    let plugin_a_id = "testkit.session.gatedmatcher.a";
    let gate = gated_matcher_plugin::CreateGate::new();
    gated_matcher_plugin::register_gate(plugin_a_id, Arc::clone(&gate));

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());

    let manifest_a = session_manifest(plugin_a_id, &["shared-gatedmatcher.exe"]);
    let loaded_a = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest_a,
        settings_schema: None,
        vtable: gated_matcher_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the gated-matcher test plugin");
    let data_dir_a = tempfile::tempdir().unwrap();
    let id_a = PluginInstanceId::new(1);

    std::thread::scope(|scope| {
        let host_ref = &host;
        let loaded_a_ref = &loaded_a[0];
        let data_dir_a_path = data_dir_a.path().to_path_buf();
        let create_thread = scope
            .spawn(move || host_ref.create_instance(id_a, loaded_a_ref, data_dir_a_path, None));

        // Wait for A's `Creating` placeholder to actually exist, rather
        // than assuming a fixed sleep covers the window before `create`
        // reaches the gate.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let creating = {
                let guard = host.instances.lock().unwrap_or_else(|p| p.into_inner());
                matches!(guard.get(&id_a), Some(InstanceSlot::Creating { .. }))
            };
            if creating {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected A's Creating placeholder to appear within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        // B: installed second, but already Live by the time the process
        // appears — the only live candidate while A is still Creating.
        // Matches by path pattern, not `process_names` (D18's static
        // overlap check only compares those), so this does not trip
        // `register`'s own refusal against A's identical exact name —
        // `failed_create_unregisters_the_matcher...`'s own doc comment.
        let events_b = create_session_instance_from_manifest(
            &host,
            PluginInstanceId::new(2),
            "testkit.session.gatedmatcher.b",
            SessionPluginConfig::default(),
            &session_manifest_by_path_suffix(
                "testkit.session.gatedmatcher.b",
                "shared-gatedmatcher.exe",
            ),
        );

        host.process_appeared(test_process(400, "shared-gatedmatcher.exe"));
        match events_b.recv_timeout(RECV_TIMEOUT).expect("B's attach") {
            SessionEvent::Attached(attached) => assert_eq!(attached.pid, 400),
            other => panic!("expected Attached, got {other:?}"),
        }

        gate.release();
        create_thread
            .join()
            .unwrap()
            .expect("creating the gated-matcher instance");
    });
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

/// A1b/DA: `SESSION_ATTACHED` carries the process facts the matcher and
/// core already have (`name`, `path`, `command_line`, `argv`), not just
/// `pid`/`bitness`.
#[test]
fn attach_carries_process_facts() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.facts",
        &["facts.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(ProcessRecord {
        pid: 400,
        name: "facts.exe".to_owned(),
        path: "/games/facts/facts.exe".to_owned(),
        command_line: "facts.exe --tournament".to_owned(),
        bitness: Bitness::X64,
        start_time: 1,
        argv: Some(vec!["facts.exe".to_owned(), "--tournament".to_owned()]),
    });

    match events.recv_timeout(RECV_TIMEOUT).expect("attach") {
        SessionEvent::Attached(attached) => {
            assert_eq!(attached.pid, 400);
            assert_eq!(attached.name, Some("facts.exe".to_owned()));
            assert_eq!(attached.path, Some("/games/facts/facts.exe".to_owned()));
            assert_eq!(
                attached.command_line,
                Some("facts.exe --tournament".to_owned())
            );
            assert_eq!(
                attached.argv,
                Some(vec!["facts.exe".to_owned(), "--tournament".to_owned()])
            );
        }
        other => panic!("expected Attached, got {other:?}"),
    }
}

/// D6: a `SESSION_DECLINE` re-offer is still the same discovered
/// occurrence — the process facts travel with it unchanged, not just
/// `pid`/`bitness`.
#[test]
fn reoffer_keeps_process_facts() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    // Distinct plugin ids from `decline_re_offers_to_next_candidate_and_
    // detaches_decliner`'s own `testkit.session.decliner`/
    // `testkit.session.accepter` — the testkit's own event/config
    // registries are process-wide statics keyed by plugin id (this
    // module's own doc comment), so two tests racing on the same id in
    // parallel overwrite each other's event channel (this one and that
    // one flaked natively, intermittently, from exactly that collision).
    let decliner_events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.decliner.reoffer",
        &["shared2.exe"],
        SessionPluginConfig {
            focus_priority: false,
            on_attach: OnAttach::Decline,
            ..Default::default()
        },
    );
    let accepter_events = create_session_instance_from_manifest(
        &host,
        PluginInstanceId::new(2),
        "testkit.session.accepter.reoffer",
        SessionPluginConfig::default(),
        &session_manifest_by_path_suffix("testkit.session.accepter.reoffer", "shared2.exe"),
    );

    host.process_appeared(ProcessRecord {
        pid: 500,
        name: "shared2.exe".to_owned(),
        path: "/games/shared/shared2.exe".to_owned(),
        command_line: "shared2.exe --debug-client-id=1".to_owned(),
        bitness: Bitness::X64,
        start_time: 1,
        argv: Some(vec![
            "shared2.exe".to_owned(),
            "--debug-client-id=1".to_owned(),
        ]),
    });

    match decliner_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("decliner attach")
    {
        SessionEvent::Attached(_) => {}
        other => panic!("expected Attached, got {other:?}"),
    }
    match decliner_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("decliner detach")
    {
        SessionEvent::Detached(_) => {}
        other => panic!("expected Detached, got {other:?}"),
    }

    match accepter_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("accepter attach")
    {
        SessionEvent::Attached(attached) => {
            assert_eq!(attached.pid, 500);
            assert_eq!(attached.name, Some("shared2.exe".to_owned()));
            assert_eq!(attached.path, Some("/games/shared/shared2.exe".to_owned()));
            assert_eq!(
                attached.command_line,
                Some("shared2.exe --debug-client-id=1".to_owned())
            );
            assert_eq!(
                attached.argv,
                Some(vec![
                    "shared2.exe".to_owned(),
                    "--debug-client-id=1".to_owned()
                ])
            );
        }
        other => panic!("expected Attached, got {other:?}"),
    }
}

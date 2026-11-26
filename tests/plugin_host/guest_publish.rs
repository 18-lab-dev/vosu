//! Non-owner own-namespace publication (`docs/v2/plugin-abi.md` §6.6/§6.7,
//! 2026-11-26) end to end through the public host API: a fixture tool
//! plugin learns a game plugin's session from `vosu.sessions`, receives
//! demand for its own namespace, publishes it through its own host
//! services for an API v1 subscriber, and its fields leave with it.

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use vosu_core::api::{ApiPlan, SessionTarget};
use vosu_core::plugin::{self, PluginHost, PluginHostHandler, PluginInstanceId};
use vosu_core::state::{SessionKey, StateValue};
use vosu_plugin_api::{
    HostServices, Message, PublishBatchWriter, PublishValueRef, Selector, StateSubscribe,
    vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::publisher::{self, PublisherEvent};

const WAIT: Duration = Duration::from_secs(10);
const OWNER: &str = "test.guest-e2e.game";
const TOOL: &str = "test.guest-e2e.tool";

struct Storage;
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, plugin::settings::SettingsError> {
        Ok(Default::default())
    }
    fn store(
        &self,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        Ok(())
    }
}

struct Console;
impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}

fn load(
    host: &Arc<PluginHost>,
    n: u64,
    id: &str,
    manifest: &str,
    schemas: &[&str],
) -> Receiver<PublisherEvent> {
    publisher::register_manifest(id, manifest);
    let events = publisher::register(id);
    let loaded = plugin::load_bundled(
        &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
        &host.validation_snapshot(),
        &[plugin::BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: schemas,
            action_schemas: &[],
            vtable: publisher::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded[0],
        std::env::temp_dir(),
        Some(plugin::WorkerPool::new(2, 64)),
    )
    .unwrap();
    events
}

fn manifest(id: &str, rest: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nconcurrent_dispatch=true\n{rest}"
    )
}

fn until<R>(
    events: &Receiver<PublisherEvent>,
    mut f: impl FnMut(PublisherEvent) -> Option<R>,
) -> R {
    let deadline = Instant::now() + WAIT;
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("bounded event deadline");
        if let Some(result) = f(event) {
            return result;
        }
    }
}

fn publish(id: &str, handle: u64, ns: &str, pointer: &str, value: u64) -> vosu_status {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push(ns, pointer, PublishValueRef::U64(value));
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes)
}

fn test_host(runtime: &tokio::runtime::Runtime, logs: &std::path::Path) -> Arc<PluginHost> {
    let logger = vosu_core::logging::Logger::new(
        logs,
        vosu_core::logging::LoggerOptions {
            version: "test".into(),
            debug_log: true,
            queue_capacity: 64,
        },
        Arc::new(Console),
    )
    .unwrap();
    PluginHost::new_with_foreground(
        logger,
        Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
            Storage,
        ))),
        runtime.handle().clone(),
        Arc::new(plugin::SyntheticForegroundSource::new(None)),
    )
}

#[test]
fn non_owner_tool_publishes_its_own_namespace_for_api_v1_and_leaves_with_its_plugin() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let logs = tempfile::tempdir().unwrap();
    let host = test_host(&runtime, logs.path());

    let game = load(
        &host,
        1,
        OWNER,
        &manifest(
            OWNER,
            &format!(
                "covers=['field:vosu.play.combo']\n[match]\nprocess_names=['{OWNER}']\n[[provides]]\nid='game:fixture'\nversion='*'\n"
            ),
        ),
        &[],
    );
    host.process_appeared(plugin::ProcessRecord {
        pid: 4801,
        name: OWNER.into(),
        path: format!("/games/{OWNER}"),
        command_line: OWNER.into(),
        argv: Some(vec![OWNER.into()]),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 1,
    });
    let handle = until(&game, |event| match event {
        PublisherEvent::Attached { handle } => Some(handle),
        _ => None,
    });

    let schema = format!(
        r#"{{"title":"{TOOL}","version":"1.0.0","type":"object","properties":{{"timeline":{{"type":"integer","x-sampling-lane":{{"lane":"regular","activity":"always"}}}}}}}}"#
    );
    let tool = load(
        &host,
        2,
        TOOL,
        &manifest(
            TOOL,
            &format!("[[provides]]\nid='field:{TOOL}'\nversion='1.0.0'\nschema='state.json'\n"),
        ),
        &[&schema],
    );
    let path = format!("/{TOOL}/timeline");
    let plan = ApiPlan::new(&[&path]).unwrap();
    let api = host.api_state_source();
    let mut subscription = api.subscribe(SessionTarget::Active, plan.clone()).unwrap();

    assert_eq!(
        host.handle(
            PluginInstanceId::new(2),
            Message::StateSubscribe(StateSubscribe {
                session: None,
                selectors: vec![Selector {
                    ns: "vosu.sessions".into(),
                    pointer: String::new(),
                }],
            }),
            0,
        ),
        vosu_status::VOSU_OK
    );
    let demand = until(&tool, |event| match event {
        PublisherEvent::Demand(demand) => Some(demand),
        _ => None,
    });
    assert_eq!(demand.handle, handle);
    assert!(demand.regular && !demand.precise);
    assert_eq!(demand.selectors.len(), 1);
    assert_eq!(demand.selectors[0].ns, TOOL);

    assert_eq!(
        publish(TOOL, handle, TOOL, "/timeline", 12),
        vosu_status::VOSU_OK
    );
    let deadline = Instant::now() + WAIT;
    while subscription.updates.borrow().values[0] != Some(StateValue::U64(12)) {
        runtime
            .block_on(tokio::time::timeout(
                deadline.saturating_duration_since(Instant::now()),
                subscription.changed(),
            ))
            .unwrap()
            .unwrap();
    }
    for (ns, pointer) in [("vosu.play", "/combo"), (OWNER, "/x")] {
        assert_eq!(
            publish(TOOL, handle, ns, pointer, 1),
            vosu_status::VOSU_ERR_PERMISSION_DENIED
        );
    }

    host.teardown_instance(PluginInstanceId::new(2)).unwrap();
    assert!(
        host.state()
            .store
            .current(SessionKey(handle))
            .unwrap()
            .namespace(TOOL)
            .is_none()
    );
    assert_eq!(
        api.read(&SessionTarget::Active, &plan).unwrap().values,
        vec![None]
    );
    drop(subscription);
    host.destroy_all(WAIT);
}

mod action_relay {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::{SyncSender, sync_channel};

    use vosu_plugin_api::{
        ActionCall, ActionResult, ActionStatus, CallerKind, CborValue, ConcurrentPlugin, Init,
        PluginError, TimerFired, VtableHostServices,
    };

    const CALLER: &str = "test.guest-action-replay.game";
    const CALLEE: &str = "test.guest-action-replay.callee";
    const ACTION: &str = "test.guest-action-replay.callee.run";

    struct Hooks {
        events: SyncSender<Message>,
        release_attach: Mutex<Receiver<()>>,
    }
    static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);

    struct Callee {
        host: VtableHostServices,
        hooks: Hooks,
    }

    impl ConcurrentPlugin for Callee {
        fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
            let hooks = HOOKS.lock().unwrap().take().unwrap();
            Ok(Self { host, hooks })
        }

        fn on_message(&self, message: Message, _: u64) -> Result<(), PluginError> {
            match &message {
                Message::SessionAttached(_) => {
                    self.hooks.events.try_send(message).unwrap();
                    self.hooks
                        .release_attach
                        .lock()
                        .unwrap()
                        .recv_timeout(WAIT)
                        .unwrap();
                }
                Message::ActionInvoke(invoke) => {
                    assert_eq!(
                        self.host.send(
                            &Message::ActionResult(ActionResult {
                                invocation: invoke.invocation,
                                status: ActionStatus::Ok,
                                value: Some(invoke.args.clone()),
                                code: None,
                                message: None,
                            }),
                            0
                        ),
                        vosu_status::VOSU_OK
                    );
                    self.hooks.events.try_send(message).unwrap();
                }
                Message::TimerFired(_) => self.hooks.events.try_send(message).unwrap(),
                _ => {}
            }
            Ok(())
        }
    }
    vosu_plugin_internal::export_plugin!(bundled concurrent Callee);

    #[test]
    fn action_call_uses_the_guest_handle_after_a_concurrent_watcher_attach() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let logs = tempfile::tempdir().unwrap();
        let host = test_host(&runtime, logs.path());
        let (events, replies) = sync_channel(8);
        let (release, release_attach) = sync_channel(1);
        *HOOKS.lock().unwrap() = Some(Hooks {
            events,
            release_attach: Mutex::new(release_attach),
        });
        let callee = PluginInstanceId::new(2);
        let loaded = plugin::load_bundled(
            &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
            &host.validation_snapshot(),
            &[plugin::BundledPlugin {
                manifest: &manifest(
                    CALLEE,
                    &format!(
                        "[[actions]]\nname='run'\nversion='1.0.0'\ntitle='Guest'\nargs_schema='args.json'\nresult_schema='result.json'\nsession='required'\n[[provides]]\nid='action:{ACTION}'\nversion='1.0.0'\n"
                    ),
                ),
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[("args.json", "{}"), ("result.json", "{}")],
                vtable: vosu_plugin_vtable,
            }],
        )
        .unwrap();
        host.create_instance(
            callee,
            &loaded[0],
            logs.path().into(),
            Some(plugin::WorkerPool::new(2, 64)),
        )
        .unwrap();
        let game = load(
            &host,
            1,
            CALLER,
            &manifest(
                CALLER,
                &format!(
                    "[match]\nprocess_names=['{CALLER}']\n[[provides]]\nid='game:fixture'\nversion='*'\n[[requires]]\nid='action:{ACTION}'\nversion='^1.0'\n"
                ),
            ),
            &[],
        );
        host.process_appeared(plugin::ProcessRecord {
            pid: 4802,
            name: CALLER.into(),
            path: format!("/games/{CALLER}"),
            command_line: CALLER.into(),
            argv: Some(vec![CALLER.into()]),
            bitness: vosu_plugin_api::Bitness::X64,
            start_time: 1,
        });
        let handle = until(&game, |event| match event {
            PublisherEvent::Attached { handle } => Some(handle),
            _ => None,
        });
        assert_eq!(
            host.handle(
                callee,
                Message::StateSubscribe(StateSubscribe {
                    session: None,
                    selectors: vec![Selector {
                        ns: "vosu.sessions".into(),
                        pointer: String::new(),
                    }],
                }),
                0,
            ),
            vosu_status::VOSU_OK
        );
        let Message::SessionAttached(attached) = replies.recv_timeout(WAIT).unwrap() else {
            panic!("the callee must first learn the handle");
        };
        assert_eq!(attached.handle, handle);
        assert!(attached.path.is_none());
        let args = CborValue::Text("guest relay".into());
        assert_eq!(
            publisher::host_services(CALLER).send(
                &Message::ActionCall(ActionCall {
                    invocation: 42,
                    action: ACTION.into(),
                    session: Some(handle),
                    args: args.clone(),
                }),
                0
            ),
            vosu_status::VOSU_OK
        );

        // Another worker can process unrelated work while this attachment
        // callback is held, but must not invoke the session-bound action.
        let probe = Message::TimerFired(TimerFired { timer: 7 });
        host.dispatch_to_instance(callee, &probe, 0).unwrap();
        assert_eq!(replies.recv_timeout(WAIT).unwrap(), probe);
        release.send(()).unwrap();
        let Message::ActionInvoke(invoke) = replies.recv_timeout(WAIT).unwrap() else {
            panic!("the existing attachment must not be duplicated");
        };
        assert_eq!(invoke.session, Some(attached.handle));
        assert_eq!(invoke.action, ACTION);
        assert_eq!(invoke.caller.kind, CallerKind::Plugin);
        assert_eq!(invoke.caller.id, CALLER);
        assert_eq!(invoke.args, args);
        host.destroy_all(WAIT);
    }
}

mod action_subscribe {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc::{SyncSender, sync_channel};
    use vosu_plugin_api::{
        ActionCall, Init, Plugin, PluginError, StateEntry, TimerFired, VtableHostServices,
    };

    const GAME: &str = "test.guest-action-read.game";
    const CALLEE: &str = "test.guest-action-read.callee";
    const ACTION: &str = "test.guest-action-read.callee.run";

    type Event = (Message, Option<vosu_status>);

    struct Hooks {
        events: SyncSender<Event>,
        release_baseline: Receiver<()>,
    }
    static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);

    fn subscribe(session: u64) -> Message {
        Message::StateSubscribe(StateSubscribe {
            session: Some(session),
            selectors: vec![Selector {
                ns: "vosu.play".into(),
                pointer: "/combo".into(),
            }],
        })
    }

    struct Callee {
        host: VtableHostServices,
        events: SyncSender<Event>,
        release_baseline: Option<Receiver<()>>,
    }

    impl Plugin for Callee {
        fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
            let hooks = HOOKS.lock().unwrap().take().unwrap();
            Ok(Self {
                host,
                events: hooks.events,
                release_baseline: Some(hooks.release_baseline),
            })
        }

        fn on_message(&mut self, message: Message, _: u64) -> Result<(), PluginError> {
            let status = match &message {
                Message::ActionInvoke(invoke) => {
                    Some(self.host.send(&subscribe(invoke.session.unwrap()), 0))
                }
                Message::SessionDetached(detached) => {
                    Some(self.host.send(&subscribe(detached.handle), 0))
                }
                Message::SessionAttached(_) | Message::StateUpdate(_) | Message::TimerFired(_) => {
                    None
                }
                _ => return Ok(()),
            };
            let update = matches!(message, Message::StateUpdate(_));
            self.events.try_send((message, status)).unwrap();
            if update && let Some(release) = self.release_baseline.take() {
                release.recv_timeout(WAIT).unwrap();
            }
            Ok(())
        }
    }
    vosu_plugin_internal::export_plugin!(bundled Callee);

    fn assert_update(event: Event, handle: u64, combo: u64) {
        let (Message::StateUpdate(update), None) = event else {
            panic!("expected STATE_UPDATE, got {event:?}");
        };
        assert_eq!(update.session, handle);
        assert!(!update.more);
        assert_eq!(
            update.values,
            [StateEntry {
                ns: "vosu.play".into(),
                pointer: "/combo".into(),
                value: Some(combo.into()),
                error: None,
            }]
        );
    }

    #[test]
    fn action_provider_can_subscribe_only_to_its_announced_session_until_detach() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let logs = tempfile::tempdir().unwrap();
        let host = test_host(&runtime, logs.path());
        let (events, received) = sync_channel(8);
        let (release, release_baseline) = sync_channel(1);
        *HOOKS.lock().unwrap() = Some(Hooks {
            events,
            release_baseline,
        });
        let callee = PluginInstanceId::new(2);
        let loaded = plugin::load_bundled(
            &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
            &host.validation_snapshot(),
            &[plugin::BundledPlugin {
                manifest: &manifest(
                    CALLEE,
                    &format!(
                        "[[actions]]\nname='run'\nversion='1.0.0'\ntitle='Guest'\nargs_schema='args.json'\nresult_schema='result.json'\nsession='required'\n[[provides]]\nid='action:{ACTION}'\nversion='1.0.0'\n"
                    ),
                )
                .replace("concurrent_dispatch=true", "concurrent_dispatch=false"),
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[("args.json", "{}"), ("result.json", "{}")],
                vtable: vosu_plugin_vtable,
            }],
        )
        .unwrap();
        host.create_instance(callee, &loaded[0], logs.path().into(), None)
            .unwrap();
        let game = load(
            &host,
            1,
            GAME,
            &manifest(
                GAME,
                &format!(
                    "covers=['field:vosu.play.combo']\n[match]\nprocess_names=['{GAME}']\n[[provides]]\nid='game:fixture'\nversion='*'\n[[requires]]\nid='action:{ACTION}'\nversion='^1.0'\n"
                ),
            ),
            &[],
        );
        let attach = |pid| {
            host.process_appeared(plugin::ProcessRecord {
                pid,
                name: GAME.into(),
                path: format!("/games/{GAME}"),
                command_line: GAME.into(),
                argv: None,
                bitness: vosu_plugin_api::Bitness::X64,
                start_time: 1,
            });
            until(&game, |event| match event {
                PublisherEvent::Attached { handle } => Some(handle),
                _ => None,
            })
        };
        let (handle, other) = (attach(4803), attach(4804));
        assert_ne!(handle, other);
        assert_eq!(
            publish(GAME, handle, "vosu.play", "/combo", 12),
            vosu_status::VOSU_OK
        );
        assert_eq!(
            host.handle(callee, subscribe(handle), 0),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            publisher::host_services(GAME).send(
                &Message::ActionCall(ActionCall {
                    invocation: 42,
                    action: ACTION.into(),
                    session: Some(handle),
                    args: vosu_plugin_api::CborValue::Text("read combo".into()),
                }),
                0
            ),
            vosu_status::VOSU_OK
        );
        let next = || received.recv_timeout(WAIT).unwrap();
        assert!(
            matches!(next(), (Message::SessionAttached(attached), None) if attached.handle == handle)
        );
        assert!(
            matches!(next(), (Message::ActionInvoke(invoke), Some(vosu_status::VOSU_OK)) if invoke.session == Some(handle))
        );
        assert_update(next(), handle, 12);
        assert_eq!(
            host.handle(callee, subscribe(other), 0),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );

        // The worker is still in the baseline callback, so the last update
        // and the terminal must both wait in the same session's delivery slot.
        assert_eq!(
            publish(GAME, handle, "vosu.play", "/combo", 13),
            vosu_status::VOSU_OK
        );
        host.process_exited(4803);
        release.send(()).unwrap();
        assert_update(next(), handle, 13);
        assert!(
            matches!(next(), (Message::SessionDetached(detached), Some(vosu_status::VOSU_ERR_INVALID_ARGUMENT)) if detached.handle == handle)
        );
        let probe = Message::TimerFired(TimerFired { timer: 7 });
        host.dispatch_to_instance(callee, &probe, 0).unwrap();
        assert_eq!(next(), (probe, None));
        host.destroy_all(WAIT);
        assert!(matches!(
            received.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }
}

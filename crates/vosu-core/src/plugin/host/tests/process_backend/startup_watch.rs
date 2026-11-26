use super::*;
use crate::plugin::HostError;
use crate::plugin::execution::{ProcessEndpoint, ProcessLaunch, ProcessLauncher};
use crate::plugin::host::tests::helpers::create_session_instance;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use vosu_plugin_api::{
    HostServices, Plugin, PluginError, Selector, StateSubscribe, VtableHostServices,
};
use vosu_plugin_testkit::session::SessionEvent;

fn watch() -> Message {
    Message::StateSubscribe(StateSubscribe {
        session: None,
        selectors: vec![Selector {
            ns: "vosu.sessions".into(),
            pointer: String::new(),
        }],
    })
}

fn start_owner(host: &Arc<PluginHost>, plugin_id: &str) -> Receiver<SessionEvent> {
    create_session_instance(
        host,
        PluginInstanceId::new(9_801),
        plugin_id,
        &["watch-owner"],
        Default::default(),
    )
}

fn identify(host: &PluginHost, owner: &Receiver<SessionEvent>, pid: u32) -> u64 {
    host.process_appeared(test_process(pid, "watch-owner"));
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    loop {
        if let SessionEvent::Attached(attached) = owner
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            assert_eq!(attached.pid, pid);
            return attached.handle;
        }
    }
}

/// Simulates the acknowledged ABI send on the real process ingress while
/// launch is still waiting on READY from the plugin's create callback.
struct StartupLauncher {
    inner: Arc<RecordingLauncher>,
    statuses: SyncSender<[vosu_status; 4]>,
}

impl ProcessLauncher for StartupLauncher {
    fn launch(&self, launch: ProcessLaunch<'_>) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        let send = |message: Message| {
            launch.ingress.deliver(MessageRef {
                opcode: message.opcode() as u32,
                flags: message.flags(),
                correlation_id: 0,
                payload: &message.encode_payload().unwrap(),
            })
        };
        self.statuses
            .send([
                send(watch()),
                send(watch()),
                send(Message::StateSubscribe(StateSubscribe {
                    session: None,
                    selectors: Vec::new(),
                })),
                send(Message::StateSubscribe(StateSubscribe {
                    session: Some(1),
                    selectors: Vec::new(),
                })),
            ])
            .unwrap();
        let endpoint = self.inner.launch(launch)?;
        let launches = self.inner.launches();
        assert!(launches.last().unwrap().endpoint.frames().is_empty());
        Ok(endpoint)
    }
}

#[test]
fn process_sessions_watch_from_create_replays_existing_and_future_sessions() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let owner = start_owner(&host, "test.startup-watch.process-owner");
    let existing = identify(&host, &owner, 9_811);

    let (status_tx, status_rx) = sync_channel(1);
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(Arc::new(StartupLauncher {
        inner: launcher.clone(),
        statuses: status_tx,
    }))
    .unwrap();
    let (dir, provider) = process_package("test.startup-watch.process", "watch-provider");
    let id = PluginInstanceId::new(9_802);
    host.create_instance(id, &provider, dir.path().into(), None)
        .unwrap();
    assert_eq!(
        status_rx.recv_timeout(RECV_TIMEOUT).unwrap(),
        [
            vosu_status::VOSU_OK,
            vosu_status::VOSU_OK,
            vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        ]
    );

    let endpoint = launcher.launches()[0].endpoint.clone();
    let attach_opcode = vosu_opcode::VOSU_OP_SESSION_ATTACHED;
    let existing_frame = endpoint
        .wait_for_frame(attach_opcode as u32, RECV_TIMEOUT)
        .unwrap();
    let parse = |payload: &[u8]| {
        let Message::SessionAttached(attached) =
            Message::decode(attach_opcode, 0, payload).unwrap()
        else {
            panic!("expected SESSION_ATTACHED");
        };
        attached.handle
    };
    assert_eq!(parse(&existing_frame.payload), existing);
    let later = identify(&host, &owner, 9_812);
    assert!(
        endpoint
            .wait_for(RECV_TIMEOUT, |frame| {
                frame.opcode == attach_opcode as u32 && parse(&frame.payload) == later
            })
            .is_some()
    );
    host.destroy_all(RECV_TIMEOUT);
    let attaches = endpoint
        .frames()
        .into_iter()
        .filter(|frame| frame.opcode == attach_opcode as u32)
        .count();
    assert_eq!(attaches, 2);
}

struct LocalWatcher(SyncSender<u64>);
static LOCAL_EVENTS: Mutex<Option<SyncSender<u64>>> = Mutex::new(None);

impl Plugin for LocalWatcher {
    fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
        let events = LOCAL_EVENTS.lock().unwrap().take().unwrap();
        let status = host.send(&watch(), 0);
        if status != vosu_status::VOSU_OK {
            return Err(status.into());
        }
        Ok(Self(events))
    }

    fn on_message(&mut self, message: Message, _: u64) -> Result<(), PluginError> {
        if let Message::SessionAttached(attached) = message {
            self.0.try_send(attached.handle).unwrap();
        }
        Ok(())
    }
}
vosu_plugin_internal::export_plugin!(bundled LocalWatcher);

#[test]
fn local_sessions_watch_from_create_replays_existing_and_future_sessions() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let owner = start_owner(&host, "test.startup-watch.local-owner");
    let existing = identify(&host, &owner, 9_811);

    let (events, attached) = sync_channel(4);
    *LOCAL_EVENTS.lock().unwrap() = Some(events);
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &host.validation_snapshot(),
        &[registry::BundledPlugin {
            manifest: "kind='plugin'\nmanifest_version=1\nid='test.startup-watch.local'\nname='Watch'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1'\nabi_version=1\nruntime='in-process'\n",
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let id = PluginInstanceId::new(9_802);
    host.create_instance(id, &loaded[0], dir.path().into(), None)
        .unwrap();
    assert_eq!(attached.recv_timeout(RECV_TIMEOUT).unwrap(), existing);
    let later = identify(&host, &owner, 9_812);
    assert_eq!(attached.recv_timeout(RECV_TIMEOUT).unwrap(), later);
    host.destroy_all(RECV_TIMEOUT);
}

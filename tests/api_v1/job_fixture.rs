//! Instance-scoped ABI fixtures shared by J3's HTTP and relay composition tests.
use super::support::{Harness, WAIT, manifest};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, mpsc},
};
use vosu_core::{identity::PluginInstanceId, plugin};
use vosu_plugin_api::{
    ActionProgress, ActionResult, ActionStatus, HostServices, Init, Message, Plugin, PluginError,
    VtableHostServices, vosu_status,
};

struct Hooks {
    events: mpsc::SyncSender<Message>,
    gate: mpsc::Receiver<()>,
}
static HOOKS: LazyLock<Mutex<HashMap<String, Hooks>>> = LazyLock::new(Mutex::default);
pub struct Fixture {
    host: VtableHostServices,
    hooks: Hooks,
    pending: Vec<u64>,
}
impl Plugin for Fixture {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        Ok(Self {
            host,
            hooks: HOOKS.lock().unwrap().remove(&init.plugin_id).unwrap(),
            pending: Vec::new(),
        })
    }
    fn on_message(&mut self, message: Message, _: u64) -> Result<(), PluginError> {
        self.hooks.events.try_send(message.clone()).unwrap();
        match message {
            Message::ActionInvoke(a) => {
                assert_eq!(
                    self.host.send(
                        &Message::ActionProgress(ActionProgress {
                            invocation: a.invocation,
                            fraction: Some(0.375),
                            message: Some("J3 progress sentinel".into())
                        }),
                        0
                    ),
                    vosu_status::VOSU_OK
                );
                self.pending.push(a.invocation);
            }
            Message::SessionAttached(a) => {
                assert_eq!(
                    self.host.send(
                        &Message::SessionIdentify(vosu_plugin_api::SessionIdentify {
                            handle: a.handle,
                            game: "jthree".into(),
                            variant: "integration".into(),
                            name: "J3".into(),
                            version: "1.0.0".into(),
                            focus_priority: false
                        }),
                        0
                    ),
                    vosu_status::VOSU_OK
                );
            }
            Message::TimerFired(t) if t.timer == 999 => {
                self.hooks.gate.recv_timeout(WAIT).unwrap();
            }
            Message::TimerFired(t) if t.timer == 4 => {
                for invocation in self.pending.drain(..) {
                    let mut result = ActionResult {
                        invocation,
                        status: ActionStatus::Ok,
                        value: Some(vosu_plugin_api::CborValue::Text(String::new())),
                        code: None,
                        message: None,
                    };
                    let empty = Message::ActionResult(result.clone())
                        .encode_payload()
                        .unwrap()
                        .len();
                    let limit = vosu_plugin_api::VOSU_FRAME_MAX_BYTES as usize;
                    result.value = Some(vosu_plugin_api::CborValue::Text(
                        "r".repeat(limit - empty - 4),
                    ));
                    assert_eq!(
                        Message::ActionResult(result.clone())
                            .encode_payload()
                            .unwrap()
                            .len(),
                        limit
                    );
                    assert_eq!(
                        self.host.send(&Message::ActionResult(result), 0),
                        vosu_status::VOSU_OK
                    );
                }
            }
            Message::TimerFired(t) if t.timer == 1 => {
                for invocation in self.pending.drain(..) {
                    // A timed-out invocation must reject this genuine late ABI success.
                    let status = self.host.send(
                        &Message::ActionResult(ActionResult {
                            invocation,
                            status: ActionStatus::Ok,
                            value: Some(vosu_plugin_api::CborValue::Text(
                                "J3 terminal sentinel".into(),
                            )),
                            code: None,
                            message: None,
                        }),
                        0,
                    );
                    self.hooks
                        .events
                        .try_send(Message::TimerFired(vosu_plugin_api::TimerFired {
                            timer: status as u64,
                        }))
                        .unwrap();
                }
            }
            Message::ActionCancel(a) => {
                // Cancellation is deliberately advisory; only timer=1 finishes.
                assert!(self.pending.contains(&a.invocation));
            }
            _ => {}
        }
        Ok(())
    }
}
vosu_plugin_internal::export_plugin!(bundled Fixture);

pub fn load(
    h: &Harness,
    instance: u64,
    id: &str,
    grants: &str,
) -> (mpsc::Receiver<Message>, mpsc::SyncSender<()>) {
    let (events, rx) = mpsc::sync_channel(2048);
    let (release, gate) = mpsc::sync_channel(1);
    assert!(
        HOOKS
            .lock()
            .unwrap()
            .insert(id.into(), Hooks { events, gate })
            .is_none()
    );
    let mut text = manifest(id);
    for (name, mode) in [
        ("nested.echo", "optional"),
        ("required", "required"),
        ("none", "none"),
    ] {
        text += &format!(
            "\n[[actions]]\nname='{name}'\nversion='1.0.0'\ntitle='J3'\nargs_schema='args.json'\nresult_schema='result.json'\nsession='{mode}'\ncli=true\n[[provides]]\nid='action:{id}.{name}'\nversion='1.0.0'\n"
        );
    }
    text += grants;
    let loaded = plugin::load_bundled(
        &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
        &h.host.validation_snapshot(),
        &[plugin::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[("args.json", "{}"), ("result.json", "{}")],
            vtable: vosu_plugin_vtable,
        }],
    )
    .unwrap();
    h.host
        .create_instance(
            PluginInstanceId::new(instance),
            &loaded[0],
            h.root.path().into(),
            None,
        )
        .unwrap();
    (rx, release)
}
pub fn next(rx: &mpsc::Receiver<Message>, predicate: impl Fn(&Message) -> bool) -> Message {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let message = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if predicate(&message) {
            return message;
        }
    }
}
pub fn timer(h: &Harness, instance: u64, timer: u64) {
    h.host
        .dispatch_to_instance(
            PluginInstanceId::new(instance),
            &Message::TimerFired(vosu_plugin_api::TimerFired { timer }),
            0,
        )
        .unwrap();
}

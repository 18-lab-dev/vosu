//! B0 (AF.12) in-process provider: an action that declares artifacts from
//! inside its own `ACTION_INVOKE` dispatch, then answers once every
//! `ARTIFACT_DECLARED` arrived, and opens each artifact from a plugin-wide
//! root or a generated stream as its opaque key says. Every message it
//! receives is forwarded to the test, in order.
use super::support::{Harness, WAIT, manifest};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{LazyLock, Mutex, mpsc},
};
use vosu_core::{identity::PluginInstanceId, plugin};
use vosu_plugin_api::{
    ActionResult, ActionStatus, ArtifactDeclare, ArtifactDeclared, CborBytes, CborValue, ErrorInfo,
    HostServices, Init, Message, Plugin, PluginError, ResourceResolved, StreamChunk,
    VOSU_STREAM_CHUNK_MAX_BYTES, VtableHostServices, vosu_status,
};

/// The generated stream's byte at `offset`.
pub fn generated(offset: u64) -> u8 {
    ((offset % 251) as u8) ^ ((offset >> 20) as u8)
}

struct Hooks {
    events: mpsc::SyncSender<Message>,
    root: PathBuf,
}
static HOOKS: LazyLock<Mutex<HashMap<String, Hooks>>> = LazyLock::new(Mutex::default);

/// One invocation waiting for its declarations' replies.
struct Waiting {
    remaining: usize,
    declared: Vec<Value>,
}

/// One open stream: its key's spec and where its bytes end.
struct Stream {
    end: u64,
    hold: bool,
}

pub struct ArtifactPlugin {
    host: VtableHostServices,
    events: mpsc::SyncSender<Message>,
    next_correlation: u64,
    /// Declaration correlation → (invocation, index).
    pending: HashMap<u64, (u64, usize)>,
    waiting: HashMap<u64, Waiting>,
    streams: HashMap<u64, Stream>,
    next_stream: u64,
}

impl Plugin for ArtifactPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let hooks = HOOKS.lock().unwrap().remove(&init.plugin_id).unwrap();
        // A plugin-wide root, declared with no session: the only root an
        // artifact open may name.
        assert_eq!(
            host.declare_root(None, "files", &hooks.root),
            vosu_status::VOSU_OK
        );
        Ok(Self {
            host,
            events: hooks.events,
            next_correlation: 0,
            pending: HashMap::new(),
            waiting: HashMap::new(),
            streams: HashMap::new(),
            next_stream: 0,
        })
    }

    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        self.events.try_send(message.clone()).unwrap();
        match message {
            Message::ActionInvoke(invoke) => {
                let args: Value = invoke.args.deserialized().unwrap();
                let target = args["invocation"].as_u64().unwrap_or(invoke.invocation);
                let specs = args["declare"].as_array().cloned().unwrap_or_default();
                if args["hold"] == true {
                    return Ok(());
                }
                if specs.is_empty() {
                    self.answer(invoke.invocation, &args, Vec::new());
                    return Ok(());
                }
                self.waiting.insert(
                    invoke.invocation,
                    Waiting {
                        remaining: specs.len(),
                        declared: vec![Value::Null; specs.len()],
                    },
                );
                for (index, spec) in specs.iter().enumerate() {
                    self.next_correlation += 1;
                    let correlation = self.next_correlation;
                    self.pending.insert(correlation, (invoke.invocation, index));
                    // Sent raw, so a name the SDK helper would refuse still
                    // reaches the host's own check.
                    let status = self.host.send(
                        &Message::ArtifactDeclare(ArtifactDeclare {
                            invocation: target,
                            key: CborValue::serialized(&spec["key"]).unwrap(),
                            name: spec["name"].as_str().unwrap().into(),
                            content_id: CborValue::Text(format!("content-{index}")),
                            size: spec["size"].as_u64().unwrap(),
                        }),
                        correlation,
                    );
                    assert_eq!(status, vosu_status::VOSU_OK);
                }
            }
            Message::ArtifactDeclared(reply) => {
                let (invocation, index) = self.pending.remove(&correlation).unwrap();
                let waiting = self.waiting.get_mut(&invocation).unwrap();
                waiting.declared[index] = match reply {
                    ArtifactDeclared::Artifact { artifact } => json!({"artifact": artifact}),
                    ArtifactDeclared::Error { error } => json!({"error": error.code}),
                };
                waiting.remaining -= 1;
                if waiting.remaining == 0 {
                    let waiting = self.waiting.remove(&invocation).unwrap();
                    self.answer(invocation, &Value::Null, waiting.declared);
                }
            }
            Message::ArtifactOpen(open) => {
                let key: Value = open.key.deserialized().unwrap();
                let size = key["size"].as_u64().unwrap_or(0);
                let reply = match key["source"].as_str() {
                    Some("file") => ResourceResolved::Path {
                        root: "files".into(),
                        path: key["path"].as_str().unwrap().into(),
                    },
                    Some("stream" | "short") => {
                        self.next_stream += 1;
                        let end = if key["source"] == "short" {
                            size / 2
                        } else {
                            size
                        };
                        self.streams.insert(
                            self.next_stream,
                            Stream {
                                end,
                                hold: key["hold"] == true,
                            },
                        );
                        ResourceResolved::Stream {
                            stream: self.next_stream,
                            size: key["advertise"].as_u64().unwrap_or(size),
                        }
                    }
                    _ => ResourceResolved::Error {
                        error: ErrorInfo {
                            code: "not_found".into(),
                            message: "no such artifact source".into(),
                        },
                    },
                };
                let status = self
                    .host
                    .send(&Message::ResourceResolved(reply), correlation);
                assert_eq!(status, vosu_status::VOSU_OK);
            }
            Message::StreamRead(read) => {
                let Some(stream) = self.streams.get(&read.stream) else {
                    return Err(vosu_status::VOSU_ERR_CANCELLED.into());
                };
                if stream.hold && read.offset > 0 {
                    return Ok(());
                }
                assert!(read.len <= VOSU_STREAM_CHUNK_MAX_BYTES);
                let end = read
                    .offset
                    .saturating_add(u64::from(read.len))
                    .min(stream.end);
                let data = (read.offset.min(end)..end).map(generated).collect();
                // A read core already abandoned is refused; that is fine.
                let _ = self.host.send(
                    &Message::StreamChunk(StreamChunk::Data {
                        data: CborBytes(data),
                    }),
                    correlation,
                );
            }
            Message::StreamClose(close) => {
                self.streams.remove(&close.stream);
            }
            _ => {}
        }
        Ok(())
    }
}

impl ArtifactPlugin {
    fn answer(&self, invocation: u64, args: &Value, declared: Vec<Value>) {
        let value = json!({
            "invocation": invocation,
            "declared": declared,
            "echo": args.get("echo").cloned().unwrap_or(Value::Null),
        });
        let status = self.host.send(
            &Message::ActionResult(ActionResult {
                invocation,
                status: ActionStatus::Ok,
                value: Some(CborValue::serialized(&value).unwrap()),
                code: None,
                message: None,
            }),
            0,
        );
        assert_eq!(status, vosu_status::VOSU_OK);
    }
}

vosu_plugin_internal::export_plugin!(bundled ArtifactPlugin);

/// Loads the provider as instance `instance`, with a `make` action of
/// session mode `none`, its plugin-wide root at `root`, and `extra`
/// manifest text appended.
pub fn load(
    h: &Harness,
    instance: u64,
    id: &str,
    root: PathBuf,
    extra: &str,
) -> mpsc::Receiver<Message> {
    let (events, receiver) = mpsc::sync_channel(4096);
    assert!(
        HOOKS
            .lock()
            .unwrap()
            .insert(id.into(), Hooks { events, root })
            .is_none()
    );
    let text = format!(
        "{}root_slots=['files']\n[[actions]]\nname='make'\nversion='1.0.0'\ntitle='B0'\nargs_schema='args.json'\nresult_schema='result.json'\nsession='none'\ncli=true\n[[provides]]\nid='action:{id}.make'\nversion='1.0.0'\n{extra}",
        manifest(id)
    );
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
    receiver
}

/// The next message accepted by `predicate`, within the harness bound.
pub fn next(rx: &mpsc::Receiver<Message>, predicate: impl Fn(&Message) -> bool) -> Message {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let message = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("expected fixture message");
        if predicate(&message) {
            return message;
        }
    }
}

/// Every message already delivered.
pub fn drain(rx: &mpsc::Receiver<Message>) -> Vec<Message> {
    rx.try_iter().collect()
}

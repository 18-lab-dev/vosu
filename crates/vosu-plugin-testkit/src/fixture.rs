//! Debug daemon fixture loaded through the normal `--plugin` development path.
//! Copy `tests/fixtures/api-v1-plugin` to a temporary directory, copy the named
//! cdylib example to its `lib` directory, and start `api_v1_target`. The root is
//! `Init.data_dir` (prepare `fixture.osu` there before starting the daemon).
//! `run` echoes arguments, `fail` fails. `wait` withholds the result until
//! cancellation; `wait_progress` also publishes nonzero progress first, while
//! `ignore` ignores cancellation. `artifact` declares one sessionless
//! generated-stream artifact (B0, AF.12) of [`ARTIFACT_SIZE`] bytes, then
//! echoes its arguments once core accepted it. State (leases and invocations)
//! is per instance.
use crate::actions::{ActionFixture, ActionScript, ActionStep};
use crate::resources::CAPACITY;
use std::collections::{HashMap, HashSet};
use vosu_plugin_api::{
    ActionResult, ActionStatus, ArtifactDeclare, ArtifactDeclared, CborBytes, CborValue, ErrorInfo,
    HostServices, Init, Message, Plugin, PluginError, PublishBatchWriter, PublishValueRef,
    ResourceRef, ResourceResolved, RootDeclare, SessionIdentify, StreamChunk, TreeEntry,
    VOSU_STREAM_CHUNK_MAX_BYTES, VtableHostServices, encode, vosu_opcode, vosu_status,
};

pub const STREAM_BYTES: &[u8] = b"fixture-generated-stream-7913\n";
/// The `artifact` mode's size: several stream chunks, never held whole.
pub const ARTIFACT_SIZE: u64 = 2_621_447;
pub const ARTIFACT_NAME: &str = "fixture-artifact-7913.bin";

/// The `artifact` mode's byte at `offset`.
pub fn artifact_byte(offset: u64) -> u8 {
    ((offset * 31 + 7) % 251) as u8
}

pub struct ApiFixturePlugin {
    host: VtableHostServices,
    id: String,
    actions: ActionFixture,
    // Keep bounded observation alive without a background consumer.
    events: std::sync::mpsc::Receiver<crate::actions::ActionEvent>,
    sessions: HashSet<u64>,
    streams: HashMap<u64, u64>,
    next_stream: u64,
    /// Generated artifact streams, which belong to no session.
    artifact_streams: HashSet<u64>,
    /// `ARTIFACT_DECLARE` correlation → (invocation, echoed arguments).
    declaring: HashMap<u64, (u64, CborValue)>,
    next_declare: u64,
    root: String,
}

impl ApiFixturePlugin {
    fn send(&self, message: Message, correlation: u64) -> Result<(), PluginError> {
        let status = self.host.send(&message, correlation);
        if status == vosu_status::VOSU_OK {
            Ok(())
        } else {
            Err(status.into())
        }
    }

    fn publish(&self, handle: u64) -> Result<(), PluginError> {
        let mut batch = PublishBatchWriter::new();
        batch.begin(handle);
        for (kind, pointer, name, tree) in [
            ("file", "/resources/file", "fixture.osu", false),
            ("stream", "/resources/stream", "generated.osu", false),
            ("tree", "/resources/tree", "fixture", true),
        ] {
            let reference = ResourceRef {
                kind: format!("fixture-{kind}"),
                key: CborValue::Text(kind.into()),
                name: name.into(),
                content_id: CborValue::Text(format!("fixture-7913-{kind}")),
            };
            let bytes = encode(&reference)
                .map_err(|_| PluginError(vosu_status::VOSU_ERR_INVALID_ARGUMENT))?;
            batch.push(
                &self.id,
                pointer,
                if tree {
                    PublishValueRef::TreeRef(&bytes)
                } else {
                    PublishValueRef::ResourceRef(&bytes)
                },
            );
        }
        let mut bytes = Vec::new();
        batch
            .finish_into(&mut bytes)
            .map_err(|_| PluginError(vosu_status::VOSU_ERR_INVALID_ARGUMENT))?;
        let status = self
            .host
            .send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes);
        if status == vosu_status::VOSU_OK {
            Ok(())
        } else {
            Err(status.into())
        }
    }

    fn next_stream(&mut self) -> Result<u64, PluginError> {
        if self.streams.len() + self.artifact_streams.len() == CAPACITY {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        self.next_stream = self
            .next_stream
            .checked_add(1)
            .ok_or(PluginError(vosu_status::VOSU_ERR_INVALID_ARGUMENT))?;
        Ok(self.next_stream)
    }

    fn stream(&mut self, handle: u64) -> Result<ResourceResolved, PluginError> {
        let stream = self.next_stream()?;
        self.streams.insert(stream, handle);
        Ok(ResourceResolved::Stream {
            stream,
            size: STREAM_BYTES.len() as u64,
        })
    }

    /// Declares the `artifact` mode's output; the action answers once the
    /// declaration's reply arrives.
    fn declare(&mut self, invocation: u64, args: CborValue) -> Result<(), PluginError> {
        if self.declaring.len() == CAPACITY {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        self.next_declare += 1;
        self.declaring.insert(self.next_declare, (invocation, args));
        self.send(
            Message::ArtifactDeclare(ArtifactDeclare {
                invocation,
                key: CborValue::Text("generated".into()),
                name: ARTIFACT_NAME.into(),
                content_id: CborValue::Text("fixture-artifact-7913".into()),
                size: ARTIFACT_SIZE,
            }),
            self.next_declare,
        )
    }
}

impl Plugin for ApiFixturePlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let (actions, events) = ActionFixture::new();
        let root = init
            .data_dir
            .to_str()
            .ok_or(PluginError(vosu_status::VOSU_ERR_INVALID_ARGUMENT))?
            .to_owned();
        Ok(Self {
            host,
            id: init.plugin_id,
            actions,
            events,
            sessions: HashSet::new(),
            streams: HashMap::new(),
            next_stream: 100,
            artifact_streams: HashSet::new(),
            declaring: HashMap::new(),
            next_declare: 0,
            root,
        })
    }

    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(session) => {
                self.sessions.insert(session.handle);
                if session.name.is_none() {
                    return Ok(()); // Guest attachments have no private process facts.
                }
                self.send(
                    Message::SessionIdentify(SessionIdentify {
                        handle: session.handle,
                        game: "fixture".into(),
                        variant: "synthetic".into(),
                        name: "API fixture 7913".into(),
                        version: "1.0.0".into(),
                        focus_priority: false,
                    }),
                    0,
                )?;
                self.send(
                    Message::RootDeclare(RootDeclare {
                        handle: Some(session.handle),
                        name: "fixture".into(),
                        path: self.root.clone(),
                    }),
                    0,
                )?;
                self.publish(session.handle)?;
            }
            Message::SessionDetached(session) => {
                self.sessions.remove(&session.handle);
                self.streams.retain(|_, handle| *handle != session.handle);
            }
            Message::ResourceResolve(request) => {
                let reply = if request.kind == "fixture-file"
                    && request.key == CborValue::Text("file".into())
                {
                    ResourceResolved::Path {
                        root: "fixture".into(),
                        path: "fixture.osu".into(),
                    }
                } else if request.kind == "fixture-stream"
                    && request.key == CborValue::Text("stream".into())
                {
                    self.stream(request.handle)?
                } else {
                    ResourceResolved::Error {
                        error: ErrorInfo {
                            code: "not_found".into(),
                            message: "fixture resource absent".into(),
                        },
                    }
                };
                self.send(Message::ResourceResolved(reply), correlation)?;
            }
            Message::TreeList(_) => self.send(
                Message::ResourceResolved(ResourceResolved::Tree {
                    entries: vec![TreeEntry {
                        path: "fixture.osu".into(),
                        size: include_bytes!("../../../tests/fixtures/api-v1-plugin/fixture.osu")
                            .len() as u64,
                        hash: None,
                    }],
                    truncated: false,
                    revision: "fixture-tree-7913".into(),
                }),
                correlation,
            )?,
            Message::TreeLookup(request) => {
                let reply = if request.path == "fixture.osu" || request.path == "alias.osu" {
                    ResourceResolved::TreePath {
                        root: "fixture".into(),
                        path: "fixture.osu".into(),
                        entry_path: "fixture.osu".into(),
                    }
                } else {
                    ResourceResolved::Error {
                        error: ErrorInfo {
                            code: "not_found".into(),
                            message: "fixture member absent".into(),
                        },
                    }
                };
                self.send(Message::ResourceResolved(reply), correlation)?;
            }
            Message::StreamRead(request) => {
                if request.len > VOSU_STREAM_CHUNK_MAX_BYTES {
                    return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
                }
                let data = if self.artifact_streams.contains(&request.stream) {
                    let end = request
                        .offset
                        .saturating_add(u64::from(request.len))
                        .min(ARTIFACT_SIZE);
                    (request.offset.min(end)..end).map(artifact_byte).collect()
                } else if self.streams.contains_key(&request.stream) {
                    let start = usize::try_from(request.offset)
                        .unwrap_or(usize::MAX)
                        .min(STREAM_BYTES.len());
                    let end = start
                        .saturating_add(request.len as usize)
                        .min(STREAM_BYTES.len());
                    STREAM_BYTES[start..end].to_vec()
                } else {
                    return Err(vosu_status::VOSU_ERR_CANCELLED.into());
                };
                self.send(
                    Message::StreamChunk(StreamChunk::Data {
                        data: CborBytes(data),
                    }),
                    correlation,
                )?;
            }
            Message::StreamClose(request) => {
                self.streams.remove(&request.stream);
                self.artifact_streams.remove(&request.stream);
            }
            Message::ArtifactDeclared(reply) => {
                let Some((invocation, args)) = self.declaring.remove(&correlation) else {
                    return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
                };
                let result = match reply {
                    ArtifactDeclared::Artifact { .. } => ActionResult {
                        invocation,
                        status: ActionStatus::Ok,
                        value: Some(args),
                        code: None,
                        message: None,
                    },
                    ArtifactDeclared::Error { error } => ActionResult {
                        invocation,
                        status: ActionStatus::Error,
                        value: None,
                        code: Some(error.code),
                        message: Some(error.message),
                    },
                };
                self.send(Message::ActionResult(result), 0)?;
            }
            Message::ArtifactOpen(open) => {
                let reply = if open.key == CborValue::Text("generated".into()) {
                    let stream = self.next_stream()?;
                    self.artifact_streams.insert(stream);
                    ResourceResolved::Stream {
                        stream,
                        size: ARTIFACT_SIZE,
                    }
                } else {
                    ResourceResolved::Error {
                        error: ErrorInfo {
                            code: "not_found".into(),
                            message: "fixture artifact absent".into(),
                        },
                    }
                };
                self.send(Message::ResourceResolved(reply), correlation)?;
            }
            Message::ActionInvoke(invoke) => {
                if invoke
                    .session
                    .is_some_and(|handle| !self.sessions.contains(&handle))
                {
                    return self.send(
                        Message::ActionResult(ActionResult {
                            invocation: invoke.invocation,
                            status: ActionStatus::Error,
                            value: None,
                            code: Some("unknown_session".into()),
                            message: None,
                        }),
                        0,
                    );
                }
                let mode = match &invoke.args {
                    CborValue::Map(entries) => entries.iter().find_map(|(k, v)| {
                        if k == &CborValue::Text("mode".into()) {
                            v.as_text()
                        } else {
                            None
                        }
                    }),
                    _ => None,
                };
                if mode == Some("artifact") && invoke.action == format!("{}.run", self.id) {
                    return self.declare(invoke.invocation, invoke.args);
                }
                let steps = if mode == Some("wait") || mode == Some("ignore") {
                    vec![]
                } else if mode == Some("wait_progress") {
                    vec![ActionStep::Progress {
                        fraction: 0.375,
                        message: "fixture progress 7913".into(),
                    }]
                } else if invoke.action == format!("{}.run", self.id) {
                    vec![
                        ActionStep::Progress {
                            fraction: 0.375,
                            message: "fixture progress 7913".into(),
                        },
                        ActionStep::Success(invoke.args.clone()),
                    ]
                } else if invoke.action == format!("{}.fail", self.id) {
                    vec![ActionStep::Failure {
                        code: "fixture_failed".into(),
                        message: "fixture failure 8927".into(),
                    }]
                } else {
                    return self.send(
                        Message::ActionResult(ActionResult {
                            invocation: invoke.invocation,
                            status: ActionStatus::Error,
                            value: None,
                            code: Some("not_found".into()),
                            message: None,
                        }),
                        0,
                    );
                };
                if invoke.action != format!("{}.run", self.id)
                    && invoke.action != format!("{}.fail", self.id)
                {
                    return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
                }
                self.actions.script(
                    invoke.action.clone(),
                    ActionScript {
                        steps: steps.into(),
                        ignores_cancel: mode == Some("ignore"),
                    },
                )?;
                let invocation = invoke.invocation;
                self.actions.invoke(invoke)?;
                let _ = self.events.try_recv();
                while let Some(reply) = self.actions.release(invocation) {
                    self.send(reply, 0)?;
                }
            }
            Message::ActionCancel(cancel) => {
                let reply = self.actions.cancel(cancel.invocation)?;
                let _ = self.events.try_recv();
                if let Some(reply) = reply {
                    self.send(reply, 0)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

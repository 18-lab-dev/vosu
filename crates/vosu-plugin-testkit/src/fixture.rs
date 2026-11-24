//! Debug daemon fixture loaded through the normal `--plugin` development path.
//! Copy `tests/fixtures/api-v1-plugin` to a temporary directory, copy the named
//! cdylib example to its `lib` directory, and start `api_v1_target`. The root is
//! `Init.data_dir` (prepare `fixture.osu` there before starting the daemon).
//! `run` echoes arguments, `fail` fails. A `mode` argument of `wait` withholds
//! the result until cancellation; `ignore` also ignores cancellation. All state
//! is per instance, including the generated stream lease and invocation ids.
use crate::actions::{ActionFixture, ActionScript, ActionStep};
use crate::resources::CAPACITY;
use std::collections::HashMap;
use vosu_plugin_api::{
    ActionResult, ActionStatus, CborBytes, CborValue, ErrorInfo, HostServices, Init, Message,
    Plugin, PluginError, PublishBatchWriter, PublishValueRef, ResourceRef, ResourceResolved,
    RootDeclare, SessionIdentify, StreamChunk, TreeEntry, VOSU_STREAM_CHUNK_MAX_BYTES,
    VtableHostServices, encode, vosu_opcode, vosu_status,
};

pub const STREAM_BYTES: &[u8] = b"fixture-generated-stream-7913\n";
pub struct ApiFixturePlugin {
    host: VtableHostServices,
    id: String,
    actions: ActionFixture,
    // Keep bounded observation alive without a background consumer.
    events: std::sync::mpsc::Receiver<crate::actions::ActionEvent>,
    streams: HashMap<u64, u64>,
    next_stream: u64,
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

    fn stream(&mut self, handle: u64) -> Result<ResourceResolved, PluginError> {
        if self.streams.len() == CAPACITY {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        self.next_stream = self
            .next_stream
            .checked_add(1)
            .ok_or(PluginError(vosu_status::VOSU_ERR_INVALID_ARGUMENT))?;
        self.streams.insert(self.next_stream, handle);
        Ok(ResourceResolved::Stream {
            stream: self.next_stream,
            size: STREAM_BYTES.len() as u64,
        })
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
            streams: HashMap::new(),
            next_stream: 100,
            root,
        })
    }

    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(session) => {
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
                self.streams.retain(|_, handle| *handle != session.handle);
                // All fixture invocations are session-less; core owns detached
                // session invocation cancellation for session-bound test scripts.
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
                if !self.streams.contains_key(&request.stream) {
                    return Err(vosu_status::VOSU_ERR_CANCELLED.into());
                }
                if request.len > VOSU_STREAM_CHUNK_MAX_BYTES {
                    return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
                }
                let start = usize::try_from(request.offset)
                    .unwrap_or(usize::MAX)
                    .min(STREAM_BYTES.len());
                let end = start
                    .saturating_add(request.len as usize)
                    .min(STREAM_BYTES.len());
                self.send(
                    Message::StreamChunk(StreamChunk::Data {
                        data: CborBytes(STREAM_BYTES[start..end].to_vec()),
                    }),
                    correlation,
                )?;
            }
            Message::StreamClose(request) => {
                self.streams.remove(&request.stream);
            }
            Message::ActionInvoke(invoke) => {
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
                let steps = if mode == Some("wait") || mode == Some("ignore") {
                    vec![]
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

//! M5 I0's host-services mode (`mode=services` in the control file): the
//! process plugin uses core's ordinary services — publication, demand,
//! settings, lifecycle, `STATE_SUBSCRIBE`, declared roots, resources and
//! streams, and actions in both directions — so `tests/plugin_host_native`
//! can drive each one through a real child and observe core's side
//! through the API, and the child's side through receipts.
//!
//! The test drives the plugin with its `drive` action (`Drive` below,
//! answered once the requested operation finished); every message the
//! plugin receives writes one receipt. Receipt values never contain
//! whitespace: text is percent-escaped and CBOR values are rendered by
//! [`show`].
//!
//! **Publication** (owner role): on attach, the plugin identifies the
//! session, declares `root` as its `files` root and publishes `/marker`,
//! `/count` and `/resources/{file,escape,tree,stream,short}`.
//!
//! **Resources.** `doc` references name a path relative to the `files`
//! root (the `escape` one points outside it); `docs` is a two-entry tree
//! (`alpha.osu`, `beta.osu`); `generated` resolves to a fresh stream lease
//! of `stream` bytes ([`generated`]), whose `short` variant ends after
//! half of its declared size.
//!
//! **Actions.** `work` is a callee: it answers with
//! `{n, invocation, caller}` after `progress` progress messages, or holds
//! the invocation (`hold`) until the test's `answer` drive or a cancel.
//! Relayed `ACTION_PROGRESS`/`ACTION_RESULT` for the plugin's own
//! `ACTION_CALL`s write `relay_progress`/`relay_result` receipts; a `call`
//! drive finishes once every call it issued has its relayed result.

use std::collections::HashMap;
use std::path::PathBuf;

use vosu_plugin_api::{
    ActionCancel, ActionInvoke, ActionProgress, ActionResult, ActionStatus, CborBytes, CborValue,
    ErrorInfo, HostServices, Init, Message, PluginError, PublishBatch, PublishField, PublishValue,
    ResourceRef, ResourceResolved, Selector, SessionDecline, StreamChunk, TreeEntry,
    VOSU_STREAM_CHUNK_MAX_BYTES, VtableHostServices, vosu_status,
};

use crate::control::Control;

/// The tree's members, relative to the `files` root.
const MEMBERS: [&str; 2] = ["alpha.osu", "beta.osu"];

/// The generated stream's byte at `offset`; `tests/plugin_host_native`
/// computes the same function.
pub fn generated(offset: u64) -> u8 {
    ((offset % 251) as u8) ^ ((offset >> 12) as u8)
}

/// One live stream lease.
struct Lease {
    /// Where the bytes end: the declared size, or half of it for `short`.
    end: u64,
}

/// A held `work` invocation.
struct Held {
    invocation: u64,
    n: i128,
    caller: String,
}

pub struct Services {
    control: &'static Control,
    plugin_id: String,
    owned: Vec<u64>,
    root: Option<PathBuf>,
    revision: u64,
    streams: HashMap<u64, Lease>,
    next_stream: u64,
    next_call: u64,
    /// This plugin's own call id → the `drive` invocation that issued it.
    calls: HashMap<u64, u64>,
    /// A `call` drive → its calls still waiting for a relayed result.
    drives: HashMap<u64, usize>,
    held: Vec<Held>,
}

impl Services {
    pub fn new(control: &'static Control, init: &Init) -> Self {
        control.receipt(
            "init",
            &format!(
                "plugin={} settings={}",
                init.plugin_id,
                show(&init.settings)
            ),
        );
        Self {
            control,
            plugin_id: init.plugin_id.clone(),
            owned: Vec::new(),
            root: control.root.clone(),
            revision: 0,
            streams: HashMap::new(),
            next_stream: 0,
            next_call: 0,
            calls: HashMap::new(),
            drives: HashMap::new(),
            held: Vec::new(),
        }
    }

    pub fn on_message(
        &mut self,
        host: &VtableHostServices,
        message: Message,
        correlation: u64,
    ) -> Result<(), PluginError> {
        let control = self.control;
        match message {
            Message::SessionAttached(attached) => {
                if control.observer {
                    control.receipt(
                        "attach",
                        &format!(
                            "handle={} session={} target={} observed=1",
                            attached.handle, attached.session, attached.pid
                        ),
                    );
                    return Ok(());
                }
                let handle = attached.handle;
                let identify = host.identify_session(
                    handle,
                    "fixture",
                    "process",
                    "Services fixture",
                    "1.0.0",
                    false,
                );
                let declare = self.root.as_deref().map_or(vosu_status::VOSU_OK, |root| {
                    host.declare_root(Some(handle), "files", root)
                });
                self.owned.push(handle);
                let mut entries = vec![
                    field(handle, "/marker", PublishValue::String("attached".into())),
                    field(handle, "/count", PublishValue::U64(0)),
                ];
                entries.extend(self.references(handle));
                let publish = host.send(&self.batch(handle, entries), 0);
                control.receipt(
                    "attach",
                    &format!(
                        "handle={handle} session={} target={} identify={} declare={} publish={}",
                        attached.session,
                        attached.pid,
                        identify as u32,
                        declare as u32,
                        publish as u32
                    ),
                );
            }
            Message::SessionDetached(detached) => {
                self.owned.retain(|handle| *handle != detached.handle);
                control.receipt(
                    "detach",
                    &format!("handle={} reason={:?}", detached.handle, detached.reason),
                );
            }
            Message::DemandChanged(demand) => {
                let selectors: Vec<String> = demand
                    .selectors
                    .iter()
                    .map(|selector| format!("{}|{}", selector.ns, escape(&selector.pointer)))
                    .collect();
                control.receipt(
                    "demand",
                    &format!(
                        "handle={} regular={} precise={} selectors={}",
                        demand.handle,
                        demand.regular,
                        demand.precise,
                        if selectors.is_empty() {
                            "-".to_owned()
                        } else {
                            selectors.join(",")
                        }
                    ),
                );
            }
            Message::SettingsChanged(settings) => {
                control.receipt("settings", &format!("values={}", show(&settings.values)));
            }
            Message::StateUpdate(update) => {
                let entries: Vec<String> = update
                    .values
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}|{}|{}",
                            entry.ns,
                            escape(&entry.pointer),
                            entry
                                .value
                                .as_ref()
                                .map_or_else(|| "absent".to_owned(), show)
                        )
                    })
                    .collect();
                control.receipt(
                    "update",
                    &format!(
                        "session={} more={} entries={}",
                        update.session,
                        update.more,
                        entries.join(";")
                    ),
                );
            }
            Message::ResourceResolve(request) => {
                let reply = match (request.kind.as_str(), &request.key) {
                    ("doc", CborValue::Text(path)) => ResourceResolved::Path {
                        root: "files".into(),
                        path: path.clone(),
                    },
                    ("generated", CborValue::Text(variant)) => {
                        self.next_stream += 1;
                        let size = control.stream;
                        let end = if variant == "short" { size / 2 } else { size };
                        self.streams.insert(self.next_stream, Lease { end });
                        ResourceResolved::Stream {
                            stream: self.next_stream,
                            size,
                        }
                    }
                    _ => not_found(),
                };
                control.receipt(
                    "resolve",
                    &format!(
                        "handle={} kind={} key={} stream={}",
                        request.handle,
                        request.kind,
                        show(&request.key),
                        match &reply {
                            ResourceResolved::Stream { stream, .. } => *stream,
                            _ => 0,
                        }
                    ),
                );
                reply_to(host, reply, correlation)?;
            }
            Message::TreeList(request) => {
                control.receipt("list", &format!("handle={}", request.handle));
                let reply = match &self.root {
                    Some(root) => ResourceResolved::Tree {
                        entries: MEMBERS
                            .iter()
                            .map(|member| TreeEntry {
                                path: (*member).to_owned(),
                                size: std::fs::metadata(root.join(member))
                                    .map_or(0, |metadata| metadata.len()),
                                hash: None,
                            })
                            .collect(),
                        truncated: false,
                        revision: format!("revision-{}", self.revision),
                    },
                    None => not_found(),
                };
                reply_to(host, reply, correlation)?;
            }
            Message::TreeLookup(request) => {
                control.receipt(
                    "lookup",
                    &format!("handle={} path={}", request.handle, escape(&request.path)),
                );
                let reply = if MEMBERS.contains(&request.path.as_str()) {
                    ResourceResolved::TreePath {
                        root: "files".into(),
                        path: request.path.clone(),
                        entry_path: request.path,
                    }
                } else {
                    not_found()
                };
                reply_to(host, reply, correlation)?;
            }
            Message::StreamRead(read) => {
                control.receipt(
                    "read",
                    &format!(
                        "stream={} offset={} len={}",
                        read.stream, read.offset, read.len
                    ),
                );
                let Some(lease) = self.streams.get(&read.stream) else {
                    return Err(vosu_status::VOSU_ERR_CANCELLED.into());
                };
                if read.len > VOSU_STREAM_CHUNK_MAX_BYTES {
                    return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
                }
                let end = read
                    .offset
                    .saturating_add(u64::from(read.len))
                    .min(lease.end);
                let data = (read.offset.min(end)..end).map(generated).collect();
                let status = host.send(
                    &Message::StreamChunk(StreamChunk::Data {
                        data: CborBytes(data),
                    }),
                    correlation,
                );
                if status != vosu_status::VOSU_OK {
                    control.receipt(
                        "chunk_refused",
                        &format!("stream={} status={}", read.stream, status as u32),
                    );
                }
            }
            Message::StreamClose(close) => {
                let known = self.streams.remove(&close.stream).is_some();
                control.receipt("close", &format!("stream={} known={known}", close.stream));
            }
            Message::ActionInvoke(invoke) => {
                if invoke.action == format!("{}.drive", self.plugin_id) {
                    self.drive(host, invoke);
                } else if invoke.action == format!("{}.work", self.plugin_id) {
                    self.work(host, invoke);
                } else {
                    finish(
                        host,
                        invoke.invocation,
                        ActionStatus::Ok,
                        Some(invoke.args),
                        None,
                    );
                }
            }
            Message::ActionCancel(cancel) => {
                let at = self
                    .held
                    .iter()
                    .position(|held| held.invocation == cancel.invocation);
                control.receipt(
                    "cancel",
                    &format!("invocation={} held={}", cancel.invocation, at.is_some()),
                );
                if let Some(at) = at {
                    self.held.remove(at);
                    finish(
                        host,
                        cancel.invocation,
                        ActionStatus::Error,
                        None,
                        Some("cancelled"),
                    );
                }
            }
            Message::ActionProgress(progress) => {
                control.receipt(
                    "relay_progress",
                    &format!(
                        "invocation={} fraction={}",
                        progress.invocation,
                        progress
                            .fraction
                            .map_or_else(|| "none".to_owned(), |fraction| fraction.to_string())
                    ),
                );
            }
            Message::ActionResult(result) => {
                control.receipt(
                    "relay_result",
                    &format!(
                        "invocation={} status={:?} code={} value={}",
                        result.invocation,
                        result.status,
                        result.code.as_deref().unwrap_or("none"),
                        result
                            .value
                            .as_ref()
                            .map_or_else(|| "none".to_owned(), show)
                    ),
                );
                if let Some(drive) = self.calls.remove(&result.invocation) {
                    self.settle(host, drive);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The resource references of session `handle` at this revision.
    fn references(&self, handle: u64) -> Vec<PublishField> {
        let reference = |kind: &str, key: &str, name: &str| ResourceRef {
            kind: kind.into(),
            key: CborValue::Text(key.into()),
            name: name.into(),
            content_id: CborValue::Text(format!("{key}@{}", self.revision)),
        };
        vec![
            field(
                handle,
                "/resources/file",
                PublishValue::ResourceRef(reference("doc", "alpha.osu", "alpha.osu")),
            ),
            field(
                handle,
                "/resources/escape",
                PublishValue::ResourceRef(reference("doc", "../outside.osu", "outside.osu")),
            ),
            field(
                handle,
                "/resources/tree",
                PublishValue::TreeRef(reference("docs", "docs", "docs")),
            ),
            field(
                handle,
                "/resources/stream",
                PublishValue::ResourceRef(reference("generated", "full", "generated.osu")),
            ),
            field(
                handle,
                "/resources/short",
                PublishValue::ResourceRef(reference("generated", "short", "short.osu")),
            ),
        ]
    }

    /// One `PUBLISH_BATCH` of `entries` into this plugin's own namespace.
    fn batch(&self, handle: u64, entries: Vec<PublishField>) -> Message {
        Message::PublishBatch(PublishBatch {
            handle,
            entries: entries
                .into_iter()
                .map(|entry| PublishField {
                    ns: self.plugin_id.clone(),
                    ..entry
                })
                .collect(),
        })
    }

    /// Publishes `entries` for every owned session; the last status.
    fn publish_all(
        &self,
        host: &VtableHostServices,
        entries: impl Fn(u64) -> Vec<PublishField>,
    ) -> vosu_status {
        let mut status = vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        for &handle in &self.owned {
            status = host.send(&self.batch(handle, entries(handle)), 0);
        }
        status
    }

    /// The `drive` action: one test-requested operation.
    fn drive(&mut self, host: &VtableHostServices, invoke: ActionInvoke) {
        let args = &invoke.args;
        let op = text(args, "op").unwrap_or_default();
        self.control.receipt(
            "drive",
            &format!("invocation={} op={op}", invoke.invocation),
        );
        let status = match op.as_str() {
            "publish" => {
                let marker = text(args, "text").unwrap_or_default();
                let count = integer(args, "count").unwrap_or(0) as u64;
                self.publish_all(host, |handle| {
                    vec![
                        field(handle, "/marker", PublishValue::String(marker.clone())),
                        field(handle, "/count", PublishValue::U64(count)),
                    ]
                })
            }
            // A batch whose last entry descends through the `/marker`
            // string: core must refuse the whole batch and commit nothing.
            "publish_invalid" => {
                let marker = text(args, "text").unwrap_or_default();
                self.publish_all(host, |handle| {
                    vec![
                        field(handle, "/marker", PublishValue::String(marker.clone())),
                        field(handle, "/count", PublishValue::U64(999)),
                        field(handle, "/marker/below", PublishValue::U64(1)),
                    ]
                })
            }
            "refs" => {
                self.revision += 1;
                self.publish_all(host, |handle| self.references(handle))
            }
            "declare" => {
                let path = PathBuf::from(text(args, "path").unwrap_or_default());
                let mut status = vosu_status::VOSU_ERR_INVALID_ARGUMENT;
                for &handle in &self.owned {
                    status = host.declare_root(Some(handle), "files", &path);
                }
                self.root = Some(path);
                status
            }
            "sessions" => host.subscribe_state(
                None,
                vec![Selector {
                    ns: "vosu.sessions".into(),
                    pointer: String::new(),
                }],
            ),
            "subscribe" => {
                let session = integer(args, "session").unwrap_or(0) as u64;
                let selectors = list(args, "selectors")
                    .into_iter()
                    .filter_map(|selector| {
                        let (ns, pointer) = selector.split_once('|')?;
                        Some(Selector {
                            ns: ns.into(),
                            pointer: pointer.into(),
                        })
                    })
                    .collect();
                host.subscribe_state(Some(session), selectors)
            }
            "call" | "call_block" => {
                self.call(host, &invoke);
                if op == "call_block" {
                    self.block(host);
                }
                return;
            }
            "cancel" => {
                let invocation = integer(args, "invocation").unwrap_or(0) as u64;
                host.send(&Message::ActionCancel(ActionCancel { invocation }), 0)
            }
            "answer" => {
                let progress = integer(args, "progress").unwrap_or(0) as u64;
                for held in std::mem::take(&mut self.held) {
                    self.answer(host, &held, progress, false);
                }
                vosu_status::VOSU_OK
            }
            _ => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        };
        self.control.receipt(
            "driven",
            &format!(
                "invocation={} op={op} status={}",
                invoke.invocation, status as u32
            ),
        );
        finish(
            host,
            invoke.invocation,
            ActionStatus::Ok,
            Some(status_value(status)),
            None,
        );
    }

    /// Issues `count` `ACTION_CALL`s of `action` with `args`, whose `n`
    /// (when present) grows by one per call. The drive finishes once every
    /// accepted call has its relayed result.
    fn call(&mut self, host: &VtableHostServices, invoke: &ActionInvoke) {
        let action = text(&invoke.args, "action").unwrap_or_default();
        let count = integer(&invoke.args, "count").unwrap_or(0);
        let template = value(&invoke.args, "args").unwrap_or(CborValue::Map(Vec::new()));
        let mut accepted = 0;
        for index in 0..count {
            self.next_call += 1;
            let id = self.next_call;
            let args = with_n_offset(&template, index);
            let status = host.call_action(id, &action, None, args);
            self.control.receipt(
                "call",
                &format!("call={id} index={index} status={}", status as u32),
            );
            if status == vosu_status::VOSU_OK {
                self.calls.insert(id, invoke.invocation);
                accepted += 1;
            }
        }
        if accepted == 0 {
            finish(
                host,
                invoke.invocation,
                ActionStatus::Ok,
                Some(status_value(vosu_status::VOSU_OK)),
                None,
            );
        } else {
            self.drives.insert(invoke.invocation, accepted);
        }
    }

    /// One relayed result for `drive`'s calls arrived.
    fn settle(&mut self, host: &VtableHostServices, drive: u64) {
        let Some(remaining) = self.drives.get_mut(&drive) else {
            return;
        };
        *remaining -= 1;
        if *remaining == 0 {
            self.drives.remove(&drive);
            finish(
                host,
                drive,
                ActionStatus::Ok,
                Some(status_value(vosu_status::VOSU_OK)),
                None,
            );
        }
    }

    /// Blocks this dispatch: `blocked`, then the `probe` barrier answered by
    /// an acknowledged send (its `STATUS` is behind every frame core wrote
    /// before it), then the `release` barrier.
    fn block(&self, host: &VtableHostServices) {
        self.control.receipt("blocked", "");
        if self.control.wait_barrier("probe") {
            let status = host.send(
                &Message::SessionDecline(SessionDecline {
                    handle: 0x5E55_DEC1,
                }),
                0,
            );
            self.control
                .receipt("probed", &format!("status={}", status as u32));
        }
        let released = self.control.wait_barrier("release");
        self.control
            .receipt("unblocked", &format!("released={released}"));
    }

    /// The `work` callee action.
    fn work(&mut self, host: &VtableHostServices, invoke: ActionInvoke) {
        let args = &invoke.args;
        let n = integer(args, "n").unwrap_or(-1);
        let held = Held {
            invocation: invoke.invocation,
            n,
            caller: invoke.caller.id.clone(),
        };
        self.control.receipt(
            "work",
            &format!(
                "invocation={} caller_kind={:?} caller={} n={n}",
                invoke.invocation, invoke.caller.kind, invoke.caller.id
            ),
        );
        if boolean(args, "hold") {
            self.held.push(held);
            return;
        }
        let progress = integer(args, "progress").unwrap_or(0) as u64;
        self.answer(host, &held, progress, boolean(args, "bad_result"));
    }

    /// `progress` progress messages, then the result (contract-invalid
    /// when `bad`).
    fn answer(&self, host: &VtableHostServices, held: &Held, progress: u64, bad: bool) {
        let mut refused = 0;
        for step in 1..=progress {
            let status = host.send(
                &Message::ActionProgress(ActionProgress {
                    invocation: held.invocation,
                    fraction: Some(step as f64 / progress as f64),
                    message: None,
                }),
                0,
            );
            if status != vosu_status::VOSU_OK {
                refused += 1;
            }
        }
        let value = if bad {
            map(&[("wrong", CborValue::Bool(true))])
        } else {
            map(&[
                (
                    "n",
                    CborValue::Integer(held.n.try_into().unwrap_or(0.into())),
                ),
                ("invocation", CborValue::Integer(held.invocation.into())),
                ("caller", CborValue::Text(held.caller.clone())),
            ])
        };
        let status = finish(host, held.invocation, ActionStatus::Ok, Some(value), None);
        self.control.receipt(
            "worked",
            &format!(
                "invocation={} progress={progress} refused={refused} result={}",
                held.invocation, status as u32
            ),
        );
    }
}

fn field(handle: u64, pointer: &str, value: PublishValue) -> PublishField {
    PublishField {
        handle,
        ns: String::new(),
        pointer: pointer.into(),
        value,
    }
}

fn not_found() -> ResourceResolved {
    ResourceResolved::Error {
        error: ErrorInfo {
            code: "not_found".into(),
            message: "services fixture resource absent".into(),
        },
    }
}

fn reply_to(
    host: &VtableHostServices,
    reply: ResourceResolved,
    correlation: u64,
) -> Result<(), PluginError> {
    match host.send(&Message::ResourceResolved(reply), correlation) {
        vosu_status::VOSU_OK => Ok(()),
        status => Err(status.into()),
    }
}

fn finish(
    host: &VtableHostServices,
    invocation: u64,
    status: ActionStatus,
    value: Option<CborValue>,
    code: Option<&str>,
) -> vosu_status {
    host.send(
        &Message::ActionResult(ActionResult {
            invocation,
            status,
            value,
            code: code.map(str::to_owned),
            message: None,
        }),
        0,
    )
}

fn status_value(status: vosu_status) -> CborValue {
    map(&[("status", CborValue::Integer((status as u32).into()))])
}

fn map(entries: &[(&str, CborValue)]) -> CborValue {
    CborValue::Map(
        entries
            .iter()
            .map(|(key, value)| (CborValue::Text((*key).into()), value.clone()))
            .collect(),
    )
}

fn value(args: &CborValue, key: &str) -> Option<CborValue> {
    let CborValue::Map(entries) = args else {
        return None;
    };
    entries
        .iter()
        .find(|(name, _)| name.as_text() == Some(key))
        .map(|(_, value)| value.clone())
}

fn text(args: &CborValue, key: &str) -> Option<String> {
    value(args, key).and_then(|value| value.as_text().map(str::to_owned))
}

fn integer(args: &CborValue, key: &str) -> Option<i128> {
    value(args, key).and_then(|value| value.as_integer().map(i128::from))
}

fn boolean(args: &CborValue, key: &str) -> bool {
    value(args, key).and_then(|value| value.as_bool()) == Some(true)
}

fn list(args: &CborValue, key: &str) -> Vec<String> {
    match value(args, key) {
        Some(CborValue::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_text().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// `template` with its integer `n` grown by `offset`.
fn with_n_offset(template: &CborValue, offset: i128) -> CborValue {
    let CborValue::Map(entries) = template else {
        return template.clone();
    };
    CborValue::Map(
        entries
            .iter()
            .map(|(key, value)| match (key.as_text(), value.as_integer()) {
                (Some("n"), Some(n)) => (
                    key.clone(),
                    CborValue::Integer((i128::from(n) + offset).try_into().unwrap_or(0.into())),
                ),
                _ => (key.clone(), value.clone()),
            })
            .collect(),
    )
}

/// Percent-escapes the characters receipts and [`show`] use as syntax.
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '%' | ',' | '|' | ';' | ':' | '{' | '}' | '[' | ']' => {
                escaped.push_str(&format!("%{:02X}", character as u32));
            }
            character if character.is_whitespace() || character.is_control() => {
                escaped.push_str(&format!("%{:02X}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

/// A whitespace-free rendering of `value`: maps as `{key:value,...}`,
/// arrays as `[...]`, text escaped, bytes as hex.
pub fn show(value: &CborValue) -> String {
    match value {
        CborValue::Integer(integer) => i128::from(*integer).to_string(),
        CborValue::Text(text) => escape(text),
        CborValue::Bool(value) => value.to_string(),
        CborValue::Null => "null".to_owned(),
        CborValue::Float(value) => value.to_string(),
        CborValue::Bytes(bytes) => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        CborValue::Array(items) => {
            format!("[{}]", items.iter().map(show).collect::<Vec<_>>().join(","))
        }
        CborValue::Map(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(key, value)| format!("{}:{}", show(key), show(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => "unsupported".to_owned(),
    }
}

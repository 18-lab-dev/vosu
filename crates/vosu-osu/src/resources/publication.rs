//! A bounded, session-owned cold worker. No filesystem work runs in publication
//! or on Tokio workers; callbacks run without the mailbox lock.
use super::{PreparedResources, RootPaths, Selection, TreeSource};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak, atomic::AtomicU64, mpsc};
use std::thread::JoinHandle;
use vosu_osu_model::enums::ClientType;
use vosu_osu_state::{ColdResourceFacts, StateBundleSnapshot};
use vosu_plugin_api::{
    CborValue, ErrorInfo, HostServices, LogLevel, Message, PublishBatchWriter, PublishValueRef,
    ResourceResolved, StreamChunk, vosu_status,
};

pub(crate) trait RefreshPublication: Send + Sync {
    fn refresh(&self);
}

type Encoded = Vec<(String, Vec<u8>)>;
#[derive(Default)]
struct State {
    facts: Option<Arc<ColdResourceFacts>>,
    stopped: bool,
    epoch: u64,
    revision: u64,
    references: Arc<Encoded>,
    streams: HashSet<u64>,
    closed: Vec<u64>,
}
enum Command {
    Wake,
    Request(Message, u64),
}
pub(crate) struct Publication {
    state: Arc<Mutex<State>>,
    sender: Mutex<Option<mpsc::SyncSender<Command>>>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Publication {
    pub fn start(
        bus: Weak<dyn RefreshPublication>,
        host: Arc<dyn HostServices + Send + Sync>,
        handle: u64,
        ids: Arc<AtomicU64>,
    ) -> Arc<Self> {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let (sender, receiver) = mpsc::sync_channel(64);
        let worker = std::thread::Builder::new().name("osu-resources".into()).spawn(move || {
            let mut prepared: Option<PreparedResources> = None;
            let mut applied = 0;
            while let Ok(command) = receiver.recv() {
                if shared.lock().unwrap_or_else(|e| e.into_inner()).stopped { break; }
                while let Some(stream) = shared.lock().unwrap_or_else(|e| e.into_inner()).closed.pop() {
                    if let Some(prepared) = prepared.as_mut() { prepared.close(stream); }
                }
                if let Command::Request(message, correlation) = command {
                    let reply = if let Some(p) = prepared.as_mut() { reply(p, message) } else { Some(Message::ResourceResolved(missing())) };
                    if let Some(reply) = reply {
                        let gone = matches!(&reply, Message::ResourceResolved(ResourceResolved::Error { error }) | Message::StreamChunk(StreamChunk::Error { error }) if error.code == "gone");
                        let stream = match &reply { Message::ResourceResolved(ResourceResolved::Stream { stream, .. }) => Some(*stream), _ => None };
                        {
                            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(stream) = stream { state.streams.insert(stream); }
                            if gone { state.epoch += 1; state.references = Arc::default(); state.revision += 1; }
                        }
                        host.send(&reply, correlation);
                    }
                }
                loop {
                    let (epoch, facts) = {
                        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
                        if state.epoch == applied { break; }
                        if state.stopped { return; }
                        (state.epoch, state.facts.clone())
                    };
                    let Some(facts) = facts else { break; };
                    let mut next = PreparedResources::prepare(selection(&facts));
                    next.next_stream = ids.clone();
                    if shared.lock().unwrap_or_else(|e| e.into_inner()).stopped { return; }
                    let declared = next.declarations(handle).into_iter().all(|root| host.send(&Message::RootDeclare(root), 0) == vosu_status::VOSU_OK);
                    let references: Encoded = if declared { next.references().into_iter().filter_map(|r| vosu_plugin_api::encode(&r).ok().map(|bytes| (r.kind, bytes))).collect() } else {
                        host.log(LogLevel::Warn, "resource root declaration refused", CborValue::Null);
                        Vec::new()
                    };
                    {
                        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
                        if state.epoch != epoch { continue; }
                        state.references = Arc::new(references);
                        state.streams.clear();
                        state.revision += 1;
                    }
                    prepared = Some(next);
                    applied = epoch;
                    if let Some(bus) = bus.upgrade() { bus.refresh(); }
                }
            }
        }).expect("failed to start osu resource worker");
        Arc::new(Self {
            state,
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(Some(worker)),
        })
    }
    pub(crate) fn import_songs_root(&self) -> Option<std::path::PathBuf> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let facts = state.facts.as_ref()?;
        (!state.stopped && facts.source == ClientType::Stable && !facts.songs_folder.is_empty())
            .then(|| std::path::PathBuf::from(&facts.songs_folder))
    }
    pub fn observe(&self, snapshot: &StateBundleSnapshot) -> bool {
        let changed = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.stopped {
                return false;
            }
            if state
                .facts
                .as_ref()
                .is_some_and(|facts| facts.is_current(snapshot))
            {
                false
            } else {
                state.facts = Some(Arc::new(snapshot.cold_resource_facts()));
                state.epoch += 1;
                state.revision += 1;
                state.references = Arc::default();
                state.streams.clear();
                true
            }
        };
        if changed {
            let _ = self.send(Command::Wake);
        }
        changed
    }
    fn send(&self, command: Command) -> Result<(), vosu_status> {
        self.sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .ok_or(vosu_status::VOSU_ERR_CANCELLED)?
            .try_send(command)
            .map_err(|_| vosu_status::VOSU_ERR_NOT_READY)
    }
    pub fn request(&self, message: Message, correlation: u64) -> Result<(), vosu_status> {
        if let Message::StreamClose(close) = message {
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.stopped {
                    return Err(vosu_status::VOSU_ERR_CANCELLED);
                }
                if state.streams.remove(&close.stream) {
                    state.closed.push(close.stream);
                }
            }
            // A full queue already wakes the worker. Retain closes separately
            // (bounded by the 64 live leases) so pressure cannot leak a slot.
            let _ = self.send(Command::Wake);
            Ok(())
        } else {
            self.send(Command::Request(message, correlation))
        }
    }
    pub fn owns_stream(&self, stream: u64) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .streams
            .contains(&stream)
    }
    pub fn revision(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .revision
    }
    pub fn append(&self, writer: &mut PublishBatchWriter, clear: bool) -> u64 {
        let (revision, references) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (state.revision, state.references.clone())
        };
        for kind in [
            super::CHART,
            super::AUDIO,
            super::BACKGROUND,
            super::SET,
            super::SKIN,
        ] {
            let pointer = match kind {
                super::CHART => "/resources/file",
                super::AUDIO => "/resources/audio",
                super::BACKGROUND => "/resources/background",
                super::SET => "/resources/set",
                _ => "/tree",
            };
            let ns = if kind == super::SKIN {
                "vosu.skin"
            } else {
                "vosu.chart"
            };
            let value = references.iter().find(|(k, _)| !clear && k == kind).map_or(
                PublishValueRef::Null,
                |(_, bytes)| {
                    if matches!(kind, super::SET | super::SKIN) {
                        PublishValueRef::TreeRef(bytes)
                    } else {
                        PublishValueRef::ResourceRef(bytes)
                    }
                },
            );
            writer.push(ns, pointer, value);
        }
        if clear { 0 } else { revision }
    }
    pub fn cancel(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).stopped = true;
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
    pub fn join(&self) {
        // Serialize joins, including an async join whose waiter was cancelled.
        // The plugin's destroy thread must not free host callbacks until this
        // worker has actually exited.
        let mut worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            worker
                .as_ref()
                .is_none_or(|thread| thread.thread().id() != std::thread::current().id()),
            "resource worker cannot join itself"
        );
        if let Some(thread) = worker.take() {
            let _ = thread.join();
        }
    }
    pub async fn shutdown(self: &Arc<Self>) {
        self.cancel();
        let owner = self.clone();
        let _ = tokio::task::spawn_blocking(move || owner.join()).await;
    }
}
fn missing() -> ResourceResolved {
    ResourceResolved::Error {
        error: ErrorInfo {
            code: "not_found".into(),
            message: "resource is no longer selected".into(),
        },
    }
}
fn reply(prepared: &mut PreparedResources, message: Message) -> Option<Message> {
    Some(match message {
        Message::ResourceResolve(r) => {
            Message::ResourceResolved(prepared.resolve(&r.kind, &r.key).unwrap_or_else(missing))
        }
        Message::TreeList(r) => {
            Message::ResourceResolved(prepared.list(&r.kind, &r.key).unwrap_or_else(missing))
        }
        Message::TreeLookup(r) => Message::ResourceResolved(
            prepared
                .lookup(&r.kind, &r.key, &r.path)
                .unwrap_or_else(missing),
        ),
        Message::StreamRead(r) => {
            Message::StreamChunk(prepared.read(r).unwrap_or_else(|| StreamChunk::Error {
                error: ErrorInfo {
                    code: "not_found".into(),
                    message: "stream is closed".into(),
                },
            }))
        }
        Message::StreamClose(r) => {
            prepared.close(r.stream);
            return None;
        }
        _ => return None,
    })
}
fn selection(f: &ColdResourceFacts) -> Selection {
    let lazer = f.source == ClientType::Lazer;
    let root = |s: &str| (!s.is_empty()).then(|| std::path::PathBuf::from(s));
    let path = |name: &Option<String>| {
        name.as_ref().map(|name| {
            if lazer || f.folder.as_ref().is_none_or(String::is_empty) {
                name.clone()
            } else {
                format!("{}/{name}", f.folder.as_deref().unwrap_or_default())
            }
        })
    };
    let skin_root = if lazer {
        None
    } else {
        let mut components = std::path::Path::new(&f.skin_folder).components();
        match (components.next(), components.next()) {
            (Some(std::path::Component::Normal(name)), None) => {
                root(&f.game_folder).map(|p| p.join("Skins").join(name))
            }
            _ => None,
        }
    };
    Selection {
        roots: RootPaths {
            songs: root(&f.songs_folder),
            skin: skin_root,
        },
        lazer,
        chart: path(&f.filename),
        audio: path(&f.audio_filename),
        background: path(&f.background_filename),
        audio_name: f.set.as_ref().map(|set| set.audio_name.clone()),
        background_name: f.set.as_ref().map(|set| set.background_name.clone()),
        set: if lazer {
            f.set.as_ref().map(|set| {
                TreeSource::Store(
                    set.files
                        .iter()
                        .filter_map(|(name, hash)| {
                            store_path(hash).map(|path| (name.clone(), path))
                        })
                        .collect(),
                )
            })
        } else {
            f.folder.clone().map(TreeSource::Folder)
        },
        chart_name: if lazer {
            f.set.as_ref().and_then(|set| {
                set.files
                    .iter()
                    .find(|(_, hash)| {
                        Some(hash.as_str())
                            == f.filename.as_deref().and_then(|p| p.rsplit('/').next())
                    })
                    .map(|(name, _)| name.clone())
            })
        } else {
            f.filename.clone()
        },
        skin: if lazer {
            f.skin
                .as_ref()
                .map(|skin| TreeSource::Store(skin.files.clone()))
        } else {
            Some(TreeSource::Folder(String::new()))
        },
    }
}

fn store_path(hash: &str) -> Option<String> {
    (matches!(hash.len(), 32 | 64) && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{}/{}/{}", &hash[..1], &hash[..2], hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lazer_cold_facts_keep_logical_names_and_expand_content_store_hashes() {
        let hash = "a123456789abcdef0123456789abcdef";
        let physical = format!("a/a1/{hash}");
        let facts = ColdResourceFacts {
            source: ClientType::Lazer,
            instance_generation: 37,
            reset_epoch: 8,
            selection_generation: 19,
            game_folder: String::new(),
            songs_folder: "selected-store-root".into(),
            memory_songs_folder: "unselected-memory-root".into(),
            skin_folder: String::new(),
            skin: None,
            checksum: Some("distinct-checksum".into()),
            folder: Some(String::new()),
            filename: Some(physical.clone()),
            audio_filename: Some(physical.clone()),
            background_filename: None,
            set: Some(Arc::new(vosu_osu_model::memory::BeatmapResources {
                files: [
                    ("Pretty.osu".into(), hash.into()),
                    ("Invalid.osu".into(), "../escape".into()),
                ]
                .into_iter()
                .collect(),
                audio_name: "Sound.ogg".into(),
                background_name: "Image.png".into(),
            })),
        };
        let selected = selection(&facts);
        assert_eq!(selected.roots.songs, Some("selected-store-root".into()));
        assert_eq!(selected.chart.as_deref(), Some(physical.as_str()));
        assert_eq!(selected.chart_name.as_deref(), Some("Pretty.osu"));
        assert_eq!(selected.audio_name.as_deref(), Some("Sound.ogg"));
        let Some(TreeSource::Store(files)) = selected.set else {
            panic!("not a store");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files.get("Pretty.osu"), Some(&physical));
    }
}

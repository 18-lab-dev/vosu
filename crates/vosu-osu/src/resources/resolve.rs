use super::{
    Selection,
    identity::{digest, identity},
    roots::Root,
    set::{Tree, logical},
    skin,
};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use vosu_plugin_api::{
    CborValue, ResourceRef, ResourceResolved, RootDeclare, StreamChunk, StreamRead,
    VOSU_STREAM_CHUNK_MAX_BYTES,
};
use vosu_sys::fs::{FileIdentity, SafeRelativePath};

pub const CHART: &str = "file";
pub const AUDIO: &str = "audio";
pub const BACKGROUND: &str = "background";
pub const SET: &str = "set";
pub const SKIN: &str = "skin";
const MAX_STREAMS: usize = 64;

struct File {
    reference: ResourceRef,
    path: String,
    identity: FileIdentity,
    root: &'static str,
}

struct Lease {
    file: Mutex<std::fs::File>,
    identity: FileIdentity,
}

/// One cold-prepared disk-backed selection. Only identity metadata is retained.
/// A changed file invalidates the old reference rather than serving new bytes.
pub struct PreparedResources {
    roots: Vec<Root>,
    files: BTreeMap<&'static str, File>,
    trees: BTreeMap<&'static str, (ResourceRef, Tree)>,
    streams: BTreeMap<u64, Lease>,
    pub(super) next_stream: Arc<AtomicU64>,
}

impl PreparedResources {
    pub(super) fn prepare(selection: Selection) -> Self {
        let mut roots = Vec::with_capacity(2);
        let songs = if selection.lazer { "files" } else { "songs" };
        if let Some(root) = Root::open(songs, selection.roots.songs) {
            roots.push(root);
        }
        if let Some(root) = Root::open("skins", selection.roots.skin) {
            roots.push(root);
        }
        let mut result = Self {
            roots,
            files: BTreeMap::new(),
            trees: BTreeMap::new(),
            streams: BTreeMap::new(),
            next_stream: Arc::new(AtomicU64::new(1)),
        };
        if let Some(root) = result.roots.iter().find(|r| r.name == songs) {
            for (kind, path, name) in [
                (CHART, selection.chart, selection.chart_name),
                (AUDIO, selection.audio, selection.audio_name),
                (BACKGROUND, selection.background, selection.background_name),
            ] {
                let Some(path) = path.and_then(|p| logical(&p)) else {
                    continue;
                };
                let Some(identity) = identity(root, &path) else {
                    continue;
                };
                let content_id = CborValue::Text(digest(&identity.encode()));
                // Include content identity in the dispatch key: a stale key
                // cannot resolve to a replacement selection's changed bytes.
                let key = CborValue::Array(vec![CborValue::Text(path.clone()), content_id.clone()]);
                let name = name
                    .and_then(|n| logical(&n))
                    .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path).into());
                result.files.insert(
                    kind,
                    File {
                        reference: ResourceRef {
                            kind: kind.into(),
                            key,
                            name,
                            content_id,
                        },
                        path,
                        identity,
                        root: songs,
                    },
                );
            }
        }
        for (kind, source, root_name) in [
            (SET, selection.set, songs),
            (
                SKIN,
                selection.skin,
                if selection.lazer { "files" } else { "skins" },
            ),
        ] {
            let Some(source) = source else {
                continue;
            };
            let Some(root) = result.roots.iter().find(|r| r.name == root_name) else {
                continue;
            };
            let tree = Tree::prepare(root, &source);
            let content_id = CborValue::Text(tree.revision.clone());
            let mut identity = root.path.to_str().unwrap_or_default().as_bytes().to_vec();
            for (name, member) in &tree.members {
                identity.push(0);
                identity.extend_from_slice(name.as_bytes());
                identity.push(0);
                identity.extend_from_slice(member.physical.as_bytes());
            }
            let reference = ResourceRef {
                kind: kind.into(),
                key: CborValue::Text(digest(&identity)),
                name: kind.into(),
                content_id,
            };
            result.trees.insert(kind, (reference, tree));
        }
        result
    }

    pub fn declarations(&self, handle: u64) -> Vec<RootDeclare> {
        self.roots
            .iter()
            .filter_map(|r| r.declaration(handle))
            .collect()
    }
    pub fn references(&self) -> Vec<ResourceRef> {
        self.files
            .values()
            .map(|f| f.reference.clone())
            .chain(self.trees.values().map(|(r, _)| r.clone()))
            .collect()
    }
    /// Each resolution is an independent bounded stream lease. STREAM_CLOSE
    /// releases its slot without invalidating another request's representation.
    pub fn resolve(&mut self, kind: &str, key: &CborValue) -> Option<ResourceResolved> {
        let file = self.files.get(kind).filter(|f| &f.reference.key == key)?;
        if self.streams.len() >= MAX_STREAMS {
            return None;
        }
        // This is a blocking resource operation, never a capture-tick or Tokio
        // worker operation. Re-open beneath the retained root and verify that
        // exact object before granting the lease.
        let root = self.roots.iter().find(|r| r.name == file.root)?;
        let opened = SafeRelativePath::parse(&file.path)
            .ok()
            .and_then(|path| root.safe.open_child(&path).ok());
        let Some(opened) = opened else {
            return Some(ResourceResolved::Error { error: gone() });
        };
        if FileIdentity::of(&opened.file).ok() != Some(file.identity) {
            return Some(ResourceResolved::Error { error: gone() });
        }
        let stream = self
            .next_stream
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .ok()?;
        let size = file.identity.size;
        self.streams.insert(
            stream,
            Lease {
                file: Mutex::new(opened.file),
                identity: file.identity,
            },
        );
        Some(ResourceResolved::Stream { stream, size })
    }
    pub fn list(&self, kind: &str, key: &CborValue) -> Option<ResourceResolved> {
        let (reference, tree) = self.trees.get(kind)?;
        (&reference.key == key).then(|| tree.list())
    }
    pub fn lookup(&self, kind: &str, key: &CborValue, path: &str) -> Option<ResourceResolved> {
        let (reference, tree) = self.trees.get(kind)?;
        if &reference.key != key {
            return None;
        }
        if kind == SKIN {
            skin::lookup(tree, path)
        } else {
            tree.lookup(path, false)
        }
    }
    pub fn read(&self, request: StreamRead) -> Option<StreamChunk> {
        if request.len as usize > VOSU_STREAM_CHUNK_MAX_BYTES as usize {
            return None;
        }
        let lease = self.streams.get(&request.stream)?;
        // No callback or plugin/host call occurs under this lease-local lock.
        let mut file = lease.file.lock().unwrap_or_else(|error| error.into_inner());
        if FileIdentity::of(&file).ok() != Some(lease.identity) {
            return Some(StreamChunk::Error { error: gone() });
        }
        let offset = request.offset.min(lease.identity.size);
        let len = u64::from(request.len).min(lease.identity.size - offset) as usize;
        let mut data = vec![0; len];
        if file.seek(SeekFrom::Start(offset)).is_err()
            || file.read_exact(&mut data).is_err()
            || FileIdentity::of(&file).ok() != Some(lease.identity)
        {
            return Some(StreamChunk::Error { error: gone() });
        }
        Some(StreamChunk::Data {
            data: vosu_plugin_api::CborBytes(data),
        })
    }
    pub fn close(&mut self, stream: u64) {
        self.streams.remove(&stream);
    }
}

fn gone() -> vosu_plugin_api::ErrorInfo {
    vosu_plugin_api::ErrorInfo {
        code: "gone".into(),
        message: "resource representation changed or is unavailable".into(),
    }
}

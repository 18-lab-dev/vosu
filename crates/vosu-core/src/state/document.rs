//! Per-session namespace documents (S1, S5; plan:567-569): the latest-only
//! snapshot a committed [`StateStore`](super) session hands readers.
//! `SessionKey` mirrors `crate::plugin::session::SessionHandle`'s `u64`
//! value without this self-contained module depending on `plugin` (the
//! `state` module has no outgoing `crate::` edges,
//! `scripts/module-boundaries/vosu-core.txt`).

use std::fmt;
use std::sync::Arc;

use super::pointer::Pointer;
use super::value::{StateValue, write_document_json};

/// A session handle's bare `u64`, independent of
/// `crate::plugin::session::SessionHandle` (no cross-module dependency).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SessionKey(pub u64);

#[derive(Clone)]
pub(super) struct NamespaceEntry {
    pub(super) name: Arc<str>,
    pub(super) value: StateValue,
    pub(super) revision: u64,
}

/// One session's committed namespace documents at a point in time: the
/// `ArcSwap` payload `StateStore` publishes (MI). Immutable once built —
/// a new commit produces a new `SessionState`, never mutates this one.
pub struct SessionState {
    pub key: SessionKey,
    pub revision: u64,
    namespaces: Vec<NamespaceEntry>,
    api_encoding: ApiEncoding,
}

/// Hand-written, not `#[derive(Debug)]`: every logged view of a session
/// document goes through [`super::projector::Redacted`] instead (D2, E5),
/// so this prints only the key, revision, and namespace names — never a
/// raw value, which would otherwise print `vosu.osu.internal`'s install
/// paths straight into a `?state`/panic message or log line.
impl fmt::Debug for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionState")
            .field("key", &self.key)
            .field("revision", &self.revision)
            .field(
                "namespaces",
                &self.namespaces.iter().map(|e| &e.name).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl SessionState {
    /// Builds a session document from its committed namespaces, in display
    /// order (`vosu.client` plus every covered core/plugin namespace).
    pub fn new(key: SessionKey, revision: u64, namespaces: Vec<(Arc<str>, StateValue)>) -> Self {
        Self::from_entries(
            key,
            revision,
            namespaces
                .into_iter()
                .map(|(name, value)| NamespaceEntry {
                    name,
                    value,
                    revision,
                })
                .collect(),
        )
    }

    pub(super) fn from_entries(
        key: SessionKey,
        revision: u64,
        namespaces: Vec<NamespaceEntry>,
    ) -> Self {
        Self {
            key,
            revision,
            namespaces,
            api_encoding: ApiEncoding::default(),
        }
    }

    pub fn namespace_revision(&self, ns: &str) -> Option<u64> {
        self.namespaces
            .iter()
            .find(|entry| entry.name.as_ref() == ns)
            .map(|entry| entry.revision)
    }

    pub fn namespace(&self, ns: &str) -> Option<&StateValue> {
        self.namespaces
            .iter()
            .find(|entry| entry.name.as_ref() == ns)
            .map(|entry| &entry.value)
    }

    /// Looks up `p` below `ns`'s root; `None` for an uncovered namespace, a
    /// missing key, or a pointer that descends through an array or scalar.
    pub fn get(&self, ns: &str, p: &Pointer) -> Option<&StateValue> {
        self.namespace(ns)?.get(p.tokens())
    }

    pub fn namespaces(&self) -> impl Iterator<Item = (&str, &StateValue)> {
        self.namespaces
            .iter()
            .map(|entry| (entry.name.as_ref(), &entry.value))
    }

    pub(super) fn namespace_entries(&self) -> &[NamespaceEntry] {
        &self.namespaces
    }

    /// The whole document's API v1 JSON projection ([`super::Audience::Api`],
    /// `internal` always stripped), encoded once and cached (D2, D13).
    pub fn api_json(&self, reg: &super::schema::SchemaRegistry) -> &bytes::Bytes {
        self.api_encoding.json.get_or_init(|| {
            let doc =
                super::projector::project_document(reg, self, super::projector::Audience::Api);
            let mut out = Vec::new();
            write_document_json(&doc, &mut out);
            bytes::Bytes::from(out)
        })
    }
}

/// The per-session API v1 JSON encoding cache (D13): built once, from
/// [`Audience::Api`](super::projector::Audience) only, and never from a
/// plugin's wider grant — a cache keyed only by session revision would
/// otherwise leak a plugin's `internal` reads into a later API read of the
/// same revision.
#[derive(Debug, Default)]
struct ApiEncoding {
    json: std::sync::OnceLock<bytes::Bytes>,
}

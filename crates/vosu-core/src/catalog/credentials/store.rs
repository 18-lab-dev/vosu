use super::types::*;
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, fs, io::{Read, Write}, path::{Path, PathBuf}, sync::{Arc, Mutex}};

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default)]
    sources: Vec<Entry>,
}

// These DTOs are private to the persistence path, never service/public views.
#[derive(Deserialize, Serialize)]
struct Entry {
    source: String,
    #[serde(flatten)]
    credential: PersistedCredential,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum PersistedCredential {
    Token { token: String },
    Basic { username: String, password: String },
}

impl From<&CredentialInput> for PersistedCredential {
    fn from(input: &CredentialInput) -> Self {
        match input {
            CredentialInput::Token { token } => Self::Token { token: token.clone() },
            CredentialInput::Basic { username, password } => Self::Basic { username: username.clone(), password: password.clone() },
        }
    }
}

impl From<PersistedCredential> for CredentialInput {
    fn from(input: PersistedCredential) -> Self {
        match input {
            PersistedCredential::Token { token } => Self::Token { token },
            PersistedCredential::Basic { username, password } => Self::Basic { username, password },
        }
    }
}

type Entries = BTreeMap<String, Arc<CredentialInput>>;
#[derive(Clone)]
struct State {
    entries: Option<Arc<Entries>>,
    sources: Arc<BTreeSet<String>>,
    generation: u64,
}

/// One instance per channel config root, shared by Arc in daemon composition.
/// Blocking operations: async callers must use spawn_blocking. The operation
/// mutex serializes reads/publications/membership, never network or callbacks.
pub struct CredentialStore {
    path: PathBuf,
    operation: Mutex<()>,
    state: Mutex<State>,
    #[cfg(test)]
    fault: Mutex<Option<PublishFault>>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum PublishFault { Sync, Replace }

impl CredentialStore {
    /// Inject the already channel-aware config root; no release-root discovery.
    pub fn open(config_root: &Path) -> Self {
        let path = config_root.join("catalog-credentials.toml");
        let store = Self { path, operation: Mutex::new(()), state: Mutex::new(State { entries: None, sources: Arc::new(BTreeSet::new()), generation: 0 }), #[cfg(test)] fault: Mutex::new(None) };
        {
            let _operation = store.operation.lock().unwrap_or_else(|e| e.into_inner());
            let entries = store.read().ok().map(Arc::new);
            store.state.lock().unwrap_or_else(|e| e.into_inner()).entries = entries;
        }
        store
    }

    pub fn presence(&self, identity: &str) -> CredentialPresence {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        CredentialPresence {
            has_credential: state.sources.contains(identity) && state.entries.as_ref().is_some_and(|e| e.contains_key(identity)),
            availability: if state.entries.is_some() { CredentialAvailability::Available } else { CredentialAvailability::Unavailable },
            generation: state.generation,
        }
    }

    pub fn set(&self, identity: &str, input: CredentialInput) -> Result<u64, CredentialError> {
        input.validate()?;
        if !field(identity) || !input.matches_source(identity) { return Err(CredentialError::InvalidInput); }
        let (_operation, state) = self.begin();
        Self::known(&state, identity)?;
        let mut next = state.entries.as_ref().ok_or(CredentialError::Unavailable)?.as_ref().clone();
        next.insert(identity.to_owned(), Arc::new(input));
        self.publish(state, next)
    }

    pub fn delete(&self, identity: &str) -> Result<u64, CredentialError> {
        let (_operation, state) = self.begin();
        Self::known(&state, identity)?;
        let mut next = state.entries.as_ref().ok_or(CredentialError::Unavailable)?.as_ref().clone();
        if next.remove(identity).is_none() { return Ok(state.generation); }
        self.publish(state, next)
    }

    /// Explicit full-document repair. Never merges data from malformed bytes.
    pub fn repair(&self, inputs: Vec<(String, CredentialInput)>) -> Result<u64, CredentialError> {
        if inputs.len() > MAX_ENTRIES { return Err(CredentialError::InvalidInput); }
        let mut next = Entries::new();
        for (source, input) in inputs {
            input.validate()?;
            if !field(&source) || !input.matches_source(&source) || next.insert(source, Arc::new(input)).is_some() {
                return Err(CredentialError::InvalidInput);
            }
        }
        let (_operation, state) = self.begin();
        for source in next.keys() { Self::known(&state, source)?; }
        self.publish(state, next)
    }

    /// S0 owns the durable source roll-forward journal and calls this before
    /// source publication, including recovery, before exposing source services.
    /// A failed publication leaves membership and credentials unchanged.
    pub fn reconcile_sources(&self, identities: &[String]) -> Result<u64, CredentialError> {
        if identities.len() > MAX_ENTRIES || identities.iter().any(|s| !field(s)) { return Err(CredentialError::InvalidInput); }
        let sources: BTreeSet<_> = identities.iter().cloned().collect();
        if sources.len() != identities.len() { return Err(CredentialError::InvalidInput); }
        let (_operation, mut state) = self.begin();
        if state.sources.as_ref() == &sources && state.entries.as_ref().is_none_or(|entries| entries.keys().all(|s| sources.contains(s))) { return Ok(state.generation); }
        let generation = Self::next_generation(&state)?;
        if let Some(entries) = &state.entries {
            let mut next = entries.as_ref().clone();
            next.retain(|source, _| sources.contains(source));
            if next.len() != entries.len() {
                self.save(&next)?;
                state.entries = Some(Arc::new(next));
            }
        }
        // Unavailable stores remain untouched/disabled. Repair replaces all
        // bytes explicitly and checks this new membership, preventing revival.
        state.sources = Arc::new(sources);
        state.generation = generation;
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        Ok(generation)
    }

    /// Secret authority stays within core, not dashboard/surface/ABI exports.
    #[cfg_attr(not(test), expect(dead_code, reason = "core-only fetch authority is consumed by the source-service landing"))]
    pub(crate) fn lease(&self, identity: &str) -> Result<Option<(Arc<CredentialInput>, u64)>, CredentialError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Self::known(&state, identity)?;
        let entries = state.entries.as_ref().ok_or(CredentialError::Unavailable)?;
        Ok(entries.get(identity).map(|input| (Arc::clone(input), state.generation)))
    }

    fn begin(&self) -> (std::sync::MutexGuard<'_, ()>, State) {
        let operation = self.operation.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (operation, state)
    }

    fn known(state: &State, identity: &str) -> Result<(), CredentialError> {
        if state.sources.contains(identity) { Ok(()) } else { Err(CredentialError::UnknownSource) }
    }

    fn next_generation(state: &State) -> Result<u64, CredentialError> {
        state.generation.checked_add(1).ok_or(CredentialError::GenerationExhausted)
    }

    fn publish(&self, mut state: State, next: Entries) -> Result<u64, CredentialError> {
        let generation = Self::next_generation(&state)?;
        self.save(&next)?;
        state.entries = Some(Arc::new(next));
        state.generation = generation;
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = state;
        Ok(generation)
    }

    fn read(&self) -> Result<Entries, CredentialError> {
        // Serialized against this store's sole writer by state, including
        // initialization. Unlike read_published, take bounds allocation even
        // when another process grows a hostile file while it is being read.
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Entries::new()),
            Err(_) => return Err(CredentialError::Unavailable),
        };
        let mut bytes = Vec::new();
        file.take((MAX_DOCUMENT_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|_| CredentialError::Unavailable)?;
        if bytes.len() > MAX_DOCUMENT_BYTES { return Err(CredentialError::Unavailable); }
        let text = std::str::from_utf8(&bytes).map_err(|_| CredentialError::Unavailable)?;
        let document: Document = toml::from_str(text).map_err(|_| CredentialError::Unavailable)?;
        if document.sources.len() > MAX_ENTRIES { return Err(CredentialError::Unavailable); }
        let mut entries = Entries::new();
        for entry in document.sources {
            let input: CredentialInput = entry.credential.into();
            if !field(&entry.source) || input.validate().is_err() || !input.matches_source(&entry.source) || entries.insert(entry.source, Arc::new(input)).is_some() {
                return Err(CredentialError::Unavailable);
            }
        }
        Ok(entries)
    }

    fn save(&self, entries: &Entries) -> Result<(), CredentialError> {
        if entries.len() > MAX_ENTRIES { return Err(CredentialError::InvalidInput); }
        let document = Document { sources: entries.iter().map(|(source, input)| Entry { source: source.clone(), credential: input.as_ref().into() }).collect() };
        let text = toml::to_string(&document).map_err(|_| CredentialError::Persistence)?;
        if text.len() > MAX_DOCUMENT_BYTES { return Err(CredentialError::InvalidInput); }
        let parent = self.path.parent().ok_or(CredentialError::Persistence)?;
        fs::create_dir_all(parent).map_err(|_| CredentialError::Persistence)?;
        // NamedTempFile is 0600 from creation on Unix. Windows inherits the
        // destination-directory ACL, matching api_keys/broker convention.
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| CredentialError::Persistence)?;
        #[cfg(all(test, unix))]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(file.as_file().metadata().unwrap().mode() & 0o777, 0o600);
        }
        file.write_all(text.as_bytes()).and_then(|()| file.flush()).map_err(|_| CredentialError::Persistence)?;
        #[cfg(test)]
        if matches!(*self.fault.lock().unwrap(), Some(PublishFault::Sync)) {
            return Err(CredentialError::Persistence);
        }
        file.as_file().sync_all().map_err(|_| CredentialError::Persistence)?;
        // ReplaceFileW cannot replace while this replacement handle is open.
        let file = file.into_temp_path();
        #[cfg(test)]
        if matches!(*self.fault.lock().unwrap(), Some(PublishFault::Replace)) {
            return Err(CredentialError::Persistence);
        }
        vosu_sys::platform::atomic_replace(&file, &self.path).map_err(|_| CredentialError::Persistence)
    }
}

#[cfg(test)]
impl CredentialStore {
    pub(super) fn fail_publication(&self, fault: PublishFault) {
        *self.fault.lock().unwrap() = Some(fault);
    }
}

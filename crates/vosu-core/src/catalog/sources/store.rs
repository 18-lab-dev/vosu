//! Source settings and journaled cross-store removal. No legacy migration runs on reads.
use super::parse::{MAX_SOURCES, OFFICIAL_SOURCES, SourceIdentity};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const MAX_DOCUMENT: u64 = 512 * 1024;
/// Core composition supplies C0's serialized purge/membership operation. This must be
/// idempotent; an error must never be interpreted as successful removal.
pub trait SourceLifecycle: Send + Sync {
    fn reconcile_sources(&self, identities: &[String]) -> Result<(), SourceStoreError>;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInput {
    pub source: String,
    pub enabled: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub identity: SourceIdentity,
    pub enabled: bool,
    pub trusted: bool,
    pub private_network: bool,
    pub order: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceGeneration(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SourceStoreError {
    #[error("invalid catalog source settings")]
    Invalid,
    #[error("catalog source settings unavailable")]
    Unavailable,
    #[error("catalog source recovery required")]
    RecoveryRequired,
    #[error("catalog source not found")]
    NotFound,
    #[error("fixed official source policy cannot be changed")]
    OfficialPolicy,
}
pub(super) struct State {
    pub(super) records: Vec<SourceRecord>,
    pub(super) generation: SourceGeneration,
    pub(super) available: bool,
    pub(super) detections: std::collections::HashMap<
        SourceIdentity,
        Result<super::Forge, super::detect::DetectionError>,
    >,
    pub(super) candidates: Option<Arc<super::snapshot::CandidateSnapshot>>,
}
impl State {
    fn reconcile_runtime(&mut self, records: &[SourceRecord]) {
        self.detections.retain(|identity, result| {
            records.iter().any(|record| {
                record.identity == *identity
                    && (result.is_ok()
                        || self.records.iter().any(|previous| {
                            previous.identity == *identity
                                && previous.private_network == record.private_network
                                && previous.enabled == record.enabled
                        }))
            })
        });
        self.candidates = None;
    }
}
pub struct SourceStore {
    root: PathBuf,
    lifecycle: Arc<dyn SourceLifecycle>,
    // Operation authority is separate from state: no state mutex spans disk or callbacks.
    operation: Mutex<()>,
    pub(super) state: Mutex<State>,
}
impl SourceStore {
    pub fn open(
        root: &Path,
        lifecycle: Arc<dyn SourceLifecycle>,
    ) -> Result<Self, SourceStoreError> {
        let store = Self {
            root: root.into(),
            lifecycle,
            operation: Mutex::new(()),
            state: Mutex::new(State {
                records: Vec::new(),
                generation: SourceGeneration(0),
                available: false,
                detections: std::collections::HashMap::new(),
                candidates: None,
            }),
        };
        store.recover()?;
        Ok(store)
    }
    fn path(&self) -> PathBuf {
        self.root.join("catalog-source-settings.json")
    }
    fn journal(&self) -> PathBuf {
        self.root.join("catalog-source-settings.pending.json")
    }
    pub fn recover(&self) -> Result<(), SourceStoreError> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?;
        self.state
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?
            .available = false;
        let pending = read_records(&self.journal())?;
        let records = match pending.as_ref() {
            Some(records) => records.clone(),
            None => read_records(&self.path())?.unwrap_or_else(default_records),
        };
        validate_records(&records)?;
        self.reconcile(&records)?;
        if pending.is_some() {
            publish(&self.path(), &records)?;
            std::fs::remove_file(self.journal()).map_err(|_| SourceStoreError::Unavailable)?;
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?;
        state.reconcile_runtime(&records);
        state.records = records;
        state.generation.0 += 1;
        state.available = true;
        Ok(())
    }
    pub fn snapshot(&self) -> Result<(SourceGeneration, Vec<SourceRecord>), SourceStoreError> {
        let state = self
            .state
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?;
        if !state.available {
            return Err(SourceStoreError::RecoveryRequired);
        }
        Ok((state.generation, state.records.clone()))
    }
    pub fn replace(&self, input: &[SourceInput]) -> Result<SourceGeneration, SourceStoreError> {
        validate_input(input)?;
        self.mutate(|old| {
            Ok(input
                .iter()
                .enumerate()
                .map(|(order, input)| {
                    let identity = SourceIdentity::parse(input.source.clone())
                        .expect("validated input")
                        .identity;
                    let previous = old.iter().find(|record| record.identity == identity);
                    SourceRecord {
                        trusted: identity.is_official() || previous.is_some_and(|r| r.trusted),
                        private_network: previous.is_some_and(|r| r.private_network),
                        identity,
                        enabled: input.enabled,
                        order,
                    }
                })
                .collect())
        })
    }
    pub fn set_trust(
        &self,
        identity: &SourceIdentity,
        trusted: bool,
    ) -> Result<SourceGeneration, SourceStoreError> {
        self.set_flag(identity, trusted, false)
    }
    pub fn set_private_network(
        &self,
        identity: &SourceIdentity,
        allowed: bool,
    ) -> Result<SourceGeneration, SourceStoreError> {
        self.set_flag(identity, allowed, true)
    }
    fn set_flag(
        &self,
        identity: &SourceIdentity,
        value: bool,
        private: bool,
    ) -> Result<SourceGeneration, SourceStoreError> {
        self.mutate(|records| {
            let mut records = records.to_vec();
            let record = records
                .iter_mut()
                .find(|r| r.identity == *identity)
                .ok_or(SourceStoreError::NotFound)?;
            if identity.is_official() && (private && value || !private && !value) {
                return Err(SourceStoreError::OfficialPolicy);
            }
            if private {
                record.private_network = value;
            } else {
                record.trusted = value;
            }
            Ok(records)
        })
    }
    /// Invalidates pending fetch/plan stamps on credential changes or explicit refresh.
    pub fn invalidate(&self) -> Result<SourceGeneration, SourceStoreError> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?;
        if !state.available {
            return Err(SourceStoreError::RecoveryRequired);
        }
        state.candidates = None;
        state.generation.0 += 1;
        Ok(state.generation)
    }
    fn reconcile(&self, records: &[SourceRecord]) -> Result<(), SourceStoreError> {
        let identities = records
            .iter()
            .map(|r| r.identity.as_str().to_owned())
            .collect::<Vec<_>>();
        self.lifecycle.reconcile_sources(&identities)
    }
    fn mutate(
        &self,
        change: impl FnOnce(&[SourceRecord]) -> Result<Vec<SourceRecord>, SourceStoreError>,
    ) -> Result<SourceGeneration, SourceStoreError> {
        let _operation = self
            .operation
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?;
        let (_, old) = self.snapshot()?;
        let records = change(&old)?;
        if records == old {
            return Ok(self.snapshot()?.0);
        }
        validate_records(&records)?;
        publish(&self.journal(), &records)?;
        // Once the intent is durable, all failures require roll-forward recovery.
        self.state
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?
            .available = false;
        self.reconcile(&records)?;
        publish(&self.path(), &records)?;
        std::fs::remove_file(self.journal()).map_err(|_| SourceStoreError::Unavailable)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| SourceStoreError::Unavailable)?;
        state.reconcile_runtime(&records);
        state.records = records;
        state.generation.0 += 1;
        state.available = true;
        Ok(state.generation)
    }
}
fn default_records() -> Vec<SourceRecord> {
    OFFICIAL_SOURCES
        .iter()
        .enumerate()
        .map(|(order, source)| SourceRecord {
            identity: SourceIdentity::parse(*source)
                .expect("fixed source")
                .identity,
            enabled: true,
            trusted: true,
            private_network: false,
            order,
        })
        .collect()
}
fn validate_input(input: &[SourceInput]) -> Result<(), SourceStoreError> {
    if input.len() > MAX_SOURCES {
        return Err(SourceStoreError::Invalid);
    }
    let mut seen = HashSet::new();
    for source in input {
        SourceIdentity::parse(source.source.clone()).map_err(|_| SourceStoreError::Invalid)?;
        if !seen.insert(&source.source) {
            return Err(SourceStoreError::Invalid);
        }
    }
    Ok(())
}
fn validate_records(records: &[SourceRecord]) -> Result<(), SourceStoreError> {
    let input = records
        .iter()
        .map(|r| SourceInput {
            source: r.identity.as_str().into(),
            enabled: r.enabled,
        })
        .collect::<Vec<_>>();
    validate_input(&input)?;
    for (order, record) in records.iter().enumerate() {
        if record.order != order
            || record.identity.is_official() && (!record.trusted || record.private_network)
        {
            return Err(SourceStoreError::Invalid);
        }
    }
    Ok(())
}
fn read_records(path: &Path) -> Result<Option<Vec<SourceRecord>>, SourceStoreError> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.len() <= MAX_DOCUMENT => (),
        _ => return Err(SourceStoreError::Unavailable),
    }
    let bytes =
        vosu_sys::platform::read_published(path).map_err(|_| SourceStoreError::Unavailable)?;
    if bytes.len() as u64 > MAX_DOCUMENT {
        return Err(SourceStoreError::Unavailable);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| SourceStoreError::Invalid)
}
fn publish(path: &Path, records: &[SourceRecord]) -> Result<(), SourceStoreError> {
    let bytes = serde_json::to_vec(records).map_err(|_| SourceStoreError::Invalid)?;
    let parent = path.parent().ok_or(SourceStoreError::Unavailable)?;
    std::fs::create_dir_all(parent).map_err(|_| SourceStoreError::Unavailable)?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| SourceStoreError::Unavailable)?;
    file.write_all(&bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| SourceStoreError::Unavailable)?;
    // Close the replacement handle before Windows atomic replacement.
    let temp = file.into_temp_path();
    vosu_sys::platform::atomic_replace(&temp, path).map_err(|_| SourceStoreError::Unavailable)
}

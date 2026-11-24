//! Durable installed-graph records: exact provenance, artifact hash, runtime
//! approval binding and requested/dependency origin. Never a reference count,
//! credential or saved trust tier: load authority is recomputed from current
//! source policy (T0) every time.
use super::{InstallError, InstallLayout};
use crate::catalog::{
    approvals::{Approval, Artifact},
    plans::Reason,
    sources::SourceIdentity,
};
use serde::{Deserialize, Serialize};
use std::{io::Write as _, path::Path};
use vosu_plugin_api::manifest::{PackageKind, PackageManifest};

pub const RECORDS_FILE: &str = "catalog-installed.json";
pub const MAX_RECORDS: usize = 1024;
pub const MAX_RECORDS_BYTES: u64 = 4 * 1024 * 1024;
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstalledFor {
    Requested,
    Dependency,
}
impl InstalledFor {
    /// An explicit request promotes an installed dependency; a later
    /// dependency add never demotes an explicitly requested package.
    pub fn merge(previous: Option<Self>, reason: Reason) -> Self {
        match (previous, reason) {
            (Some(Self::Requested), _) | (_, Reason::Requested) => Self::Requested,
            _ => Self::Dependency,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstalledKind {
    Plugin,
    Tool,
    Overlay,
}
impl From<PackageKind> for InstalledKind {
    fn from(kind: PackageKind) -> Self {
        match kind {
            PackageKind::Plugin => Self::Plugin,
            PackageKind::Tool => Self::Tool,
            PackageKind::Overlay => Self::Overlay,
        }
    }
}
impl From<InstalledKind> for PackageKind {
    fn from(kind: InstalledKind) -> Self {
        match kind {
            InstalledKind::Plugin => Self::Plugin,
            InstalledKind::Tool => Self::Tool,
            InstalledKind::Overlay => Self::Overlay,
        }
    }
}

/// One committed package. `artifact` is present exactly for plugins and binds
/// the exact id/source/version/hash/runtime an `approval` may cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledRecord {
    pub kind: InstalledKind,
    pub id: String,
    pub version: String,
    pub source: SourceIdentity,
    pub target: String,
    pub sha256: String,
    pub artifact: Option<Artifact>,
    pub approval: Option<Approval>,
    pub installed_for: InstalledFor,
}
impl InstalledRecord {
    pub fn package_kind(&self) -> PackageKind {
        self.kind.into()
    }
}

/// Published installed graph. `generation` increases on every durable commit
/// and lets recovery decide roll-forward versus roll-back from the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledDocument {
    pub format: u32,
    pub generation: u64,
    pub records: Vec<InstalledRecord>,
}
impl InstalledDocument {
    pub fn record(&self, kind: PackageKind, id: &str) -> Option<&InstalledRecord> {
        let kind = InstalledKind::from(kind);
        self.records.iter().find(|r| r.kind == kind && r.id == id)
    }
    fn validate(&self) -> Result<(), InstallError> {
        if self.format != FORMAT || self.records.len() > MAX_RECORDS {
            return Err(InstallError::RecordsInvalid);
        }
        for (index, record) in self.records.iter().enumerate() {
            let duplicate = self.records[..index]
                .iter()
                .any(|other| other.kind == record.kind && other.id == record.id);
            let artifact_matches = match (&record.artifact, record.kind) {
                (Some(artifact), InstalledKind::Plugin) => {
                    artifact.id == record.id
                        && artifact.source == record.source
                        && artifact.version == record.version
                        && hex(&artifact.sha256) == record.sha256
                }
                (None, InstalledKind::Tool | InstalledKind::Overlay) => record.approval.is_none(),
                _ => false,
            };
            if duplicate
                || !artifact_matches
                || !vosu_net::streaming::portable_component(&record.id)
                || record.id.contains(['/', '\\'])
            {
                return Err(InstallError::RecordsInvalid);
            }
        }
        Ok(())
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 15)] as char);
    }
    text
}

/// Missing means an empty graph. Unreadable/malformed input is a typed refusal,
/// never rewritten: discovery/load stop until an explicit repair.
pub(super) fn read_records(layout: &InstallLayout) -> Result<InstalledDocument, InstallError> {
    let path = layout.state.join(RECORDS_FILE);
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstalledDocument {
                format: FORMAT,
                ..InstalledDocument::default()
            });
        }
        Ok(meta) if meta.len() <= MAX_RECORDS_BYTES => (),
        _ => return Err(InstallError::RecordsInvalid),
    }
    let bytes = vosu_sys::platform::read_published(&path).map_err(InstallError::io)?;
    if bytes.len() as u64 > MAX_RECORDS_BYTES {
        return Err(InstallError::RecordsInvalid);
    }
    let document: InstalledDocument =
        serde_json::from_slice(&bytes).map_err(|_| InstallError::RecordsInvalid)?;
    document.validate()?;
    Ok(document)
}

/// Same-directory temp, flush/sync, close the replacement handle, then the
/// shared `atomic_replace` (Windows inherits the directory ACL).
pub(super) fn publish_json(path: &Path, value: &impl Serialize) -> Result<(), InstallError> {
    let bytes = serde_json::to_vec(value).map_err(|_| InstallError::RecordsInvalid)?;
    if bytes.len() as u64 > MAX_RECORDS_BYTES {
        return Err(InstallError::RecordsInvalid);
    }
    let parent = path.parent().ok_or(InstallError::RecordsInvalid)?;
    std::fs::create_dir_all(parent).map_err(InstallError::io)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(InstallError::io)?;
    file.write_all(&bytes)
        .and_then(|()| file.flush())
        .and_then(|()| file.as_file().sync_all())
        .map_err(InstallError::io)?;
    let temp = file.into_temp_path();
    vosu_sys::platform::atomic_replace(&temp, path).map_err(InstallError::io)
}

pub(in crate::catalog) fn publish_records(
    layout: &InstallLayout,
    document: &InstalledDocument,
) -> Result<(), InstallError> {
    document.validate()?;
    publish_json(&layout.state.join(RECORDS_FILE), document)
}

/// The current SDK-validated manifest of one committed package, read through
/// the same confined, bounded asset reader preparation used. It must still be
/// the recorded kind/id/version; anything else is a refusal, never a guess.
pub(in crate::catalog) fn read_manifest(
    layout: &InstallLayout,
    record: &InstalledRecord,
) -> Result<PackageManifest, InstallError> {
    let mismatch = || InstallError::InstalledManifest(record.id.clone());
    let root = layout.destination(record.package_kind(), &record.id)?;
    let text =
        super::preflight::read_asset(&root, "manifest.toml", super::preflight::MAX_MANIFEST_BYTES)
            .map_err(|_| mismatch())?;
    let manifest = PackageManifest::parse(&text).map_err(|_| mismatch())?;
    let fields = manifest.fields();
    if manifest.kind() != record.package_kind()
        || fields.id != record.id
        || fields.version != record.version
    {
        return Err(mismatch());
    }
    Ok(manifest)
}

/// Published installed graph for discovery/load. `open` runs crash recovery
/// first, so no loader can observe a partially published graph; registry
/// discovery must obtain installed packages only through an opened store.
pub struct InstalledStore {
    layout: InstallLayout,
    document: std::sync::Mutex<InstalledDocument>,
}
impl InstalledStore {
    pub fn open(layout: InstallLayout) -> Result<Self, InstallError> {
        super::recovery::recover(&layout)?;
        let document = read_records(&layout)?;
        Ok(Self {
            layout,
            document: std::sync::Mutex::new(document),
        })
    }
    pub fn layout(&self) -> &InstallLayout {
        &self.layout
    }
    /// Coherent in-memory snapshot; never performs I/O.
    pub fn snapshot(&self) -> InstalledDocument {
        self.document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn generation(&self) -> u64 {
        self.document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
    }
    /// Publish in memory only after the durable commit point succeeded.
    pub(in crate::catalog) fn published(&self, document: InstalledDocument) {
        *self
            .document
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = document;
    }
}

//! Process-plugin approval (catalog-v2.md "Process-plugin approval and
//! management", A5.5; `docs/v2/plugin-abi.md` §7.1).
//!
//! Approval is the explicit user decision that lets a `user`-trust plugin
//! run native code under `runtime = "process"`. An [`ApprovalRecord`] binds
//! the plugin id, the runtime, the SHA-256 of the exact `manifest.toml`
//! bytes, the SHA-256 of the host target's library and the package's
//! [`Provenance`]. Records live in [`APPROVALS_FILE`] under the
//! configuration root, one per `(id, provenance)`, and only an explicit
//! approval writes one.
//!
//! [`ApprovalGate`] is the only launcher production composition installs.
//! Every load and every restart, automatic or manual, passes through it: it
//! hashes the library now, compares every bound value with the record, and
//! starts the child only on an exact match, handing the launcher the
//! *approved* digest. The child then verifies that digest against the
//! library object it opens (§7.6 "Startup" step 6), so bytes replaced after
//! this check fail startup with stage `approval`. Anything else leaves the
//! plugin unloaded with stage `approval`: never a crash event and never
//! retried (§7.6 startup stages).

use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::error::HostError;
use super::execution::{ProcessEndpoint, ProcessLaunch, ProcessLauncher, ProcessPackage};
use super::process::control::{StartupFailure, StartupStage};
use super::process::launcher::{ProcessHostLauncher, library_digest};

/// The approval store, relative to the configuration root (which is
/// already the `prerelease/` subdirectory in a prerelease build, D21).
pub const APPROVALS_FILE: &str = "plugins/approvals.json";

/// The only `version` of [`APPROVALS_FILE`] this build reads or writes.
const FILE_VERSION: u32 = 1;

/// A SHA-256 digest, written as 64 lowercase hexadecimal digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    /// Parses exactly 64 lowercase hexadecimal digits.
    pub fn parse_hex(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let mut digest = [0_u8; 32];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            digest[index] = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        Some(Self(digest))
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Sha256Digest({self})")
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse_hex(&text).ok_or_else(|| {
            serde::de::Error::custom("a SHA-256 digest is 64 lowercase hexadecimal digits")
        })
    }
}

/// Where an approved package came from. M5's `--plugin` packages are
/// `development`; M6b writes `installed` records of the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// A `--plugin <dir>` package: its canonical absolute directory.
    Development { path: PathBuf },
    /// A catalog-installed package (M6b).
    Installed {
        source: String,
        version: String,
        package_sha256: Sha256Digest,
    },
}

impl Provenance {
    /// The provenance of a process package. Every M5 process package is a
    /// `--plugin` development package (`registry::load_development`; a
    /// bundled manifest is refused unless it requests `in-process`), so its
    /// provenance is its canonical directory.
    pub fn of(package: &ProcessPackage) -> Self {
        Self::Development {
            path: package.root().to_owned(),
        }
    }
}

/// The runtime an approval grants: always `process`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovedRuntime {
    Process,
}

/// One explicit approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRecord {
    pub id: String,
    pub runtime: ApprovedRuntime,
    pub manifest_sha256: Sha256Digest,
    pub library_sha256: Sha256Digest,
    pub provenance: Provenance,
}

/// The digests a package would be approved under right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Digests {
    pub manifest_sha256: Sha256Digest,
    pub library_sha256: Sha256Digest,
}

impl Digests {
    /// Hashes `package`'s library now; the manifest digest is the one its
    /// validated bytes had.
    pub fn of(package: &ProcessPackage) -> Result<Self, StartupFailure> {
        Ok(Self {
            manifest_sha256: Sha256Digest(*package.manifest_sha256()),
            library_sha256: Sha256Digest(library_digest(package.library())?),
        })
    }

    /// The record approving `id` from `provenance` with these digests.
    pub fn record(self, id: &str, provenance: Provenance) -> ApprovalRecord {
        ApprovalRecord {
            id: id.to_owned(),
            runtime: ApprovedRuntime::Process,
            manifest_sha256: self.manifest_sha256,
            library_sha256: self.library_sha256,
            provenance,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalsFile {
    version: u32,
    approvals: Vec<ApprovalRecord>,
}

/// The approval store, [`APPROVALS_FILE`]. A file that cannot be read or
/// parsed fails closed: every check refuses and nothing is written over
/// it until it is fixed or removed and vosu restarts.
pub struct ApprovalStore {
    path: PathBuf,
    records: Mutex<Result<Vec<ApprovalRecord>, String>>,
}

impl fmt::Debug for ApprovalStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ApprovalStore {
    /// Reads `<config_root>/plugins/approvals.json`. A missing file is an
    /// empty store.
    pub fn open(config_root: &Path) -> Self {
        let path = config_root.join(APPROVALS_FILE);
        let records = read(&path).map_err(|error| {
            let message = format!("{} is unreadable: {error}", path.display());
            tracing::warn!("{message}; every process plugin needing approval stays unloaded");
            message
        });
        Self {
            path,
            records: Mutex::new(records),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, Result<Vec<ApprovalRecord>, String>> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The record for `(id, provenance)`. `Err` when the store failed to
    /// load.
    pub fn record(
        &self,
        id: &str,
        provenance: &Provenance,
    ) -> Result<Option<ApprovalRecord>, String> {
        let records = self.lock();
        let records = records.as_ref().map_err(Clone::clone)?;
        Ok(records
            .iter()
            .find(|record| record.id == id && &record.provenance == provenance)
            .cloned())
    }

    /// Persists `record`, replacing the one for the same `(id, provenance)`.
    /// The file is replaced atomically in its own directory before the
    /// in-memory store changes.
    pub fn approve(&self, record: ApprovalRecord) -> io::Result<()> {
        let mut records = self.lock();
        let current = records
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?;
        let mut next: Vec<ApprovalRecord> = current
            .iter()
            .filter(|existing| {
                !(existing.id == record.id && existing.provenance == record.provenance)
            })
            .cloned()
            .collect();
        next.push(record);
        write(&self.path, &next)?;
        *records = Ok(next);
        Ok(())
    }

    /// Removes the record for `(id, provenance)`; `Ok(false)` when there
    /// was none.
    pub fn revoke(&self, id: &str, provenance: &Provenance) -> io::Result<bool> {
        let mut records = self.lock();
        let current = records
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?;
        let next: Vec<ApprovalRecord> = current
            .iter()
            .filter(|existing| !(existing.id == id && &existing.provenance == provenance))
            .cloned()
            .collect();
        if next.len() == current.len() {
            return Ok(false);
        }
        write(&self.path, &next)?;
        *records = Ok(next);
        Ok(true)
    }

    /// Whether `id`'s `package` may start now: its library is hashed now
    /// and every bound value must equal the record. Returns the approved
    /// library digest the child must find.
    pub fn check(
        &self,
        id: &str,
        package: &ProcessPackage,
    ) -> Result<Sha256Digest, StartupFailure> {
        let digests = Digests::of(package)?;
        let provenance = Provenance::of(package);
        let refused = |code: &str, message: String| {
            Err(StartupFailure::new(StartupStage::Approval, code, message))
        };
        match self.record(id, &provenance) {
            Err(error) => refused("approvals_unreadable", error),
            Ok(None) => refused(
                "approval_required",
                format!("{id} has no approval for this package"),
            ),
            Ok(Some(record)) if record == digests.record(id, provenance) => {
                Ok(digests.library_sha256)
            }
            Ok(Some(_)) => refused(
                "approval_changed",
                format!("{id}'s manifest or library changed since it was approved"),
            ),
        }
    }
}

fn read(path: &Path) -> io::Result<Vec<ApprovalRecord>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let file: ApprovalsFile = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if file.version != FILE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported version {}", file.version),
        ));
    }
    for (index, record) in file.approvals.iter().enumerate() {
        if file.approvals[..index]
            .iter()
            .any(|earlier| earlier.id == record.id && earlier.provenance == record.provenance)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("two records for {}", record.id),
            ));
        }
    }
    Ok(file.approvals)
}

/// Replaces `path` atomically in its own directory (the `tools` state
/// precedent, `src/tools/mod.rs` `atomic_json`).
fn write(path: &Path, approvals: &[ApprovalRecord]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("approval store needs a parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(
        &mut temporary,
        &ApprovalsFile {
            version: FILE_VERSION,
            approvals: approvals.to_vec(),
        },
    )?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Starts a process child given the approved library digest.
pub trait ApprovedLaunch: Send + Sync {
    fn launch_approved(
        &self,
        launch: ProcessLaunch<'_>,
        library_sha256: [u8; 32],
    ) -> Result<Arc<dyn ProcessEndpoint>, HostError>;
}

impl ApprovedLaunch for ProcessHostLauncher {
    fn launch_approved(
        &self,
        launch: ProcessLaunch<'_>,
        library_sha256: [u8; 32],
    ) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        ProcessHostLauncher::launch_approved(self, launch, library_sha256)
    }
}

/// The production [`ProcessLauncher`]: a launch whose package has no
/// matching approval record never reaches the inner launcher.
pub struct ApprovalGate<L: ApprovedLaunch = ProcessHostLauncher> {
    store: Arc<ApprovalStore>,
    launcher: L,
}

impl<L: ApprovedLaunch> ApprovalGate<L> {
    pub fn new(store: Arc<ApprovalStore>, launcher: L) -> Self {
        Self { store, launcher }
    }
}

impl<L: ApprovedLaunch> ProcessLauncher for ApprovalGate<L> {
    fn launch(&self, launch: ProcessLaunch<'_>) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        match self.store.check(&launch.manifest.id, launch.package) {
            Ok(approved) => self.launcher.launch_approved(launch, approved.0),
            Err(failure) => {
                // One WARN per load attempt (A5.5 "Noninteractive starts").
                if failure.stage == StartupStage::Approval {
                    tracing::warn!(
                        plugin = %launch.manifest.id,
                        %failure,
                        "plugin {} is not loaded: it needs approval, which only the vosu dashboard can give",
                        launch.manifest.id
                    );
                } else {
                    tracing::warn!(plugin = %launch.manifest.id, %failure, "plugin host did not start");
                }
                Err(HostError::ProcessStartupFailed(failure))
            }
        }
    }
}

#[cfg(test)]
mod tests;

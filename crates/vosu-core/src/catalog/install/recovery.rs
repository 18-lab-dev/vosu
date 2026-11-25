//! Versioned install journal and crash recovery.
//!
//! The published installed-records generation is the only commit point. A
//! journal whose `next_generation` equals the records generation rolls
//! forward (remove backups); one equal to `previous_generation` rolls back
//! (move each published tree out of the way, restore its backup). Any other
//! state is refused rather than guessed. Recovery runs from
//! `InstalledStore::open`, before registry discovery/load, and again at the
//! start of every catalog operation under the operation gate.
//!
//! A removal operation (U0 uninstall/autoremove) has no staging tree: its
//! destination only moves to its backup, so roll-forward deletes the backup
//! and roll-back moves it back. Durable data/settings are never journal paths.
use super::{InstallError, InstallLayout, installed};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use vosu_net::streaming::rename_directory_no_replace;
use vosu_plugin_api::manifest::PackageKind;

pub const JOURNAL_FILE: &str = "catalog-install-journal.json";
pub(super) const BACKUP_PREFIX: &str = ".vosu-backup-";
/// Prefix owned by `vosu_net::streaming::extract_zip_staged`.
const STAGING_PREFIX: &str = ".stage-";
const FORMAT: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct JournalOperation {
    /// `None` for a removal: the destination only moves to its backup.
    pub staging: Option<PathBuf>,
    pub destination: PathBuf,
    pub backup: Option<PathBuf>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    format: u32,
    pub previous_generation: u64,
    pub next_generation: u64,
    pub operations: Vec<JournalOperation>,
}
impl Journal {
    pub fn new(previous: u64, next: u64, operations: Vec<JournalOperation>) -> Self {
        Self {
            format: FORMAT,
            previous_generation: previous,
            next_generation: next,
            operations,
        }
    }
    /// A journal may only name direct children of the layout's kind roots
    /// with the expected roles; a tampered path is never moved or deleted.
    fn validate(&self, layout: &InstallLayout) -> Result<(), InstallError> {
        let roots = [PackageKind::Plugin, PackageKind::Tool, PackageKind::Overlay]
            .map(|kind| layout.root(kind));
        let child = |path: &Path, prefix: Option<&str>| {
            let name = path.file_name().and_then(|n| n.to_str());
            roots.iter().any(|root| path.parent() == Some(*root))
                && name.is_some_and(|name| match prefix {
                    Some(prefix) => name.starts_with(prefix),
                    None => !name.starts_with('.'),
                })
        };
        let valid = self.format == FORMAT
            && self.next_generation == self.previous_generation.wrapping_add(1)
            && self.operations.len() <= installed::MAX_RECORDS
            && self.operations.iter().all(|operation| {
                child(&operation.destination, None)
                    && match &operation.staging {
                        Some(staging) => {
                            child(staging, Some(STAGING_PREFIX))
                                && staging.parent() == operation.destination.parent()
                        }
                        // A removal is only ever a move to its own backup.
                        None => operation.backup.is_some(),
                    }
                    && operation.backup.as_ref().is_none_or(|backup| {
                        child(backup, Some(BACKUP_PREFIX))
                            && backup.parent() == operation.destination.parent()
                    })
            });
        if valid {
            Ok(())
        } else {
            Err(InstallError::RecoveryRequired)
        }
    }
}

pub(super) fn publish_journal(
    layout: &InstallLayout,
    journal: &Journal,
) -> Result<(), InstallError> {
    journal.validate(layout)?;
    installed::publish_json(&layout.state.join(JOURNAL_FILE), journal)
}

fn read_journal(layout: &InstallLayout) -> Result<Option<Journal>, InstallError> {
    let path = layout.state.join(JOURNAL_FILE);
    match std::fs::metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.len() <= MAX_JOURNAL_BYTES => (),
        _ => return Err(InstallError::RecoveryRequired),
    }
    let bytes = vosu_sys::platform::read_published(&path).map_err(InstallError::io)?;
    let journal: Journal =
        serde_json::from_slice(&bytes).map_err(|_| InstallError::RecoveryRequired)?;
    journal.validate(layout)?;
    Ok(Some(journal))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    NoJournal,
    RolledForward,
    RolledBack,
}

/// Resolve `journal` against the published records generation, then remove it.
pub(super) fn resolve(
    layout: &InstallLayout,
    journal: &Journal,
) -> Result<Resolution, InstallError> {
    let published = installed::read_records(layout)?.generation;
    let resolution = if published == journal.next_generation {
        for operation in &journal.operations {
            if let Some(backup) = &operation.backup {
                remove_tree(backup)?;
            }
            if let Some(staging) = &operation.staging {
                remove_tree(staging)?;
            }
        }
        Resolution::RolledForward
    } else if published == journal.previous_generation {
        for operation in journal.operations.iter().rev() {
            if let Some(staging) = &operation.staging
                && !staging.exists()
                && operation.destination.exists()
            {
                // This operation's new tree is published: move it aside.
                rename_directory_no_replace(&operation.destination, staging)
                    .map_err(InstallError::io)?;
            }
            if let Some(backup) = &operation.backup
                && backup.exists()
                && !operation.destination.exists()
            {
                rename_directory_no_replace(backup, &operation.destination)
                    .map_err(InstallError::io)?;
            }
        }
        for staging in journal.operations.iter().filter_map(|o| o.staging.as_ref()) {
            remove_tree(staging)?;
        }
        Resolution::RolledBack
    } else {
        return Err(InstallError::RecoveryRequired);
    };
    remove_journal(layout)?;
    Ok(resolution)
}

/// Crash recovery: resolve an interrupted commit, then remove unpublished
/// staging/backup trees this installer owns and leftover downloads.
pub fn recover(layout: &InstallLayout) -> Result<Resolution, InstallError> {
    let resolution = match read_journal(layout)? {
        Some(journal) => resolve(layout, &journal)?,
        None => Resolution::NoJournal,
    };
    for kind in [PackageKind::Plugin, PackageKind::Tool, PackageKind::Overlay] {
        let root = layout.root(kind);
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(InstallError::io)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(STAGING_PREFIX) || name.starts_with(BACKUP_PREFIX) {
                remove_tree(&entry.path())?;
            }
        }
    }
    if layout.downloads.exists() {
        for entry in std::fs::read_dir(&layout.downloads).map_err(InstallError::io)? {
            remove_tree(&entry.map_err(InstallError::io)?.path())?;
        }
    }
    Ok(resolution)
}

fn remove_tree(path: &Path) -> Result<(), InstallError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(InstallError::io(error)),
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path).map_err(InstallError::io),
        Ok(_) => std::fs::remove_file(path).map_err(InstallError::io),
    }
}

fn remove_journal(layout: &InstallLayout) -> Result<(), InstallError> {
    match std::fs::remove_file(layout.state.join(JOURNAL_FILE)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(InstallError::io(error)),
        _ => Ok(()),
    }
}

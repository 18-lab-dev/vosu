//! Journaled, same-filesystem, dependency-first publication of a prepared graph.
//!
//! Order: (1) every package is already staged and verified under its own kind
//! root; (2) the versioned journal naming every staging/destination/backup path
//! and both record generations is durably published; (3) per add, in
//! dependency-first order, an existing managed directory moves to its backup
//! and the staged tree moves into place (no-replace renames); (4) the installed
//! records, including provenance, approvals and `installed_for`, are published
//! with the journal's next generation; this single replacement is the commit
//! point; (5) backups and then the journal are removed. Any failure resolves
//! the journal from the published records generation: roll forward after (4),
//! otherwise roll back every directory. Crash recovery applies the same rule.
use super::{
    InstallError, InstallLayout,
    installed::{InstalledDocument, InstalledFor, InstalledRecord, publish_records},
    prepare::Prepared,
    recovery::{self, Journal, JournalOperation},
};
use crate::catalog::approvals::Approval;
use vosu_net::streaming::rename_directory_no_replace;

/// Test-only crash/failure injection at each publication boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FaultPoint {
    JournalPublished,
    BackupMoved(usize),
    DirectoryMoved(usize),
    RecordsPublished,
}
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Faults {
    #[cfg(test)]
    pub crash: Option<FaultPoint>,
    #[cfg(test)]
    pub fail: Option<FaultPoint>,
}
impl Faults {
    fn at(self, point: FaultPoint) -> Result<(), Stop> {
        #[cfg(test)]
        {
            if self.crash == Some(point) {
                return Err(Stop::Crash);
            }
            if self.fail == Some(point) {
                return Err(Stop::Failed(InstallError::Io("injected fault".into())));
            }
        }
        let _ = point;
        Ok(())
    }
}
enum Stop {
    /// Simulated process death: leave every byte exactly as it is.
    #[cfg(test)]
    Crash,
    Failed(InstallError),
}
impl From<InstallError> for Stop {
    fn from(error: InstallError) -> Self {
        Self::Failed(error)
    }
}

/// Committed records in dependency-first plan order.
pub(super) struct Committed {
    pub document: InstalledDocument,
    pub records: Vec<InstalledRecord>,
}

pub(super) fn commit(
    layout: &InstallLayout,
    current: &InstalledDocument,
    prepared: Vec<Prepared>,
    approvals: &[Approval],
    faults: Faults,
) -> Result<Committed, InstallError> {
    let next_generation = current
        .generation
        .checked_add(1)
        .ok_or(InstallError::RecordsInvalid)?;
    let mut document = current.clone();
    document.generation = next_generation;
    // Pass 1 checks every add while staged trees are still drop-owned, so a
    // refusal removes all of them and publishes nothing.
    let mut planned = Vec::with_capacity(prepared.len());
    let mut committed: Vec<InstalledRecord> = Vec::with_capacity(prepared.len());
    for (index, item) in prepared.into_iter().enumerate() {
        let kind = item.record.package_kind();
        let destination = layout.destination(kind, &item.record.id)?;
        if committed
            .iter()
            .any(|r| r.kind == item.record.kind && r.id == item.record.id)
        {
            return Err(InstallError::DuplicateAdd(item.record.id.clone()));
        }
        let previous = current.record(kind, &item.record.id);
        // No-overwrite safety: never replace an unmanaged or foreign package,
        // and never perform an implicit `replaces` data migration here.
        if destination.exists() && previous.is_none() {
            return Err(InstallError::Unmanaged(item.record.id.clone()));
        }
        if previous.is_some_and(|r| r.source != item.record.source) {
            return Err(InstallError::DifferentSource(item.record.id.clone()));
        }
        for old in &item.replaces {
            if current.record(kind, old).is_some() || layout.destination(kind, old)?.exists() {
                return Err(InstallError::ReplacementMigration(old.clone()));
            }
        }
        let mut record = item.record.clone();
        record.installed_for = InstalledFor::merge(previous.map(|r| r.installed_for), item.reason);
        record.approval = record.artifact.as_ref().and_then(|artifact| {
            approvals
                .iter()
                .find(|a| a.matches(artifact))
                .or_else(|| {
                    previous
                        .and_then(|r| r.approval.as_ref())
                        .filter(|a| a.matches(artifact))
                })
                .cloned()
        });
        let backup = destination.exists().then(|| {
            layout
                .root(kind)
                .join(format!("{}{}-{index}", recovery::BACKUP_PREFIX, record.id))
        });
        if backup.as_ref().is_some_and(|b| b.exists()) {
            return Err(InstallError::RecoveryRequired);
        }
        match document
            .records
            .iter_mut()
            .find(|r| r.kind == record.kind && r.id == record.id)
        {
            Some(slot) => *slot = record.clone(),
            None => document.records.push(record.clone()),
        }
        committed.push(record);
        planned.push((item, destination, backup));
    }
    if document.records.len() > super::installed::MAX_RECORDS {
        return Err(InstallError::RecordsInvalid);
    }
    // Pass 2: the journal owns every kept staging tree once it is durable.
    let operations: Vec<_> = planned
        .into_iter()
        .map(|(item, destination, backup)| JournalOperation {
            staging: item.staged.keep(),
            destination,
            backup,
        })
        .collect();
    let journal = Journal::new(current.generation, next_generation, operations);
    if let Err(error) = recovery::publish_journal(layout, &journal) {
        for operation in &journal.operations {
            let _ = std::fs::remove_dir_all(&operation.staging);
        }
        return Err(error);
    }
    match publish(layout, &journal, &document, faults) {
        Ok(()) => {}
        #[cfg(test)]
        Err(Stop::Crash) => return Err(InstallError::SimulatedCrash),
        Err(Stop::Failed(error)) => {
            return match recovery::resolve(layout, &journal) {
                Ok(recovery::Resolution::RolledForward) => Ok(Committed {
                    document,
                    records: committed,
                }),
                Ok(recovery::Resolution::RolledBack) => Err(error),
                Ok(recovery::Resolution::NoJournal) | Err(_) => Err(InstallError::RecoveryRequired),
            };
        }
    }
    // Committed. Cleanup failure leaves the journal for roll-forward on the
    // next operation or open; it never un-commits the graph.
    let _ = recovery::resolve(layout, &journal);
    Ok(Committed {
        document,
        records: committed,
    })
}

fn publish(
    layout: &InstallLayout,
    journal: &Journal,
    document: &InstalledDocument,
    faults: Faults,
) -> Result<(), Stop> {
    faults.at(FaultPoint::JournalPublished)?;
    for (index, operation) in journal.operations.iter().enumerate() {
        if let Some(backup) = &operation.backup {
            rename_directory_no_replace(&operation.destination, backup)
                .map_err(InstallError::io)?;
            faults.at(FaultPoint::BackupMoved(index))?;
        }
        rename_directory_no_replace(&operation.staging, &operation.destination)
            .map_err(InstallError::io)?;
        faults.at(FaultPoint::DirectoryMoved(index))?;
    }
    publish_records(layout, document)?;
    faults.at(FaultPoint::RecordsPublished)?;
    Ok(())
}

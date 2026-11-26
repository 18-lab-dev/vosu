//! A6b.4/A6b.7 source-policy mutations over the installed graph.
//!
//! Order under the shared catalog operation gate:
//! 1. project the S0 change onto the current records (trust is preserved for
//!    kept identities; removed identities lose it);
//! 2. evaluate T0 `InstalledPolicy::source_changed` for every installed
//!    plugin artifact against that projection;
//! 3. durably clear the approvals T0 revoked (I0 records publication)
//!    BEFORE the source change, so no later load or restart can resurrect an
//!    approval whatever fails next (a failed source change leaves approvals
//!    conservatively cleared: the user re-approves);
//! 4. publish the S0 change;
//! 5. only then add daemon-lifetime pending-restart markers for revoked
//!    in-process plugins that were running. Markers never clear before the
//!    daemon restarts, including on re-trust.
use super::CatalogError;
use crate::catalog::{
    forges::CatalogFetchService,
    install::InstalledStore,
    sources::{SourceGeneration, SourceIdentity, SourceInput, SourceRecord},
    trust::{CurrentPolicy, InstalledPolicy, Revocation},
};
use std::{collections::BTreeSet, sync::Mutex};

pub(super) enum PolicyChange {
    Replace(Vec<SourceInput>),
    Trust(SourceIdentity, bool),
}

/// Daemon-lifetime A6b.7 marker: installed plugin ids whose in-process code
/// was revoked while running. Bounded by the installed-record limit.
#[derive(Debug, Default)]
pub struct PendingRestart(Mutex<BTreeSet<String>>);
impl PendingRestart {
    pub fn contains(&self, plugin: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(plugin)
    }
    pub fn list(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }
    fn extend(&self, plugins: impl IntoIterator<Item = String>) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(plugins);
    }
}

/// Effects of one source-policy mutation, for H0's status display.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevocationReport {
    pub generation: SourceGeneration,
    /// Approvals durably cleared (process plugins need a new approval).
    pub approvals_cleared: Vec<String>,
    /// Running in-process code now "revocation pending restart".
    pub pending_restart: Vec<String>,
    /// Installed process plugins whose current source authority was revoked.
    pub stop_process: Vec<String>,
}

fn project(previous: &[SourceRecord], change: &PolicyChange) -> Vec<SourceRecord> {
    match change {
        PolicyChange::Trust(identity, trusted) => previous
            .iter()
            .cloned()
            .map(|mut record| {
                if record.identity == *identity {
                    record.trusted = *trusted;
                }
                record
            })
            .collect(),
        PolicyChange::Replace(input) => previous
            .iter()
            .filter(|record| {
                input
                    .iter()
                    .any(|input| input.source == record.identity.as_str())
            })
            .cloned()
            .collect(),
    }
}

fn lowers_trust(previous: &[SourceRecord], projected: &[SourceRecord]) -> bool {
    previous.iter().any(|record| {
        record.trusted
            && !projected
                .iter()
                .any(|kept| kept.identity == record.identity && kept.trusted)
    })
}

pub(super) fn apply(
    fetch: &CatalogFetchService,
    installed: Option<&InstalledStore>,
    pending: &PendingRestart,
    live: &[String],
    change: PolicyChange,
) -> Result<RevocationReport, CatalogError> {
    let (generation, previous) = fetch.sources().snapshot()?;
    let projected = project(&previous, &change);
    let mut report = RevocationReport::default();
    let mut markers = Vec::new();
    match installed {
        Some(store) => {
            let before = CurrentPolicy {
                generation,
                sources: &previous,
            };
            let after = CurrentPolicy {
                generation,
                sources: &projected,
            };
            for record in store.snapshot().records {
                let Some(artifact) = record.artifact else {
                    continue;
                };
                // A process can be starting or in backoff without appearing
                // in the live providers list. Its restart must stop too.
                let running = artifact.runtime == vosu_plugin_api::Runtime::Process
                    || live.contains(&record.id);
                let mut policy = InstalledPolicy::new(artifact, None);
                match policy.source_changed(&before, &after, running) {
                    Revocation::None => continue,
                    Revocation::StopProcess => report.stop_process.push(record.id.clone()),
                    Revocation::PendingRestart => markers.push(record.id.clone()),
                }
                if record.approval.is_some() {
                    report.approvals_cleared.push(record.id);
                }
            }
            let cleared: Vec<&str> = report
                .approvals_cleared
                .iter()
                .map(String::as_str)
                .collect();
            store.clear_approvals(&cleared)?;
        }
        // Unreadable records cannot be revoked durably: refuse any change
        // that removes trust instead of risking a resurrected approval.
        None if lowers_trust(&previous, &projected) => return Err(CatalogError::Disabled),
        None => {}
    }
    report.generation = match change {
        PolicyChange::Replace(input) => fetch.sources().replace(&input)?,
        PolicyChange::Trust(identity, trusted) => fetch.sources().set_trust(&identity, trusted)?,
    };
    report.pending_restart.clone_from(&markers);
    pending.extend(markers);
    Ok(report)
}

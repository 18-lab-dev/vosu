//! Generation-scoped, contextual SDK ingestion; failed sources never retain last-good entries.
//!
//! Dashboard status mapping (all reasons are static/redacted):
//! | Failure | Display |
//! |---|---|
//! | Disabled | Source disabled |
//! | Unavailable | Not found, unavailable or no access; existence unconfirmed |
//! | CredentialRequired | Credential may be required; existence unconfirmed |
//! | CredentialRejected | Credential rejected or insufficient read access |
//! | CredentialStoreUnavailable | Repair unavailable credential store |
//! | MalformedRelease | Invalid release metadata or missing/ambiguous catalog asset |
//! | UnsafeAsset | Unsafe initial catalog asset origin |
//! | UndetectedForge | Could not detect forge type |
//! | InvalidCatalog | SDK catalog validation failed |
//! | Limit | Catalog/snapshot limit exceeded |
use super::{
    SourceCatalog, SourceGeneration, SourceIdentity, SourceRecord, SourceStore, validate_catalogs,
};
use semver::Version;
use std::{collections::HashMap, sync::Arc};
use vosu_plugin_api::{
    catalog::{Catalog, CatalogEntry, EntrySupport, MAX_CATALOG_BYTES},
    manifest::{NamespaceRevision, PackageKind, Registration},
};

pub const MAX_SNAPSHOT_CANDIDATES: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceFailure {
    #[error("source disabled")]
    Disabled,
    #[error("source unavailable")]
    Unavailable,
    #[error("invalid catalog")]
    InvalidCatalog,
    #[error("catalog snapshot limit exceeded")]
    Limit,
    #[error("credential may be required; source existence unconfirmed")]
    CredentialRequired,
    #[error("credential rejected or insufficient read access")]
    CredentialRejected,
    #[error("catalog credential store unavailable")]
    CredentialStoreUnavailable,
    #[error("invalid release metadata or ambiguous catalog asset")]
    MalformedRelease,
    #[error("unsafe initial catalog asset origin")]
    UnsafeAsset,
    #[error("could not detect forge type")]
    UndetectedForge,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("source snapshot is stale")]
    Stale,
    #[error("invalid source result set")]
    InvalidResults,
    #[error("source store unavailable")]
    Unavailable,
}
/// Raw bytes deliberately have no Debug/Serialize implementation.
pub struct FetchedCatalog {
    pub source: SourceIdentity,
    pub result: Result<Vec<u8>, SourceFailure>,
}
/// Capture before network work, including before obtaining credential leases.
pub struct SourceFetchTicket {
    generation: SourceGeneration,
    records: Vec<SourceRecord>,
}
impl SourceFetchTicket {
    pub fn generation(&self) -> SourceGeneration {
        self.generation
    }
    /// Includes disabled records for visible status; fetch only enabled records.
    pub fn sources(&self) -> &[SourceRecord] {
        &self.records
    }
}
pub struct SourceSnapshot {
    record: SourceRecord,
    catalog: Result<Catalog, SourceFailure>,
    support: Vec<EntrySupport>,
}
impl SourceSnapshot {
    pub fn source(&self) -> &SourceRecord {
        &self.record
    }
    pub fn catalog(&self) -> Result<&Catalog, SourceFailure> {
        self.catalog.as_ref().map_err(|e| *e)
    }
}
pub struct CandidateSnapshot {
    generation: SourceGeneration,
    sources: Vec<SourceSnapshot>,
}
pub struct SourceCandidate<'a> {
    pub source: &'a SourceRecord,
    pub catalog_index: usize,
    pub kind: PackageKind,
    pub entry: &'a CatalogEntry,
    pub support: EntrySupport,
}
impl CandidateSnapshot {
    pub fn generation(&self) -> SourceGeneration {
        self.generation
    }
    pub fn sources(&self) -> &[SourceSnapshot] {
        &self.sources
    }
    /// Preserves every alternative, including unsupported entries and shadowed IDs.
    pub fn candidates(&self) -> impl Iterator<Item = SourceCandidate<'_>> {
        self.sources.iter().flat_map(|source| {
            source
                .catalog
                .as_ref()
                .ok()
                .into_iter()
                .flat_map(move |catalog| {
                    catalog.entries().zip(&source.support).enumerate().map(
                        move |(catalog_index, ((kind, entry), &support))| SourceCandidate {
                            source: &source.record,
                            catalog_index,
                            kind,
                            entry,
                            support,
                        },
                    )
                })
        })
    }
}
impl SourceStore {
    pub fn begin_fetch(&self) -> Result<SourceFetchTicket, SnapshotError> {
        let (generation, records) = self.snapshot().map_err(|_| SnapshotError::Unavailable)?;
        Ok(SourceFetchTicket {
            generation,
            records,
        })
    }
    pub fn candidate_snapshot(&self) -> Result<Option<Arc<CandidateSnapshot>>, SnapshotError> {
        let state = self.state.lock().map_err(|_| SnapshotError::Unavailable)?;
        if !state.available {
            return Err(SnapshotError::Unavailable);
        }
        Ok(state.candidates.clone())
    }
    /// Contextual validation occurs once against the complete successful enabled set,
    /// outside the source lock. Final publication compares generations under that lock.
    pub fn complete_fetch(
        &self,
        ticket: SourceFetchTicket,
        fetched: Vec<FetchedCatalog>,
        installed_and_bundled: &[Registration<'_>],
        revisions: &[NamespaceRevision<'_>],
        application: &Version,
        target: &str,
    ) -> Result<Arc<CandidateSnapshot>, SnapshotError> {
        if fetched.len() > ticket.records.len() {
            return Err(SnapshotError::InvalidResults);
        }
        let mut results = HashMap::new();
        for item in fetched {
            if !ticket
                .records
                .iter()
                .any(|r| r.enabled && r.identity == item.source)
                || results.insert(item.source, item.result).is_some()
            {
                return Err(SnapshotError::InvalidResults);
            }
        }
        let mut count = 0;
        let mut sources: Vec<_> = ticket
            .records
            .into_iter()
            .map(|record| {
                if !record.enabled {
                    return SourceSnapshot {
                        record,
                        catalog: Err(SourceFailure::Disabled),
                        support: Vec::new(),
                    };
                }
                let catalog = results
                    .remove(&record.identity)
                    .unwrap_or(Err(SourceFailure::Unavailable))
                    .and_then(|bytes| {
                        if bytes.len() > MAX_CATALOG_BYTES {
                            return Err(SourceFailure::Limit);
                        }
                        let text = std::str::from_utf8(&bytes)
                            .map_err(|_| SourceFailure::InvalidCatalog)?;
                        let catalog =
                            Catalog::parse(text).map_err(|_| SourceFailure::InvalidCatalog)?;
                        count += catalog.entries().count();
                        if count > MAX_SNAPSHOT_CANDIDATES {
                            return Err(SourceFailure::Limit);
                        }
                        Ok(catalog)
                    });
                SourceSnapshot {
                    record,
                    catalog,
                    support: Vec::new(),
                }
            })
            .collect();
        let failures: Vec<_> = {
            let catalogs: Vec<_> = sources
                .iter()
                .filter_map(|source| {
                    source.catalog.as_ref().ok().map(|catalog| SourceCatalog {
                        source: &source.record,
                        catalog,
                    })
                })
                .collect();
            validate_catalogs(&catalogs, installed_and_bundled, revisions, application)
                .into_iter()
                .filter_map(|(id, result)| result.err().map(|_| id.clone()))
                .collect()
        };
        for source in &mut sources {
            if failures.contains(&source.record.identity) {
                source.catalog = Err(SourceFailure::InvalidCatalog);
            } else if let Ok(catalog) = &source.catalog {
                source.support = catalog
                    .entries()
                    .map(|(kind, entry)| {
                        entry
                            .support(kind, application, target)
                            .expect("SDK-validated entry")
                    })
                    .collect();
            }
        }
        let snapshot = Arc::new(CandidateSnapshot {
            generation: ticket.generation,
            sources,
        });
        let mut state = self.state.lock().map_err(|_| SnapshotError::Unavailable)?;
        if !state.available || state.generation != ticket.generation {
            return Err(SnapshotError::Stale);
        }
        state.candidates = Some(snapshot.clone());
        Ok(snapshot)
    }
}

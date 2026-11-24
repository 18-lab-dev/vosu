//! Compiles offerable catalog game entries with the runtime matcher engine.
use crate::{
    catalog::sources::{CandidateSnapshot, SourceIdentity},
    plugin::{
        CandidateProcess, CompiledMatcher, MatchPlatform, OBSERVER_PREFILTER_BUDGET, Prefilter,
    },
};
use std::collections::HashMap;
use vosu_plugin_api::{MatchTable, manifest::PackageKind};

/// One offer target: the exact source identity and plugin id the accepted
/// request names. Version alternatives of one id in one source collapse.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct CandidateKey {
    pub source: SourceIdentity,
    pub id: String,
}

pub(super) struct GameCandidate {
    pub key: CandidateKey,
    /// Display metadata of the first matching-id entry in catalog order.
    pub name: String,
    pub version: String,
    /// Every alternative entry's validated table and runtime matcher.
    matchers: Vec<(MatchTable, CompiledMatcher)>,
}

impl GameCandidate {
    pub fn matches(&self, process: &CandidateProcess) -> bool {
        self.matchers
            .iter()
            .any(|(_, matcher)| matcher.matches(process))
    }
}

#[derive(Default)]
pub(super) struct CompiledCatalog {
    pub candidates: Vec<GameCandidate>,
    /// Bounded extra discovery hint names (never an unfiltered request).
    pub prefilter_names: Vec<String>,
}

/// Pure and bounded by S0's `MAX_SNAPSHOT_CANDIDATES`; never touches the
/// network, filesystem or plugin host.
pub(super) fn compile(
    platform: MatchPlatform,
    snapshot: Option<&CandidateSnapshot>,
    installed: &[&str],
) -> CompiledCatalog {
    let mut candidates: Vec<GameCandidate> = Vec::new();
    let mut index: HashMap<CandidateKey, usize> = HashMap::new();
    for candidate in snapshot.into_iter().flat_map(CandidateSnapshot::candidates) {
        let Some(table) = candidate.entry.matcher.as_ref() else {
            continue;
        };
        if candidate.kind != PackageKind::Plugin
            || !candidate.source.enabled
            || !candidate.support.supported()
            || installed.contains(&candidate.entry.id.as_str())
        {
            continue;
        }
        // The SDK already validated this table with the runtime's anchored
        // regex policy; a failure here means no offer, never a looser matcher.
        let Ok(matcher) = CompiledMatcher::compile(platform, table) else {
            tracing::warn!(plugin = %candidate.entry.id, "catalog game matcher failed to compile");
            continue;
        };
        let key = CandidateKey {
            source: candidate.source.identity.clone(),
            id: candidate.entry.id.clone(),
        };
        match index.get(&key) {
            Some(&existing) => candidates[existing].matchers.push((table.clone(), matcher)),
            None => {
                index.insert(key.clone(), candidates.len());
                candidates.push(GameCandidate {
                    key,
                    name: candidate.entry.name.clone(),
                    version: candidate.entry.version.clone(),
                    matchers: vec![(table.clone(), matcher)],
                });
            }
        }
    }
    // Path/command-line-only entries and entries past the separate catalog
    // budget add no interest: remote content never widens the pre-filter.
    let prefilter_names = Prefilter::hint_names(
        platform,
        candidates
            .iter()
            .flat_map(|candidate| candidate.matchers.iter())
            .map(|(table, _)| table),
        OBSERVER_PREFILTER_BUDGET,
    );
    CompiledCatalog {
        candidates,
        prefilter_names,
    }
}

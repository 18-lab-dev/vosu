//! Bounded, generation-scoped offer registry and requested-plan acceptance.
use super::candidates::{CandidateKey, CompiledCatalog, compile};
use crate::{
    catalog::{
        plans::{self, PlanError, PlanRegistry, PlanView, Prerequisites},
        resolver::{Request, Snapshot},
        sources::{CandidateSnapshot, SourceGeneration, SourceIdentity},
        trust::CurrentPolicy,
    },
    plugin::{CandidateProcess, DiscoveryObserver, MatchPlatform, Prefilter, ProcessRecord},
};
use serde::Serialize;
use std::sync::{Arc, Mutex, MutexGuard};
use vosu_plugin_api::manifest::PackageKind;

/// Live offers across every process and candidate. Excess matches are not
/// offered until an existing offer is invalidated (never evicted silently).
pub const MAX_GAME_OFFERS: usize = 64;

/// Redacted management view: no executable path, argv or command line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GameOfferView {
    pub offer_id: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: SourceIdentity,
    /// The observed executable/`comm` name only.
    pub process: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OfferError {
    #[error("unknown or expired game offer")]
    NotFound,
    #[error("game offer inputs are stale")]
    Stale,
    #[error(transparent)]
    Plan(#[from] PlanError),
}

impl OfferError {
    pub fn status_code(self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::Stale => 409,
            Self::Plan(error) => error.status_code(),
        }
    }
}

/// Installed plugin ids (including bundled ones) and the installed generation
/// P0 pins as `Prerequisites::installed_generation`.
pub struct InstalledPlugins<'a> {
    pub generation: u64,
    pub ids: &'a [&'a str],
}

/// The composition-supplied, already-current P0 inputs for one acceptance.
pub struct PlanInputs<'s, 'a> {
    pub snapshot: &'s Snapshot<'a>,
    pub policy: &'s CurrentPolicy<'s>,
    pub prerequisites: Prerequisites,
    pub registry: &'s PlanRegistry,
}

struct Offer {
    id: String,
    candidate: usize,
    pid: u32,
    start_time: u64,
    process: String,
}

#[derive(Default)]
struct State {
    source_generation: SourceGeneration,
    installed_generation: u64,
    snapshot: Option<Arc<CandidateSnapshot>>,
    catalog: Arc<CompiledCatalog>,
    /// Observer generation: advances on every catalog publication.
    epoch: u64,
    offers: Vec<Offer>,
    next_offer: u64,
    saturated: bool,
}

/// Catalog game-offer service; see the module documentation.
pub struct GameOffers {
    platform: MatchPlatform,
    state: Mutex<State>,
}

impl GameOffers {
    #[must_use]
    pub fn new(platform: MatchPlatform) -> Self {
        Self {
            platform,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Publishes the current candidate snapshot (`None` after an S0 source
    /// mutation cleared it) and installed set. Older generations are refused
    /// as stale; an identical publication is a no-op. Any other publication
    /// clears every offer and advances the observer generation. Matchers are
    /// compiled outside the state lock.
    pub fn refresh(
        &self,
        catalog: Option<Arc<CandidateSnapshot>>,
        source_generation: SourceGeneration,
        installed: &InstalledPlugins<'_>,
    ) -> Result<(), OfferError> {
        if catalog
            .as_ref()
            .is_some_and(|snapshot| snapshot.generation() != source_generation)
        {
            return Err(OfferError::Stale);
        }
        if !self.publication_needed(catalog.as_ref(), source_generation, installed)? {
            return Ok(());
        }
        let compiled = Arc::new(compile(self.platform, catalog.as_deref(), installed.ids));
        let mut state = self.lock();
        if !needs_publication(&state, catalog.as_ref(), source_generation, installed)? {
            return Ok(());
        }
        state.source_generation = source_generation;
        state.installed_generation = installed.generation;
        state.snapshot = catalog;
        state.catalog = compiled;
        state.offers.clear();
        state.saturated = false;
        state.epoch += 1;
        Ok(())
    }

    fn publication_needed(
        &self,
        catalog: Option<&Arc<CandidateSnapshot>>,
        source_generation: SourceGeneration,
        installed: &InstalledPlugins<'_>,
    ) -> Result<bool, OfferError> {
        needs_publication(&self.lock(), catalog, source_generation, installed)
    }

    /// Current offers, oldest first.
    #[must_use]
    pub fn offers(&self) -> Vec<GameOfferView> {
        let state = self.lock();
        state
            .offers
            .iter()
            .map(|offer| {
                let candidate = &state.catalog.candidates[offer.candidate];
                GameOfferView {
                    offer_id: offer.id.clone(),
                    id: candidate.key.id.clone(),
                    name: candidate.name.clone(),
                    version: candidate.version.clone(),
                    source: candidate.key.source.clone(),
                    process: offer.process.clone(),
                }
            })
            .collect()
    }

    /// The user's click: builds and registers one normal P0 plan whose only
    /// request is this offer's `(plugin, id, exact source)`, so the add is
    /// `reason: "requested"`. Never confirms, approves, trusts or downloads;
    /// the offer stays live until install/source/process invalidation.
    pub fn accept(
        &self,
        offer_id: &str,
        inputs: PlanInputs<'_, '_>,
    ) -> Result<PlanView, OfferError> {
        let (key, source_generation, installed_generation) = {
            let state = self.lock();
            let offer = state
                .offers
                .iter()
                .find(|offer| offer.id == offer_id)
                .ok_or(OfferError::NotFound)?;
            (
                state.catalog.candidates[offer.candidate].key.clone(),
                state.source_generation,
                state.installed_generation,
            )
        };
        if inputs.policy.generation != source_generation
            || inputs.prerequisites.source_generation != source_generation.0
            || inputs.prerequisites.installed_generation != installed_generation
        {
            return Err(OfferError::Stale);
        }
        let CandidateKey { source, id } = &key;
        let plan = plans::build(
            inputs.snapshot,
            &[Request {
                kind: PackageKind::Plugin,
                id,
                source,
            }],
            inputs.policy,
            inputs.prerequisites,
        );
        Ok(inputs.registry.insert(plan)?)
    }
}

fn needs_publication(
    state: &State,
    catalog: Option<&Arc<CandidateSnapshot>>,
    source_generation: SourceGeneration,
    installed: &InstalledPlugins<'_>,
) -> Result<bool, OfferError> {
    if source_generation.0 < state.source_generation.0
        || installed.generation < state.installed_generation
    {
        return Err(OfferError::Stale);
    }
    let same_snapshot = match (catalog, state.snapshot.as_ref()) {
        (Some(new), Some(old)) => Arc::ptr_eq(new, old),
        (None, None) => true,
        _ => false,
    };
    Ok(!(same_snapshot
        && source_generation == state.source_generation
        && installed.generation == state.installed_generation
        && state.epoch != 0))
}

impl DiscoveryObserver for GameOffers {
    fn process_observed(&self, process: &ProcessRecord, claimed: bool) {
        let (catalog, epoch) = {
            let mut state = self.lock();
            // A reused pid is a different occurrence: its old offers go first.
            state.offers.retain(|offer| {
                offer.pid != process.pid || (offer.start_time == process.start_time && !claimed)
            });
            if claimed {
                return;
            }
            (Arc::clone(&state.catalog), state.epoch)
        };
        let candidate = CandidateProcess {
            name: process.name.clone(),
            path: process.path.clone(),
            command_line: process.command_line.clone(),
        };
        let matched: Vec<usize> = catalog
            .candidates
            .iter()
            .enumerate()
            .filter(|(_, game)| game.matches(&candidate))
            .map(|(index, _)| index)
            .collect();
        let mut state = self.lock();
        if state.epoch != epoch {
            // A newer publication re-offers this process through discovery.
            return;
        }
        state.offers.retain(|offer| {
            offer.pid != process.pid
                || offer.start_time != process.start_time
                || matched.contains(&offer.candidate)
        });
        for index in matched {
            let present = state.offers.iter().any(|offer| {
                offer.pid == process.pid
                    && offer.start_time == process.start_time
                    && offer.candidate == index
            });
            if present {
                continue;
            }
            if state.offers.len() >= MAX_GAME_OFFERS {
                if !state.saturated {
                    state.saturated = true;
                    tracing::warn!("catalog game offer limit reached; further matches not offered");
                }
                break;
            }
            state.next_offer += 1;
            let id = format!("{:016x}", state.next_offer);
            state.offers.push(Offer {
                id,
                candidate: index,
                pid: process.pid,
                start_time: process.start_time,
                process: process.name.clone(),
            });
        }
    }

    fn process_exited(&self, pid: u32) {
        let mut state = self.lock();
        state.offers.retain(|offer| offer.pid != pid);
        if state.offers.len() < MAX_GAME_OFFERS {
            state.saturated = false;
        }
    }

    fn prefilter(&self) -> Option<Prefilter> {
        self.lock().catalog.prefilter.clone()
    }

    fn generation(&self) -> u64 {
        self.lock().epoch
    }
}

//! osu! publication infrastructure. Pointer names are namespace prefix plus
//! PascalCase schema path: `OsuBeatmapTimeLive` is `vosu.osu/beatmap/time/live`;
//! core namespaces omit `Osu`. Arrays and infrequently changing groups are whole
//! values. `dirty` tracks source dependencies before the diff sink encodes them.
mod beatmap;
mod common;
mod dirty;
mod internal;
mod keys;
mod leaderboard;
mod performance;
mod play;
mod pointers;
mod profile;
mod result;
mod room;
mod session;
mod settings;
mod sink;
mod skin;
mod status_map;
mod tourney;
pub use pointers::{OsuPointer, Source};
pub use sink::{DiffSink, LastValues, ProjectionSink};
use std::sync::Arc;
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishBatchWriter;

pub struct SessionFacts {
    pub client: ClientType,
    pub settings: InstanceSettings,
    pub foreground: Option<bool>,
}

pub type Projection = fn(&StateBundleSnapshot, &SessionFacts, &mut dyn ProjectionSink);
// Each projection unit extends this list without changing publication mechanics.
const PROJECTIONS: &[Projection] = &[common::project, play::project, settings::project_all];
#[derive(Default)]
/// Session-owned caches for stateful projections, retained across samples.
pub struct Caches {
    skin: skin::SkinCache,
    tourney: tourney::AvailabilityCache,
}
impl Caches {
    pub(crate) fn take_error(&mut self) -> Option<std::io::Error> {
        self.skin.take_error()
    }
    pub(crate) fn take_tourney_unavailable(&mut self) -> bool {
        self.tourney.take_transition()
    }
}
pub(crate) fn project_stateful(
    caches: &mut Caches,
    facts: &SessionFacts,
    snapshot: &StateBundleSnapshot,
    sink: &mut dyn ProjectionSink,
) {
    if sink.wants(OsuPointer::OsuSkinBuiltin) {
        caches.skin.project(facts, snapshot, sink);
    }
    if sink.wants(OsuPointer::OsuTourney) {
        caches.tourney.update(snapshot, facts);
    }
}

#[derive(Default)]
pub struct Projector {
    pub(crate) last_snapshot: Option<Arc<StateBundleSnapshot>>,
    last: LastValues,
    // None means cold/invalidated: every pointer must be visited on retry.
    dirty: Option<dirty::Dirty>,
}
impl Projector {
    pub fn project(
        &mut self,
        snap: &StateBundleSnapshot,
        facts: &SessionFacts,
        caches: &mut Caches,
        sink: &mut dyn ProjectionSink,
    ) {
        for project in PROJECTIONS {
            project(snap, facts, sink);
        }
        project_stateful(caches, facts, snap, sink);
    }
    pub(crate) fn project_into(
        &mut self,
        snap: &StateBundleSnapshot,
        facts: &SessionFacts,
        writer: &mut PublishBatchWriter,
        projections: &[Projection],
        caches: &mut Caches,
    ) {
        let dirty = self.dirty.unwrap_or(dirty::Dirty::ALL);
        let mut sink = DiffSink::filtered(&mut self.last, writer, dirty);
        for project in projections {
            project(snap, facts, &mut sink);
        }
        project_stateful(caches, facts, snap, &mut sink);
        self.dirty = Some(dirty::Dirty::ALL);
    }
    // Compare before the publisher replaces last_snapshot with its attempt.
    pub(crate) fn prepare(&mut self, snap: &StateBundleSnapshot) {
        if self.dirty.is_some() {
            self.dirty = Some(dirty::Dirty::between(self.last_snapshot.as_deref(), snap));
        }
    }
    pub(crate) fn invalidate(&mut self) {
        self.last.invalidate();
        self.dirty = None;
    }
}
pub(crate) fn projections() -> &'static [Projection] {
    PROJECTIONS
}

#[cfg(test)]
#[path = "tests/o5_parity.rs"]
mod o5_parity;
#[cfg(test)]
#[path = "tests/o5.rs"]
mod o5_tests;
#[cfg(test)]
#[path = "tests/skin_paths.rs"]
mod skin_paths_tests;
#[cfg(test)]
#[path = "tests/skin.rs"]
mod skin_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "tests/tourney.rs"]
mod tourney_tests;

//! osu! publication infrastructure. Pointer names are namespace prefix plus
//! PascalCase schema path: `OsuBeatmapTimeLive` is `vosu.osu/beatmap/time/live`;
//! core namespaces omit `Osu`. Arrays and infrequently changing groups are whole
//! values. O3–O5 add their modules and one function each to `PROJECTIONS`.
mod pointers;
mod sink;
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
const PROJECTIONS: &[Projection] = &[];

#[derive(Default)]
pub struct Projector {
    pub(crate) last_snapshot: Option<Arc<StateBundleSnapshot>>,
    last: LastValues,
}
impl Projector {
    pub fn project(
        &mut self,
        snap: &StateBundleSnapshot,
        facts: &SessionFacts,
        sink: &mut dyn ProjectionSink,
    ) {
        for project in PROJECTIONS {
            project(snap, facts, sink);
        }
    }
    pub(crate) fn project_into(
        &mut self,
        snap: &StateBundleSnapshot,
        facts: &SessionFacts,
        writer: &mut PublishBatchWriter,
        projections: &[Projection],
    ) {
        let mut sink = DiffSink::new(&mut self.last, writer);
        for project in projections {
            project(snap, facts, &mut sink);
        }
    }
    pub(crate) fn invalidate(&mut self) {
        self.last.invalidate();
    }
}
pub(crate) fn projections() -> &'static [Projection] {
    PROJECTIONS
}

#[cfg(test)]
mod tests;

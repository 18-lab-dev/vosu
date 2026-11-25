//! Instance-owned work counters around a real adapter projection. Every call is
//! forwarded unchanged; the counters only prove how much work actually ran.
use bytes::Bytes;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vosu_adapter_support::filtered::Filter;
use vosu_plugin_api::{Selector, StateEntry};
use vosu_surface::{
    projection::{Projection, ProjectionError, ProjectionView},
    state::CapturedSelection,
};

type Adapter = dyn Projection<Filter, Bytes>;
type View = dyn ProjectionView<Filter, Bytes>;

#[derive(Default)]
pub struct Counts {
    decodes: AtomicUsize,
    unfiltered: AtomicUsize,
    filtered: AtomicUsize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub decodes: usize,
    pub unfiltered: usize,
    pub filtered: usize,
}
impl std::ops::Sub for Work {
    type Output = Self;
    fn sub(self, before: Self) -> Self {
        Self {
            decodes: self.decodes - before.decodes,
            unfiltered: self.unfiltered - before.unfiltered,
            filtered: self.filtered - before.filtered,
        }
    }
}
impl Counts {
    pub fn work(&self) -> Work {
        Work {
            decodes: self.decodes.load(Ordering::Relaxed),
            unfiltered: self.unfiltered.load(Ordering::Relaxed),
            filtered: self.filtered.load(Ordering::Relaxed),
        }
    }
}

pub struct Counted {
    inner: Arc<Adapter>,
    pub counts: Arc<Counts>,
}
impl Counted {
    pub fn new(inner: Arc<Adapter>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            counts: Arc::default(),
        })
    }
}
struct CountedView {
    inner: Arc<View>,
    counts: Arc<Counts>,
}
impl Projection<Filter, Bytes> for Counted {
    fn observes_field(&self, entry: &StateEntry, field: &str) -> bool {
        self.inner.observes_field(entry, field)
    }
    fn selectors(&self) -> Vec<Selector> {
        self.inner.selectors()
    }
    fn demand_selectors(&self, filters: &[Filter]) -> Vec<Selector> {
        self.inner.demand_selectors(filters)
    }
    fn decode(&self, capture: CapturedSelection) -> Result<Arc<View>, ProjectionError> {
        self.counts.decodes.fetch_add(1, Ordering::Relaxed);
        let inner = self.inner.decode(capture)?;
        Ok(Arc::new(CountedView {
            inner,
            counts: Arc::clone(&self.counts),
        }))
    }
    fn revision(&self) -> u64 {
        self.inner.revision()
    }
    fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.inner.changes()
    }
    fn retain(&self, filters: &[Filter]) -> Option<Box<dyn Send + Sync>> {
        self.inner.retain(filters)
    }
}
impl ProjectionView<Filter, Bytes> for CountedView {
    fn revision(&self) -> u64 {
        self.inner.revision()
    }
    fn encode(&self, filters: &[Filter]) -> Result<Bytes, String> {
        let counter = if filters.is_empty() {
            &self.counts.unfiltered
        } else {
            &self.counts.filtered
        };
        counter.fetch_add(1, Ordering::Relaxed);
        self.inner.encode(filters)
    }
}

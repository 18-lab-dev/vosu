//! Production bus-backed provider. The adapter owns selectors, typed decoding and
//! wire semantics; this module owns latest-only caches and connection demand.
mod demand;
mod identity;
use super::{
    routes::{CachedJson, StateJsonProvider, StateRoute},
    websocket::{Schema, StateDemand, StateProvider, StateSnapshot},
};
use bytes::Bytes;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::watch;
use vosu_surface::filter::Filter;
use vosu_surface::{
    projection::{Projection, ProjectionError, ProjectionView},
    requester::BundledRequester,
    state::{CapturedSelection, SessionTarget, SurfaceSubscription},
};

type Adapter = dyn Projection<Filter, Bytes>;
type View = dyn ProjectionView<Filter, Bytes>;
/// Longest an HTTP read waits for retained derived work (the streamCompanion
/// chart job) already in flight for its exact inputs, before falling back to
/// the pinned chart-less bytes. 1.x always served chart facts, so one-shot
/// readers must not race their own release. Measured 2026-11-24, release
/// build, Ryzen 9 9950X3D pinned to CPUs 0-7 (non-exclusive), 60 cold
/// `BeatmapCache::load` + projection runs each: oracle fixture chart p99
/// 0.23 ms; 33 KB `2785319.osu` p99 4.62 ms; 243 KB / 7,052-line
/// "The Unforgiving \[Marathon\]" p99 40.08 ms. Bound = 2 × the marathon p99,
/// well under the 500 ms cap (docs/testing/benchmarks.md, M4a section).
pub const DERIVED_HTTP_WAIT: Duration = Duration::from_millis(80);

pub struct BusStateProvider {
    schema: Schema,
    requester: Arc<dyn BundledRequester>,
    adapter: Arc<Adapter>,
    selectors: Vec<vosu_plugin_api::Selector>,
    cadence: Duration,
    events: watch::Sender<u64>,
    latest: Mutex<Latest>,
    demand: Arc<demand::Registry>,
}
#[derive(Default)]
struct Latest {
    generation: u64,
    snapshot: Option<Arc<Snapshot>>,
}
struct Snapshot {
    capture: CapturedSelection,
    derived: u64,
    view: Result<Arc<View>, ProjectionError>,
    generation: u64,
    unfiltered: OnceLock<Result<Bytes, String>>,
    http: OnceLock<CachedJson>,
}
impl StateSnapshot for Snapshot {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.unfiltered
            .get_or_init(|| self.view.as_ref().map_err(ToString::to_string)?.encode(&[]))
            .clone()
    }
    fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String> {
        self.view
            .as_ref()
            .map_err(ToString::to_string)?
            .encode(filters)
    }
}
impl Snapshot {
    /// The production HTTP result of exactly this snapshot, shared by the state
    /// route and the test-harness capture.
    fn http(&self) -> CachedJson {
        self.http
            .get_or_init(|| {
                if self.capture.session.is_none()
                    || matches!(self.view, Err(ProjectionError::NotReady))
                {
                    return CachedJson::NotReady;
                }
                match self.encode_unfiltered() {
                    Ok(bytes) => CachedJson::Ready(bytes),
                    Err(error) => failed(error),
                }
            })
            .clone()
    }
}
impl BusStateProvider {
    pub fn new(
        schema: Schema,
        requester: Arc<dyn BundledRequester>,
        adapter: Arc<Adapter>,
        cadence: Duration,
    ) -> Self {
        let selectors = adapter.selectors();
        let events = watch::channel(0).0;
        let demand = demand::Registry::new(adapter.clone(), requester.clone(), events.clone());
        Self {
            schema,
            requester,
            adapter,
            cadence,
            events,
            demand,
            latest: Mutex::new(Latest::default()),
            selectors,
        }
    }
    fn snapshot(&self) -> Result<Arc<Snapshot>, String> {
        // Serialize captures as well as replacements: a delayed old capture must
        // not evict a newer one. This is not a host/state-bus lock.
        let mut latest = self.latest.lock().unwrap();
        let capture = self
            .requester
            .capture(SessionTarget::Focused, &self.selectors)
            .map_err(|e| e.to_string())?;
        let derived = self.adapter.revision();
        if let Some(snapshot) = &latest.snapshot
            && snapshot.derived == derived
            && same_inputs(self.adapter.as_ref(), &snapshot.capture, &capture)
        {
            return Ok(Arc::clone(snapshot));
        }
        latest.generation = latest.generation.wrapping_add(1);
        let view = self.adapter.decode(capture.clone());
        let derived = view.as_ref().map_or(derived, |view| view.revision());
        let snapshot = Arc::new(Snapshot {
            view,
            capture,
            derived,
            generation: latest.generation,
            unfiltered: OnceLock::new(),
            http: OnceLock::new(),
        });
        latest.snapshot = Some(Arc::clone(&snapshot));
        Ok(snapshot)
    }
    /// The exact selection this provider's adapter decoded for one snapshot,
    /// with the production HTTP result of that same snapshot. Derived work is
    /// retained exactly as by a one-shot HTTP read, but never awaited: there is
    /// one `snapshot()` call, no second capture, subscription, sampling demand
    /// or retained history. Envelope encoding happens later, outside the
    /// provider lock.
    #[cfg(feature = "test-harness")]
    pub fn capture_for_harness(
        &self,
    ) -> Result<super::adapter_capture::AdapterCaptureSample, String> {
        use super::adapter_capture::{AdapterCaptureSample, DerivedStatus};
        let _retained = self.adapter.retain(&[]);
        let snapshot = self.snapshot()?;
        let http = snapshot.http();
        let derived = if self.adapter.changes().is_none() {
            DerivedStatus::None
        } else if self.adapter.pending() || self.adapter.revision() != snapshot.derived {
            DerivedStatus::Pending
        } else {
            DerivedStatus::Ready
        };
        Ok(AdapterCaptureSample {
            schema: self.schema,
            provider_generation: snapshot.generation,
            capture: snapshot.capture.clone(),
            derived_revision: snapshot.derived,
            derived,
            http,
        })
    }
}
fn same_inputs(adapter: &Adapter, left: &CapturedSelection, right: &CapturedSelection) -> bool {
    // Namespace revisions are too broad: precise-only publications must not
    // discard a regular view. Compare exactly the authorized selected image.
    left.session == right.session
        && left.focus_revision == right.focus_revision
        && left.roster == right.roster
        && (Arc::ptr_eq(&left.values, &right.values)
            || identity::same_entries(&left.values, &right.values, |entry, field| {
                adapter.observes_field(entry, field)
            }))
}

impl StateProvider for BusStateProvider {
    fn poll_rate(&self, _: Schema) -> Duration {
        self.cadence
    }
    fn subscribe(&self, _: Schema) -> watch::Receiver<u64> {
        self.events.subscribe()
    }
    fn capture(&self, schema: Schema) -> Option<Arc<dyn StateSnapshot>> {
        if schema != self.schema {
            return None;
        }
        self.snapshot()
            .ok()
            .map(|snapshot| snapshot as Arc<dyn StateSnapshot>)
    }
    fn demand(
        &self,
        schema: Schema,
        filters: &[Filter],
    ) -> Result<Option<Box<dyn StateDemand>>, String> {
        if schema != self.schema {
            return Err("provider schema mismatch".into());
        }
        self.demand.acquire(filters).map(Some)
    }
}
impl StateJsonProvider for BusStateProvider {
    fn cached_json(&self, route: StateRoute) -> super::routes::ProviderFuture<'_, CachedJson> {
        Box::pin(async move {
            let _retained = self.adapter.retain(&[]);
            let schema = match route {
                StateRoute::V1 => Schema::V1,
                StateRoute::V2 => Schema::V2,
                StateRoute::V2Precise => Schema::V2Precise,
                StateRoute::StreamCompanion => Schema::StreamCompanion,
            };
            if schema != self.schema {
                return CachedJson::NotReady;
            }
            // Subscribe before checking, so a completion cannot slip between.
            let changes = self.adapter.changes();
            let mut snapshot = self.snapshot();
            if let Some(mut changes) = changes
                && snapshot.is_ok()
            {
                if self.adapter.pending() {
                    let _ = tokio::time::timeout(DERIVED_HTTP_WAIT, async {
                        while self.adapter.pending() {
                            if changes.changed().await.is_err() {
                                break;
                            }
                        }
                    })
                    .await;
                }
                // Work may also have completed between decode and the check.
                if snapshot
                    .as_ref()
                    .is_ok_and(|current| current.derived != self.adapter.revision())
                {
                    snapshot = self.snapshot();
                }
            }
            match snapshot {
                Ok(snapshot) => snapshot.http(),
                Err(error) => failed(error),
            }
        })
    }
}
fn failed(error: String) -> CachedJson {
    CachedJson::Failed {
        status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        body: Bytes::from(
            serde_json::to_vec(&serde_json::json!({"error": error}))
                .expect("error JSON serializes"),
        ),
    }
}

#[cfg(test)]
mod tests;

//! Unmounted streamCompanion provider with consumer-owned chart work.
use crate::{
    derived::{ChartKey, ChartWorker, Counts, DerivedChart},
    view::View,
};
use bytes::Bytes;
use std::sync::{Arc, Mutex, OnceLock};
use vosu_adapter_support::filtered::{Filter, SharedDocument, selects_path};
use vosu_plugin_api::Selector;
use vosu_surface::{
    projection::{Projection, ProjectionError, ProjectionView},
    requester::BundledRequester,
    state::CapturedSelection,
};

pub struct Provider {
    state: Arc<Mutex<State>>,
}
struct State {
    worker: Option<ChartWorker>,
    dependents: usize,
    last: Option<Arc<View>>,
    key: Option<ChartKey>,
    chart: Option<Arc<DerivedChart>>,
    revision: u64,
}
impl State {
    fn refresh(&mut self) -> Option<Arc<DerivedChart>> {
        let chart = self
            .key
            .as_ref()
            .and_then(|key| self.worker.as_ref()?.current_for(key));
        let same = match (&chart, &self.chart) {
            (None, None) => true,
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            _ => false,
        };
        if !same {
            self.revision = self.revision.wrapping_add(1);
            self.chart = chart;
        }
        self.chart.clone()
    }
}
impl Provider {
    pub fn new(requester: Arc<dyn BundledRequester>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                worker: Some(ChartWorker::new(requester)),
                dependents: 0,
                last: None,
                key: None,
                chart: None,
                revision: 0,
            })),
        }
    }
    /// Composition must await this before dropping the provider.
    pub async fn shutdown(&self) -> Result<Counts, crate::Error> {
        let worker = {
            let mut state = self.state.lock().unwrap();
            state.key = None;
            state.chart = None;
            state.last = None;
            state.revision = state.revision.wrapping_add(1);
            state.worker.take()
        };
        match worker {
            Some(worker) => worker.shutdown().await,
            None => Ok(Counts::default()),
        }
    }
    pub fn counts(&self) -> Counts {
        self.state
            .lock()
            .unwrap()
            .worker
            .as_ref()
            .map_or(Counts::default(), ChartWorker::counts)
    }
}
struct Retained(Arc<Mutex<State>>);
impl Drop for Retained {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.dependents -= 1;
        if state.dependents == 0 {
            if let Some(view) = state.last.clone()
                && let Some(worker) = state.worker.as_mut()
            {
                let _ = worker.select(&view, false);
            }
            state.key = None;
            state.refresh();
        }
    }
}
const CHART_FIELDS: &[&str] = &[
    "mStars",
    "ar",
    "mAR",
    "od",
    "mOD",
    "cs",
    "mCS",
    "hp",
    "mHP",
    "bpm",
    "mainBpm",
    "maxBpm",
    "minBpm",
    "mMainBpm",
    "mMaxBpm",
    "mMinBpm",
    "mBpm",
    "previewtime",
    "totaltime",
    "timeLeft",
    "drainingtime",
    "firstHitObjectTime",
    "mapStrains",
    "mapBreaks",
    "mapTimingPoints",
    "sliders",
    "circles",
    "spinners",
    "maxCombo",
];
fn needs_chart(filters: &[Filter]) -> bool {
    CHART_FIELDS
        .iter()
        .any(|field| selects_path(filters, &[field]))
}
impl Projection<Filter, Bytes> for Provider {
    fn selectors(&self) -> Vec<Selector> {
        crate::view::selectors()
    }
    fn demand_selectors(&self, filters: &[Filter]) -> Vec<Selector> {
        self.selectors()
            .into_iter()
            .filter(|selector| match selector.pointer.as_str() {
                "/keys/overlay" => selects_path(filters, &["keyOverlay"]),
                "/play/hitErrors/all" => selects_path(filters, &["hitErrors"]),
                "/play/preciseUnstableRate" => {
                    selects_path(filters, &["unstableRate"])
                        || selects_path(filters, &["convertedUnstableRate"])
                }
                "/resources/file" => needs_chart(filters),
                _ => true,
            })
            .collect()
    }
    fn retain(&self, filters: &[Filter]) -> Option<Box<dyn Send + Sync>> {
        if !needs_chart(filters) {
            return None;
        }
        let mut state = self.state.lock().unwrap();
        state.worker.as_ref()?;
        state.dependents += 1;
        if state.dependents == 1
            && let Some(view) = state.last.clone()
        {
            state.key = state
                .worker
                .as_mut()
                .and_then(|worker| worker.select(&view, true).ok().flatten());
        }
        Some(Box::new(Retained(Arc::clone(&self.state))))
    }
    fn revision(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.refresh();
        state.revision
    }
    fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.state
            .lock()
            .unwrap()
            .worker
            .as_ref()
            .map(ChartWorker::subscribe)
    }
    fn decode(
        &self,
        capture: CapturedSelection,
    ) -> Result<Arc<dyn ProjectionView<Filter, Bytes>>, ProjectionError> {
        let decoded = View::decode(capture);
        let mut state = self.state.lock().unwrap();
        let view = match decoded {
            Ok(view) => Arc::new(view),
            Err(error) => {
                if let Some(view) = state.last.take()
                    && let Some(worker) = state.worker.as_mut()
                {
                    let _ = worker.select(&view, false);
                }
                state.key = None;
                state.refresh();
                return Err(match error {
                    crate::Error::NotReady => ProjectionError::NotReady,
                    other => ProjectionError::Failed(other.to_string()),
                });
            }
        };
        if state.worker.is_none() {
            return Err(ProjectionError::Failed("provider is shut down".into()));
        }
        let dependent = state.dependents != 0;
        // Chart read/calculation errors intentionally use the pinned not-ready
        // defaults. current_for never pairs an old completion with a new view.
        let desired = if dependent {
            ChartKey::from_view(&view).ok().flatten()
        } else {
            None
        };
        if desired != state.key {
            state.key = state
                .worker
                .as_mut()
                .unwrap()
                .select(&view, dependent)
                .ok()
                .flatten();
        }
        let chart = state.refresh();
        state.last = Some(Arc::clone(&view));
        Ok(Arc::new(Snapshot {
            view,
            chart,
            revision: state.revision,
            response: OnceLock::new(),
        }))
    }
}
struct Snapshot {
    view: Arc<View>,
    chart: Option<Arc<DerivedChart>>,
    revision: u64,
    response: OnceLock<Result<crate::dto::Response, String>>,
}
impl ProjectionView<Filter, Bytes> for Snapshot {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn encode(&self, filters: &[Filter]) -> Result<Bytes, String> {
        let chart = self.chart.as_ref().map(|chart| &chart.facts);
        if filters.is_empty() {
            return crate::encode(&self.view, chart).map_err(|e| e.to_string());
        }
        let response = self
            .response
            .get_or_init(|| crate::build(&self.view, chart).map_err(|e| e.to_string()));
        SharedDocument::new(response.as_ref().map_err(Clone::clone)?)
            .encode(filters)
            .map_err(|e| e.to_string())
    }
}

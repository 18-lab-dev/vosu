use super::{
    cache::{ChartKey, Job},
    projection::{DerivedChart, project},
};
use crate::{Error, view::View};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;
use vosu_osu_calc::{
    beatmap::{BeatmapCache, BeatmapInput},
    live_beatmap::LiveBeatmapEngine,
};
use vosu_surface::resources::ResourceRequester;

#[derive(Default)]
struct Counters {
    starts: AtomicU64,
    opens: AtomicU64,
    reads: AtomicU64,
    parses: AtomicU64,
    cancels: AtomicU64,
    joins: AtomicU64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub starts: u64,
    pub opens: u64,
    pub reads: u64,
    pub parses: u64,
    pub cancels: u64,
    pub joins: u64,
}
#[derive(Default)]
struct State {
    generation: u64,
    key: Option<ChartKey>,
    current: Option<Arc<DerivedChart>>,
    error: Option<Error>,
}
#[cfg(test)]
type CompletionHook = Arc<dyn Fn(&ChartKey) + Send + Sync>;

/// One lazy owned worker, one running job, one replaceable pending job, one cache.
/// Call `select` only for a real dependent consumer; passing false releases it.
/// Async owners must call `shutdown` before dropping, so joining cannot block an
/// async executor. The synchronous Drop fallback still joins, never detaches.
pub struct ChartWorker {
    requester: Arc<dyn ResourceRequester>,
    state: Arc<Mutex<State>>,
    counts: Arc<Counters>,
    pending: Option<watch::Sender<Option<(u64, Job)>>>,
    thread: Option<std::thread::JoinHandle<()>>,
    notices: watch::Sender<u64>,
    #[cfg(any(test, feature = "test-harness"))]
    fixture_engine: Option<Arc<dyn vosu_osu_calc::beatmap::BeatmapEngine + Send + Sync>>,
    #[cfg(test)]
    completion_hook: Option<CompletionHook>,
}
impl ChartWorker {
    pub fn new(requester: Arc<dyn ResourceRequester>) -> Self {
        let (notices, _) = watch::channel(0);
        Self {
            requester,
            state: Arc::default(),
            counts: Arc::default(),
            pending: None,
            thread: None,
            notices,
            #[cfg(any(test, feature = "test-harness"))]
            fixture_engine: None,
            #[cfg(test)]
            completion_hook: None,
        }
    }
    #[cfg(any(test, feature = "test-harness"))]
    pub fn with_fixture_engine(
        requester: Arc<dyn ResourceRequester>,
        engine: Arc<dyn vosu_osu_calc::beatmap::BeatmapEngine + Send + Sync>,
    ) -> Self {
        let mut worker = Self::new(requester);
        worker.fixture_engine = Some(engine);
        worker
    }
    #[cfg(test)]
    pub(super) fn before_completion(&mut self, hook: CompletionHook) {
        self.completion_hook = Some(hook);
    }
    /// Notifications are latest-only and contain no chart histories.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.notices.subscribe()
    }
    pub fn counts(&self) -> Counts {
        let c = &self.counts;
        Counts {
            starts: c.starts.load(Ordering::Relaxed),
            opens: c.opens.load(Ordering::Relaxed),
            reads: c.reads.load(Ordering::Relaxed),
            parses: c.parses.load(Ordering::Relaxed),
            cancels: c.cancels.load(Ordering::Relaxed),
            joins: c.joins.load(Ordering::Relaxed),
        }
    }
    /// Only combine a completion with the exact selected view that requested it.
    pub fn current_for(&self, key: &ChartKey) -> Option<Arc<DerivedChart>> {
        let state = self.state.lock().unwrap();
        (state.key.as_ref() == Some(key))
            .then(|| state.current.clone())
            .flatten()
    }
    #[cfg(test)]
    pub(super) fn current(&self) -> Option<Arc<DerivedChart>> {
        self.state.lock().unwrap().current.clone()
    }
    pub fn error(&self) -> Option<Error> {
        self.state.lock().unwrap().error.clone()
    }
    pub fn select(&mut self, view: &View, dependent: bool) -> Result<Option<ChartKey>, Error> {
        let candidate = if dependent {
            Job::from_view(view)
        } else {
            Ok(None)
        };
        // Bad/missing selections clear old output too; never show the prior map.
        let job = candidate.as_ref().ok().and_then(|job| job.clone());
        let key = job.as_ref().map(|j| j.key.clone());
        let generation = {
            let mut state = self.state.lock().unwrap();
            if state.key == key && candidate.is_ok() && state.error.is_none() {
                return Ok(key);
            }
            state.generation = state.generation.wrapping_add(1);
            state.key = key.clone();
            state.current = None;
            state.error = candidate.as_ref().err().cloned();
            state.generation
        };
        if job.is_some()
            && self.thread.is_none()
            && let Err(error) = self.start()
        {
            self.state.lock().unwrap().error = Some(error.clone());
            return Err(error);
        }
        if let Some(pending) = &self.pending {
            pending.send_replace(job.map(|job| (generation, job)));
        }
        self.notices.send_replace(generation);
        candidate.map(|_| key)
    }
    fn start(&mut self) -> Result<(), Error> {
        let (sender, mut jobs) = watch::channel(None::<(u64, Job)>);
        let requester = self.requester.clone();
        let state = self.state.clone();
        let counts = self.counts.clone();
        let notices = self.notices.clone();
        #[cfg(any(test, feature = "test-harness"))]
        let engine = FixtureEngine(self.fixture_engine.clone());
        #[cfg(not(any(test, feature = "test-harness")))]
        let engine = LiveBeatmapEngine;
        #[cfg(test)]
        let completion_hook = self.completion_hook.clone();
        let thread = std::thread::Builder::new().name("sc-chart".into()).spawn(move || {
            let joined = counts.clone();
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("chart runtime");
            runtime.block_on(async move {
                let mut cache = BeatmapCache::new(engine);
                loop {
                    if jobs.changed().await.is_err() { break; }
                    let Some((generation, job)) = jobs.borrow_and_update().clone() else { cache.invalidate(); continue; };
                    counts.starts.fetch_add(1, Ordering::Relaxed);
                    let load = async {
                        counts.opens.fetch_add(1, Ordering::Relaxed);
                        let resource = requester.open_published_resource(&job.session, &job.key.token).await.map_err(|e| Error::Source(e.to_string()))?;
                        if resource.identity != job.key.token { return Err(Error::Source("chart identity changed".into())); }
                        counts.reads.fetch_add(1, Ordering::Relaxed);
                        resource.read_chart().await.map_err(|e| Error::Source(e.to_string()))
                    };
                    let bytes = tokio::select! {
                        biased;
                        change = jobs.changed() => {
                            counts.cancels.fetch_add(1, Ordering::Relaxed);
                            if change.is_err() { break; }
                            // Retain the notification for the next loop iteration.
                            jobs.mark_changed();
                            continue;
                        },
                        result = tokio::time::timeout(std::time::Duration::from_secs(10), load) => result.unwrap_or_else(|_| Err(Error::Source("chart read deadline".into()))),
                    };
                    let result = bytes.and_then(|bytes| {
                        let source = String::from_utf8_lossy(&bytes);
                        let before = cache.parse_count();
                        let parsed = cache.load(BeatmapInput::Content { content: &source, identity_path: None }, job.key.mode, &job.key.mods, job.key.scoring);
                        counts.parses.fetch_add(cache.parse_count() - before, Ordering::Relaxed);
                        parsed.map(|map| Arc::new(project(&map, job.key.audio_length.map(f64::from_bits)))).map_err(|e| Error::Source(e.to_string()))
                    });
                    #[cfg(test)]
                    if let Some(hook) = &completion_hook { hook(&job.key); }
                    {
                        let mut state = state.lock().unwrap();
                        if state.generation != generation || state.key.as_ref() != Some(&job.key) {
                            counts.cancels.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        match result { Ok(chart) => { state.current = Some(chart); state.error = None; }, Err(e) => { cache.invalidate(); state.error = Some(e); } }
                    }
                    notices.send_replace(generation);
                }
            });
            joined.joins.fetch_add(1, Ordering::Relaxed);
        }).map_err(|e| Error::Source(e.to_string()))?;
        self.pending = Some(sender);
        self.thread = Some(thread);
        Ok(())
    }
    /// Cancels I/O, rejects running calculation completion, and joins its owner.
    pub async fn shutdown(mut self) -> Result<Counts, Error> {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|e| Error::Source(e.to_string()))?
                .map_err(|_| Error::Source("chart worker panicked".into()))?;
        }
        Ok(self.counts())
    }
    fn cancel(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        state.key = None;
        state.current = None;
        self.pending.take();
    }
}
impl Drop for ChartWorker {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// Production retains its concrete engine and allocates no engine wrapper.
// Only explicitly declared source-oracle inputs inject a fixture calculation.
#[cfg(any(test, feature = "test-harness"))]
struct FixtureEngine(Option<Arc<dyn vosu_osu_calc::beatmap::BeatmapEngine + Send + Sync>>);
#[cfg(any(test, feature = "test-harness"))]
impl vosu_osu_calc::beatmap::BeatmapEngine for FixtureEngine {
    fn calculate(
        &self,
        request: vosu_osu_calc::beatmap::EngineRequest<'_>,
    ) -> Result<vosu_osu_calc::beatmap::EngineOutput, vosu_osu_calc::beatmap::BeatmapError> {
        match &self.0 {
            Some(engine) => engine.calculate(request),
            None => vosu_osu_calc::beatmap::BeatmapEngine::calculate(&LiveBeatmapEngine, request),
        }
    }
}

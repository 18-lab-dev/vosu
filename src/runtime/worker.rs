use super::*;

/// Exactly five state publication loops. Connections are owned by their hubs;
/// shutdown cancels every loop and joins it before the provider can be dropped.
pub struct WebSocketHubs {
    pub v1: WebSocketHub,
    pub stream_companion: WebSocketHub,
    pub v2: WebSocketHub,
    pub v2_precise: WebSocketHub,
    pub v2_subscription: V2SubscriptionHub,
    tasks: Vec<JoinHandle<()>>,
}

impl WebSocketHubs {
    pub fn start(
        provider: Arc<dyn WebSocketStateProvider>,
        subscription_provider: Arc<dyn V2SubscriptionProvider>,
        debounce: Duration,
    ) -> Self {
        let v1 = WebSocketHub::new(WebSocketSchema::V1, Arc::clone(&provider));
        let stream_companion =
            WebSocketHub::new(WebSocketSchema::StreamCompanion, Arc::clone(&provider));
        let v2 = WebSocketHub::new(WebSocketSchema::V2, Arc::clone(&provider));
        let v2_precise = WebSocketHub::new(WebSocketSchema::V2Precise, provider);
        let v2_subscription = V2SubscriptionHub::new(subscription_provider, debounce);
        let mut tasks = [&v1, &stream_companion, &v2, &v2_precise]
            .into_iter()
            .map(|hub| {
                let hub = hub.clone();
                tokio::spawn(async move { hub.run().await })
            })
            .collect::<Vec<_>>();
        tasks.push(tokio::spawn({
            let hub = v2_subscription.clone();
            async move { hub.run().await }
        }));
        Self {
            v1,
            stream_companion,
            v2,
            v2_precise,
            v2_subscription,
            tasks,
        }
    }

    pub async fn shutdown(self) {
        // Seal registration and enqueue a protocol close before cancelling the
        // publication loops. `close_all` also waits for each writer/reader
        // callback to unregister, so no upgraded callback can outlive the hub.
        self.v1.close_all(1001).await;
        self.stream_companion.close_all(1001).await;
        self.v2.close_all(1001).await;
        self.v2_precise.close_all(1001).await;
        self.v2_subscription.close_all(1001);
        self.v1.shutdown();
        self.stream_companion.shutdown();
        self.v2.shutdown();
        self.v2_precise.shutdown();
        self.v2_subscription.shutdown();
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

/// Owns the reloadable production Axum listener and its complete join path.
pub struct ListenerOwner {
    pub(super) desired: watch::Sender<ListenTarget>,
    pub(super) state: watch::Receiver<ListenerState>,
    pub(super) cancel: CancellationToken,
    pub(super) task: JoinHandle<()>,
}

impl ListenerOwner {
    /// Starts the listener runtime and waits until the initial address is
    /// actually bound. Readiness must never be reported before this returns.
    pub async fn start(providers: RouteProviders, target: ListenTarget) -> io::Result<Self> {
        let factory = AxumListenerFactory::new(providers);
        let (runtime, state) =
            ListenerRuntime::new(Arc::new(TokioResolver), Arc::new(TokioBinder(factory)));
        let (desired, desired_rx) = watch::channel(target.clone());
        let cancel = CancellationToken::new();
        let task = tokio::spawn(runtime.run(desired_rx, cancel.clone()));
        let mut owner = Self {
            desired,
            state,
            cancel,
            task,
        };
        owner.wait_for_stage(&target).await?;
        Ok(owner)
    }

    /// Stages a replacement. The old listener stays effective when DNS or bind
    /// fails; callers commit their effective config only after success.
    pub async fn stage(&mut self, target: ListenTarget) -> io::Result<()> {
        if self.desired.borrow().clone() == target
            && self
                .state
                .borrow()
                .effective
                .as_ref()
                .is_some_and(|effective| effective.target == target)
        {
            return Ok(());
        }
        self.desired
            .send(target.clone())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "listener runtime stopped"))?;
        self.wait_for_stage(&target).await
    }

    pub fn state(&self) -> ListenerState {
        self.state.borrow().clone()
    }

    async fn wait_for_stage(&mut self, target: &ListenTarget) -> io::Result<()> {
        loop {
            let state = self.state.borrow().clone();
            if state
                .effective
                .as_ref()
                .is_some_and(|effective| &effective.target == target)
            {
                return Ok(());
            }
            if state.desired.as_ref() == Some(target)
                && let Some(error) = state.last_error
            {
                return Err(io::Error::new(io::ErrorKind::AddrNotAvailable, error));
            }
            self.state.changed().await.map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "listener runtime stopped before bind",
                )
            })?;
        }
    }

    /// Stops accepting new HTTP connections and begins the listener drain.
    /// Joining is deliberately separate so WebSocket hubs can first emit 1001
    /// and allow their upgrade callbacks to unregister.
    pub fn stop_accepting(&self) {
        self.cancel.cancel();
    }

    pub async fn join(self) {
        let _ = self.task.await;
    }

    pub async fn shutdown(self) {
        self.stop_accepting();
        self.join().await;
    }
}

pub(super) struct PidWorker {
    generation: u64,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

/// Race-free, exactly-one-worker-per-process registry. A PID reuse with a new
/// process generation is a restart, never an in-place mutation.
#[derive(Default)]
pub(super) struct PidWorkerRegistry {
    pub(super) workers: BTreeMap<u32, PidWorker>,
}

impl PidWorkerRegistry {
    pub(super) async fn reconcile<F>(&mut self, desired: &BTreeMap<u32, u64>, mut spawn: F)
    where
        F: FnMut(u32, u64, CancellationToken) -> JoinHandle<()>,
    {
        let stale = self
            .workers
            .iter()
            .filter_map(|(&pid, worker)| {
                (desired.get(&pid).copied() != Some(worker.generation)).then_some(pid)
            })
            .collect::<Vec<_>>();
        for pid in stale {
            self.stop(pid).await;
        }
        for (&pid, &generation) in desired {
            if self.workers.contains_key(&pid) {
                continue;
            }
            let cancel = CancellationToken::new();
            let task = spawn(pid, generation, cancel.clone());
            self.workers.insert(
                pid,
                PidWorker {
                    generation,
                    cancel,
                    task,
                },
            );
        }
    }

    async fn stop(&mut self, pid: u32) {
        if let Some(worker) = self.workers.remove(&pid) {
            worker.cancel.cancel();
            let _ = worker.task.await;
        }
    }

    pub(super) async fn shutdown(&mut self) {
        let pids = self.workers.keys().copied().collect::<Vec<_>>();
        for pid in pids {
            self.stop(pid).await;
        }
    }
}

/// One worker's API-visible and live state. Both are carried on every
/// publication so the latest-only queue never drops an API update.
pub(super) struct StatePublication {
    pub(super) pid: u32,
    pub(super) generation: u64,
    pub(super) api: Arc<StateBundleSnapshot>,
    pub(super) live: Arc<StateBundleSnapshot>,
}

#[derive(Default)]
struct StatePublicationQueueState {
    closed: bool,
    pending: BTreeMap<u32, StatePublication>,
}

#[derive(Default)]
pub(super) struct StatePublicationQueue {
    state: Mutex<StatePublicationQueueState>,
    notified: tokio::sync::Notify,
}

impl StatePublicationQueue {
    pub(super) fn publish(&self, publication: StatePublication) -> Result<(), StateWorkerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(StateWorkerError::Publish("state supervisor stopped".into()));
        }

        let should_replace = state
            .pending
            .get(&publication.pid)
            .is_none_or(|pending| pending.generation <= publication.generation);
        if should_replace {
            let should_notify = state.pending.is_empty();
            state.pending.insert(publication.pid, publication);
            drop(state);
            if should_notify {
                self.notified.notify_one();
            }
        }
        Ok(())
    }

    pub(super) fn drain(&self) -> BTreeMap<u32, StatePublication> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut state.pending)
    }

    async fn notified(&self) {
        self.notified.notified().await;
    }

    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.pending.clear();
    }
}

#[derive(Default)]
struct PublishedStates {
    api: Option<Arc<StateBundleSnapshot>>,
    live: Option<Arc<StateBundleSnapshot>>,
}

pub(super) struct StatePublicationSink {
    pid: u32,
    generation: u64,
    publications: Arc<StatePublicationQueue>,
    last: Mutex<PublishedStates>,
}

impl StatePublicationSink {
    pub(super) fn new(pid: u32, generation: u64, publications: Arc<StatePublicationQueue>) -> Self {
        Self {
            pid,
            generation,
            publications,
            last: Mutex::new(PublishedStates::default()),
        }
    }
}

impl SnapshotPublisher<Arc<StateBundleSnapshot>> for StatePublicationSink {
    fn publish(
        &self,
        snapshot: Arc<StateBundleSnapshot>,
        delivery: Delivery,
    ) -> Result<(), StateWorkerError> {
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let api = api_state(last.api.as_ref(), &snapshot, delivery);
        let unchanged = |previous: &Option<Arc<StateBundleSnapshot>>, next| {
            previous
                .as_ref()
                .is_some_and(|previous| equivalent_state_snapshots(previous, next))
        };
        if unchanged(&last.live, &snapshot) && unchanged(&last.api, &api) {
            return Ok(());
        }
        self.publications.publish(StatePublication {
            pid: self.pid,
            generation: self.generation,
            api: Arc::clone(&api),
            live: Arc::clone(&snapshot),
        })?;
        last.api = Some(api);
        last.live = Some(snapshot);
        Ok(())
    }
}

/// Selects the API-visible state for a publication.
///
/// API consumers keep the regular component of the last API-cadence capture.
/// A generation or reset-epoch change is promoted immediately so a regular
/// transition and the precise reset it causes stay atomic for API readers.
pub(super) fn api_state(
    previous: Option<&Arc<StateBundleSnapshot>>,
    live: &Arc<StateBundleSnapshot>,
    delivery: Delivery,
) -> Arc<StateBundleSnapshot> {
    let Some(previous) = previous.filter(|previous| {
        previous.source == live.source
            && previous.generation == live.generation
            && previous.reset_epoch == live.reset_epoch
    }) else {
        return Arc::clone(live);
    };
    match delivery {
        Delivery::All => Arc::clone(live),
        Delivery::Precise if Arc::ptr_eq(&previous.regular, &live.regular) => Arc::clone(live),
        Delivery::Precise => Arc::new(StateBundleSnapshot {
            regular: Arc::clone(&previous.regular),
            ..StateBundleSnapshot::clone(live)
        }),
        Delivery::Live => Arc::clone(previous),
    }
}

/// Owns per-ready-instance state workers and the single atomic manager
/// publication loop.
pub struct ProductionStateSupervisor {
    pub(super) cancel: CancellationToken,
    pub(super) task: JoinHandle<()>,
}

impl ProductionStateSupervisor {
    pub fn start(
        lifecycle: watch::Receiver<Arc<InstanceLifecycleSnapshot>>,
        config: watch::Receiver<Arc<crate::config::Config>>,
        subscription_demand: watch::Receiver<bool>,
        snapshots: Arc<SnapshotProviders>,
        logger: Logger,
    ) -> Self {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(run_state_supervisor(
            lifecycle,
            config,
            subscription_demand,
            snapshots,
            logger,
            task_cancel,
        ));
        Self { cancel, task }
    }

    pub async fn shutdown(self) {
        self.shutdown_with_timeout(Duration::from_secs(2)).await;
    }

    pub(super) async fn shutdown_with_timeout(self, deadline: Duration) {
        let Self { cancel, mut task } = self;
        cancel.cancel();
        if tokio::time::timeout(deadline, &mut task).await.is_err() {
            task.abort();
            let _ = task.await;
        }
    }
}

/// One manager publication target: API consumers or the live selected-field
/// stream. Each keeps its own per-instance states and semantic revisions.
#[derive(Default)]
struct ManagerView {
    states: BTreeMap<u32, Arc<StateBundleSnapshot>>,
    state_revisions: BTreeMap<u32, u64>,
    revisions: SemanticRevision,
}

impl ManagerView {
    fn retain(&mut self, desired: &BTreeMap<u32, u64>) {
        self.states
            .retain(|pid, state| desired.get(pid).copied() == Some(state.generation));
        self.state_revisions
            .retain(|pid, _| desired.contains_key(pid));
    }

    fn update(&mut self, pid: u32, snapshot: Arc<StateBundleSnapshot>) {
        update_state_publication(&mut self.states, &mut self.state_revisions, pid, snapshot);
    }

    fn manager(
        &mut self,
        lifecycle: &InstanceLifecycleSnapshot,
        config: &crate::config::Config,
    ) -> Option<Arc<ManagerSnapshot>> {
        build_manager_snapshot(
            &mut self.revisions,
            lifecycle,
            config,
            &self.states,
            &self.state_revisions,
        )
    }
}

pub(super) async fn run_state_supervisor(
    mut lifecycle: watch::Receiver<Arc<InstanceLifecycleSnapshot>>,
    mut config: watch::Receiver<Arc<crate::config::Config>>,
    subscription_demand: watch::Receiver<bool>,
    snapshots: Arc<SnapshotProviders>,
    logger: Logger,
    cancel: CancellationToken,
) {
    let publications = Arc::new(StatePublicationQueue::default());
    let mut workers = PidWorkerRegistry::default();
    let mut api = ManagerView::default();
    let mut live = ManagerView::default();
    let mut lifecycle_value = lifecycle.borrow_and_update().clone();
    let mut config_value = config.borrow_and_update().clone();

    loop {
        let ready = lifecycle_value
            .instances
            .iter()
            .filter(|entry| entry.client == ClientType::Stable || entry.offset_ready)
            .cloned()
            .map(|entry| (entry.pid, entry))
            .collect::<BTreeMap<_, _>>();
        let desired = ready
            .iter()
            .map(|(&pid, entry)| (pid, entry.generation))
            .collect::<BTreeMap<_, _>>();
        api.retain(&desired);
        live.retain(&desired);

        let worker_config = config.clone();
        let worker_demand = subscription_demand.clone();
        let worker_publications = Arc::clone(&publications);
        let worker_logger = logger.clone();
        workers
            .reconcile(&desired, |pid, _generation, worker_cancel| {
                let entry = ready.get(&pid).expect("desired entry").clone();
                let publications = Arc::clone(&worker_publications);
                let config = worker_config.clone();
                let demand = worker_demand.clone();
                let logger = worker_logger.clone();
                tokio::spawn(async move {
                    run_instance_state_worker(
                        entry,
                        config,
                        demand,
                        publications,
                        logger,
                        worker_cancel,
                    )
                    .await;
                })
            })
            .await;

        if let Some(manager) = api.manager(&lifecycle_value, &config_value) {
            snapshots.publish_api(manager);
        }
        if let Some(manager) = live.manager(&lifecycle_value, &config_value) {
            snapshots.publish_live(manager);
        }

        tokio::select! {
            _ = cancel.cancelled() => break,
            changed = lifecycle.changed() => {
                if changed.is_err() { break; }
                lifecycle_value = lifecycle.borrow_and_update().clone();
            }
            changed = config.changed() => {
                if changed.is_err() { break; }
                config_value = config.borrow_and_update().clone();
                snapshots.set_poll_rates(
                    poll_duration(config_value.poll_rate),
                    poll_duration(config_value.precise_data_poll_rate),
                );
            }
            _ = publications.notified() => {
                for (pid, publication) in publications.drain() {
                    if desired.get(&pid).copied() == Some(publication.generation) {
                        api.update(pid, publication.api);
                        live.update(pid, publication.live);
                    }
                }
            }
        }
    }
    publications.close();
    workers.shutdown().await;
}

fn equivalent_state_snapshots(current: &StateBundleSnapshot, next: &StateBundleSnapshot) -> bool {
    current.source == next.source
        && current.generation == next.generation
        && current.reset_epoch == next.reset_epoch
        && Arc::ptr_eq(&current.regular, &next.regular)
        && Arc::ptr_eq(&current.precise, &next.precise)
}

pub(super) fn update_state_publication(
    states: &mut BTreeMap<u32, Arc<StateBundleSnapshot>>,
    state_revisions: &mut BTreeMap<u32, u64>,
    pid: u32,
    snapshot: Arc<StateBundleSnapshot>,
) {
    let unchanged = states
        .get(&pid)
        .is_some_and(|current| equivalent_state_snapshots(current, &snapshot));
    if unchanged {
        return;
    }
    states.insert(pid, snapshot);
    let revision = state_revisions.entry(pid).or_default();
    *revision = revision.saturating_add(1);
}

pub(super) fn poll_duration(milliseconds: f64) -> Duration {
    if milliseconds.is_finite() && milliseconds > 0.0 {
        Duration::try_from_secs_f64(milliseconds.min(ConfigDocument::MAX_INTERVAL_MS) / 1_000.0)
            .unwrap_or(Duration::from_millis(1))
    } else {
        Duration::from_millis(1)
    }
}

pub(super) async fn initialize_state_worker<T, F, Fut, L>(
    cancel: &CancellationToken,
    retry_delay: Duration,
    mut initialize: F,
    mut log_permission_denied: L,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>, StateWorkerError>>,
    L: FnMut(&StateWorkerError),
{
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        match initialize().await {
            Ok(Some(worker)) => return Some(worker),
            Err(error @ StateWorkerError::MemoryPermissionDenied { .. }) => {
                log_permission_denied(&error);
                return None;
            }
            Ok(None) | Err(_) => {
                tokio::select! {
                    _ = cancel.cancelled() => return None,
                    _ = tokio::time::sleep(retry_delay) => {}
                }
            }
        }
    }
}

pub(super) async fn run_instance_state_worker(
    entry: InstanceGeneration,
    config: watch::Receiver<Arc<crate::config::Config>>,
    subscription_demand: watch::Receiver<bool>,
    publications: Arc<StatePublicationQueue>,
    logger: Logger,
    cancel: CancellationToken,
) {
    let publisher = Arc::new(StatePublicationSink::new(
        entry.pid,
        entry.generation,
        publications,
    ));
    let synchronization = Arc::new(Mutex::new(ReaderSynchronization::default()));
    let worker =
        initialize_state_worker(
            &cancel,
            Duration::from_secs(1),
            || {
                let entry = entry.clone();
                let config = config.clone();
                let demand = subscription_demand.clone();
                let publisher = Arc::clone(&publisher);
                let synchronization = Arc::clone(&synchronization);
                async move {
                    create_state_worker(&entry, config, demand, publisher, synchronization).await
                }
            },
            |error| {
                logger.error(format!(
                    "State initialization stopped for PID {} ({:?}, generation {}): {error}",
                    entry.pid, entry.client, entry.generation
                ));
            },
        )
        .await;
    let Some(worker) = worker else {
        return;
    };
    cancel.cancelled().await;
    let _ = worker.shutdown().await;
}

pub(super) enum PreparedReaders {
    Stable(Box<(StableMemory, StableMemory)>),
    Lazer(Box<(LazerMemory, LazerMemory)>),
}

pub(super) async fn create_state_worker(
    entry: &InstanceGeneration,
    config: watch::Receiver<Arc<crate::config::Config>>,
    subscription_demand: watch::Receiver<bool>,
    publisher: Arc<StatePublicationSink>,
    synchronization: Arc<Mutex<ReaderSynchronization>>,
) -> Result<Option<StateWorkerHandle>, StateWorkerError> {
    let process = entry.instance.process().clone();
    let client = entry.client;
    let offsets = entry.instance.lazer_offsets();
    let readers = tokio::task::spawn_blocking(
        move || -> Result<Option<PreparedReaders>, StateWorkerError> {
            match client {
                ClientType::Stable => {
                    let patterns =
                        match StablePatterns::resolve(&process).map_err(StateWorkerError::from)? {
                            crate::process::ReadStatus::Ready(patterns) => patterns,
                            crate::process::ReadStatus::NotReady
                            | crate::process::ReadStatus::Unchanged
                            | crate::process::ReadStatus::Unsupported(_) => return Ok(None),
                        };
                    Ok(Some(PreparedReaders::Stable(Box::new((
                        StableMemory::new(process.clone(), patterns),
                        StableMemory::new(process, patterns),
                    )))))
                }
                ClientType::Lazer => {
                    let Some(offsets) = offsets else {
                        return Ok(None);
                    };
                    let pattern = LazerPattern::source_default();
                    let mut regular =
                        LazerMemory::new(process.clone(), Arc::clone(&offsets), pattern.clone());
                    if !regular
                        .resolve_game_base()
                        .map_err(StateWorkerError::from)?
                    {
                        return Ok(None);
                    }
                    Ok(Some(PreparedReaders::Lazer(Box::new((
                        regular,
                        LazerMemory::new(process, offsets, pattern),
                    )))))
                }
            }
        },
    )
    .await
    .map_err(|error| {
        StateWorkerError::Capture(format!("reader preparation task failed: {error}"))
    })??;
    let Some(readers) = readers else {
        return Ok(None);
    };
    let intervals = PollIntervalUpdates::Config {
        config: config.clone(),
        demand: subscription_demand,
    };
    let worker = match readers {
        PreparedReaders::Stable(readers) => {
            let (regular, precise) = *readers;
            finish_state_worker(
                entry,
                config,
                intervals,
                publisher,
                synchronization,
                regular,
                precise,
            )?
        }
        PreparedReaders::Lazer(readers) => {
            let (regular, precise) = *readers;
            finish_state_worker(
                entry,
                config,
                intervals,
                publisher,
                synchronization,
                regular,
                precise,
            )?
        }
    };
    Ok(Some(worker))
}

pub(super) fn finish_state_worker<R, Q>(
    entry: &InstanceGeneration,
    config: watch::Receiver<Arc<crate::config::Config>>,
    intervals: PollIntervalUpdates,
    publisher: Arc<StatePublicationSink>,
    synchronization: Arc<Mutex<ReaderSynchronization>>,
    regular_memory: R,
    precise_memory: Q,
) -> Result<StateWorkerHandle, StateWorkerError>
where
    R: SynchronizedMemoryReader + 'static,
    Q: SynchronizedMemoryReader + 'static,
{
    let current = config.borrow().clone();
    let calculation =
        Arc::new(crate::calculation::state_backend::LiveStateCalculationBackend::new());
    let mut bundle = StateBundle::new_with_root(
        entry.client,
        entry.generation,
        entry.instance.root().to_path_buf(),
        calculation,
    );
    bundle.set_roles(
        entry.instance.is_tournament_manager(),
        entry.settings.tournament_spectator,
    );
    bundle.set_calculate_pp(current.calculate_pp);
    bundle.set_capture_config(current.read_mania_scroll_speed, current.show_mp_commands);
    bundle.set_config_updates(config.clone());
    publisher.publish(bundle.snapshot(), Delivery::All)?;

    let initial_context = ReadContext {
        source: entry.client,
        instance_generation: entry.generation,
        reset_epoch: 0,
        status: GameState::Menu.value() as i64,
        play_time: 0.0,
        mode: 0,
        mods: default_calculated_mods(),
        committed_hit_error_index: 0,
        accepted_precise_sequence: 0,
        latest_precise_global_sequence: 0,
    };
    let capture = RegularCapturePlan {
        read_mania_scroll_speed: current.read_mania_scroll_speed,
        show_mp_commands: current.show_mp_commands,
        ..RegularCapturePlan::default()
    };
    let regular =
        SourceRegularReader::new(entry.client, regular_memory, Arc::clone(&synchronization));
    let precise = SourcePreciseReader::new(precise_memory, config.clone());
    Ok(spawn_state_worker(
        regular,
        precise,
        bundle,
        publisher,
        initial_context,
        capture,
        intervals,
        synchronization,
    ))
}

pub(super) fn build_manager_snapshot(
    revisions: &mut SemanticRevision,
    lifecycle: &InstanceLifecycleSnapshot,
    config: &crate::config::Config,
    states: &BTreeMap<u32, Arc<StateBundleSnapshot>>,
    state_revisions: &BTreeMap<u32, u64>,
) -> Option<Arc<ManagerSnapshot>> {
    let ready = lifecycle
        .instances
        .iter()
        .filter_map(|entry| states.get(&entry.pid).map(|state| (entry, state)))
        .collect::<Vec<_>>();
    let focused_pid = ready
        .iter()
        .find(|(entry, _)| entry.instance.is_tournament_manager())
        .or_else(|| {
            ready
                .iter()
                .find(|(entry, _)| entry.client == lifecycle.focus.focused_client)
        })
        .map(|(entry, _)| entry.pid);
    let semantics = ManagerSemantics {
        focused_pid,
        game_focused: lifecycle.focus.game_focused,
        enable_key_overlay: config.enable_key_overlay,
        enable_mania_key_hit_errors: config.enable_mania_key_hit_errors,
        instances: ready
            .iter()
            .map(|(entry, _)| {
                (
                    entry.pid,
                    state_revisions.get(&entry.pid).copied().unwrap_or_default(),
                )
            })
            .collect(),
    };
    let (revision, changed) = revisions.observe_with_change(semantics);
    if !changed {
        return None;
    }
    let entries = ready.into_iter().map(|(entry, state)| {
        ManagerInstanceSnapshot::new(
            entry.pid,
            entry.settings.clone(),
            entry.instance.is_tournament_manager(),
            Arc::clone(state),
        )
    });
    Some(Arc::new(ManagerSnapshot::new(
        revision,
        focused_pid,
        lifecycle.focus.game_focused,
        ApiRuntimeSettings {
            enable_key_overlay: config.enable_key_overlay,
            enable_mania_key_hit_errors: config.enable_mania_key_hit_errors,
        },
        entries,
    )))
}
/// Advances only when API-observable manager semantics change. In particular,
/// every instance generation participates, not only the focused PID.
#[derive(Default)]
pub(super) struct SemanticRevision {
    pub(super) revision: u64,
    pub(super) previous: Option<ManagerSemantics>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ManagerSemantics {
    pub(super) focused_pid: Option<u32>,
    pub(super) game_focused: bool,
    pub(super) enable_key_overlay: bool,
    pub(super) enable_mania_key_hit_errors: bool,
    pub(super) instances: BTreeMap<u32, u64>,
}

impl SemanticRevision {
    fn observe_with_change(&mut self, semantics: ManagerSemantics) -> (u64, bool) {
        let changed = self.previous.as_ref() != Some(&semantics);
        if changed {
            self.revision = self.revision.saturating_add(1);
            self.previous = Some(semantics);
        }
        (self.revision, changed)
    }

    #[cfg(test)]
    pub(super) fn observe(&mut self, semantics: ManagerSemantics) -> u64 {
        self.observe_with_change(semantics).0
    }
}

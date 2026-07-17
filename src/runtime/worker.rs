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

#[derive(Clone)]
pub(super) struct StatePublication {
    pid: u32,
    generation: u64,
    snapshot: Arc<StateBundleSnapshot>,
}

pub(super) struct StatePublicationSink {
    pid: u32,
    generation: u64,
    sender: mpsc::UnboundedSender<StatePublication>,
}

impl SnapshotPublisher<Arc<StateBundleSnapshot>> for StatePublicationSink {
    fn publish(&self, snapshot: Arc<StateBundleSnapshot>) -> Result<(), StateWorkerError> {
        self.sender
            .send(StatePublication {
                pid: self.pid,
                generation: self.generation,
                snapshot,
            })
            .map_err(|_| StateWorkerError::Publish("state supervisor stopped".into()))
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
        snapshots: Arc<SnapshotProviders>,
    ) -> Self {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(run_state_supervisor(
            lifecycle,
            config,
            snapshots,
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

pub(super) async fn run_state_supervisor(
    mut lifecycle: watch::Receiver<Arc<InstanceLifecycleSnapshot>>,
    mut config: watch::Receiver<Arc<crate::config::Config>>,
    snapshots: Arc<SnapshotProviders>,
    cancel: CancellationToken,
) {
    let (publications, mut publication_rx) = mpsc::unbounded_channel();
    let mut workers = PidWorkerRegistry::default();
    let mut states = BTreeMap::<u32, Arc<StateBundleSnapshot>>::new();
    let mut state_revisions = BTreeMap::<u32, u64>::new();
    let mut lifecycle_value = lifecycle.borrow_and_update().clone();
    let mut config_value = config.borrow_and_update().clone();
    let mut revisions = SemanticRevision::default();

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
        states.retain(|pid, state| desired.get(pid).copied() == Some(state.generation));
        state_revisions.retain(|pid, _| desired.contains_key(pid));

        let worker_config = config.clone();
        let sender = publications.clone();
        workers
            .reconcile(&desired, |pid, _generation, worker_cancel| {
                let entry = ready.get(&pid).expect("desired entry").clone();
                let sender = sender.clone();
                let worker_config = worker_config.clone();
                tokio::spawn(async move {
                    run_instance_state_worker(entry, worker_config, sender, worker_cancel).await;
                })
            })
            .await;

        publish_manager_snapshot(
            &snapshots,
            &mut revisions,
            &lifecycle_value,
            &config_value,
            &states,
            &state_revisions,
        );

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
            publication = publication_rx.recv() => {
                let Some(publication) = publication else { break; };
                if desired.get(&publication.pid).copied() == Some(publication.generation) {
                    states.insert(publication.pid, publication.snapshot);
                    let revision = state_revisions.entry(publication.pid).or_default();
                    *revision = revision.saturating_add(1);
                }
            }
        }
    }
    workers.shutdown().await;
}

pub(super) fn poll_duration(milliseconds: f64) -> Duration {
    let seconds = milliseconds / 1_000.0;
    if seconds.is_finite() && seconds > 0.0 {
        Duration::from_secs_f64(seconds)
    } else {
        Duration::from_millis(1)
    }
}

pub(super) async fn run_instance_state_worker(
    entry: InstanceGeneration,
    config: watch::Receiver<Arc<crate::config::Config>>,
    sender: mpsc::UnboundedSender<StatePublication>,
    cancel: CancellationToken,
) {
    let publisher = Arc::new(StatePublicationSink {
        pid: entry.pid,
        generation: entry.generation,
        sender,
    });
    let synchronization = Arc::new(Mutex::new(ReaderSynchronization::default()));
    let worker = loop {
        if cancel.is_cancelled() {
            return;
        }
        match create_state_worker(
            &entry,
            config.clone(),
            Arc::clone(&publisher),
            Arc::clone(&synchronization),
        )
        .await
        {
            Ok(Some(worker)) => break worker,
            Ok(None) | Err(_) => {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
    };
    cancel.cancelled().await;
    let _ = worker.shutdown().await;
}

pub(super) enum PreparedReaders {
    Stable(StableMemory, StableMemory),
    Lazer(LazerMemory, LazerMemory),
}

pub(super) async fn create_state_worker(
    entry: &InstanceGeneration,
    config: watch::Receiver<Arc<crate::config::Config>>,
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
                    let patterns = match StablePatterns::resolve(&process)
                        .map_err(|error| StateWorkerError::Capture(error.to_string()))?
                    {
                        crate::process::ReadStatus::Ready(patterns) => patterns,
                        crate::process::ReadStatus::NotReady
                        | crate::process::ReadStatus::Unchanged
                        | crate::process::ReadStatus::Unsupported(_) => return Ok(None),
                    };
                    Ok(Some(PreparedReaders::Stable(
                        StableMemory::new(process.clone(), patterns),
                        StableMemory::new(process, patterns),
                    )))
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
                        .map_err(|error| StateWorkerError::Capture(error.to_string()))?
                    {
                        return Ok(None);
                    }
                    Ok(Some(PreparedReaders::Lazer(
                        regular,
                        LazerMemory::new(process, offsets, pattern),
                    )))
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
    let worker = match readers {
        PreparedReaders::Stable(regular, precise) => {
            finish_state_worker(entry, config, publisher, synchronization, regular, precise)?
        }
        PreparedReaders::Lazer(regular, precise) => {
            finish_state_worker(entry, config, publisher, synchronization, regular, precise)?
        }
    };
    Ok(Some(worker))
}

pub(super) fn finish_state_worker<R, Q>(
    entry: &InstanceGeneration,
    config: watch::Receiver<Arc<crate::config::Config>>,
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
    publisher.publish(bundle.snapshot())?;

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
        PollIntervalUpdates::Config(config),
        synchronization,
    ))
}

pub(super) fn publish_manager_snapshot(
    snapshots: &SnapshotProviders,
    revisions: &mut SemanticRevision,
    lifecycle: &InstanceLifecycleSnapshot,
    config: &crate::config::Config,
    states: &BTreeMap<u32, Arc<StateBundleSnapshot>>,
    state_revisions: &BTreeMap<u32, u64>,
) {
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
    let revision = revisions.observe(semantics);
    let entries = ready.into_iter().map(|(entry, state)| {
        ManagerInstanceSnapshot::new(
            entry.pid,
            entry.settings.clone(),
            entry.instance.is_tournament_manager(),
            Arc::clone(state),
        )
    });
    snapshots.publish(Arc::new(ManagerSnapshot::new(
        revision,
        focused_pid,
        lifecycle.focus.game_focused,
        ApiRuntimeSettings {
            enable_key_overlay: config.enable_key_overlay,
        },
        entries,
    )));
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
    pub(super) instances: BTreeMap<u32, u64>,
}

impl SemanticRevision {
    pub(super) fn observe(&mut self, semantics: ManagerSemantics) -> u64 {
        if self.previous.as_ref() != Some(&semantics) {
            self.revision = self.revision.saturating_add(1);
            self.previous = Some(semantics);
        }
        self.revision
    }
}

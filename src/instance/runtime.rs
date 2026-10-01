use super::*;

pub(super) async fn publish_lifecycle(
    manager: &tokio::sync::RwLock<InstanceManager>,
    sender: &watch::Sender<Arc<InstanceLifecycleSnapshot>>,
) {
    let mut next = {
        let manager = manager.read().await;
        InstanceLifecycleSnapshot {
            revision: 0,
            instances: manager
                .instances
                .values()
                .map(|instance| InstanceGeneration {
                    generation: instance.generation(),
                    pid: instance.pid(),
                    client: instance.client(),
                    settings: instance.settings().clone(),
                    offset_ready: instance.lazer_offsets().is_some(),
                    instance: instance.clone(),
                })
                .collect::<Vec<_>>()
                .into(),
            focus: manager.focus,
        }
    };
    sender.send_if_modified(|current| {
        if current.semantically_eq(&next) {
            return false;
        }
        next.revision = current
            .revision
            .checked_add(1)
            .expect("instance lifecycle revision exhausted");
        *current = Arc::new(next);
        true
    });
}

impl<P, G> InstanceRuntime<P, G>
where
    P: ProcessProvider + 'static,
    G: OffsetGenerator + 'static,
{
    pub fn start(provider: P, loader: LazerOffsetLoader<G>) -> Self {
        Self::start_inner(provider, loader, None)
    }

    pub(crate) fn start_with_logger(
        provider: P,
        loader: LazerOffsetLoader<G>,
        logger: Logger,
    ) -> Self {
        Self::start_inner(provider, loader, Some(logger))
    }

    fn start_inner(provider: P, loader: LazerOffsetLoader<G>, logger: Option<Logger>) -> Self {
        let manager_value = InstanceManager::default();
        let lifecycle_changed = manager_value.lifecycle_changed.clone();
        let manager = Arc::new(tokio::sync::RwLock::new(manager_value));
        let (lifecycle_sender, lifecycle) =
            watch::channel(Arc::new(InstanceLifecycleSnapshot::default()));
        let cancel = CancellationToken::new();
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        let discovery_scans = Arc::new(AtomicU64::new(0));
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        let targeted_inspections = Arc::new(AtomicU64::new(0));
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        let event_wakes = Arc::new(AtomicU64::new(0));
        let loop_logger = logger.clone();
        let provider = Arc::new(provider);
        let loader = Arc::new(loader);
        let loop_manager = manager.clone();
        let loop_cancel = cancel.clone();
        let loop_provider = provider.clone();
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        let loop_discovery_scans = Arc::clone(&discovery_scans);
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        let loop_targeted_inspections = Arc::clone(&targeted_inspections);
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        let loop_event_wakes = Arc::clone(&event_wakes);
        let loop_loader = loader.clone();
        let task = tokio::spawn(async move {
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            let mut full_reconcile = true;
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            let mut process_wake = false;
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            let mut next_reconcile = tokio::time::Instant::now();
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            let mut pending_exits = BTreeMap::<u64, (Pid, tokio::time::Instant, Duration)>::new();
            let mut initial_scan_reported = false;
            let mut last_discovery_error = None::<String>;

            loop {
                if loop_cancel.is_cancelled() {
                    break;
                }

                #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
                {
                    let (inspect, exit, lost) = if process_wake {
                        loop_provider.drain_discovery_events()
                    } else {
                        (Vec::new(), Vec::new(), false)
                    };
                    process_wake = false;
                    full_reconcile |= lost;

                    let now = tokio::time::Instant::now();
                    let (removed, pending_resolvers, interval, tracked, retry_reconcile) = {
                        let mut manager = loop_manager.write().await;
                        let mut removed = Vec::new();
                        let mut retry_reconcile = false;
                        if full_reconcile {
                            removed.extend(manager.take_dead());
                            let result = if let Some(logger) = loop_logger.as_ref() {
                                manager.discover_with_logger(loop_provider.as_ref(), logger)
                            } else {
                                manager.discover(loop_provider.as_ref())
                            };
                            match result {
                                Ok(inserted) => {
                                    last_discovery_error = None;
                                    loop_discovery_scans.fetch_add(1, Ordering::Relaxed);
                                    if !initial_scan_reported {
                                        initial_scan_reported = true;
                                        if let Some(logger) = loop_logger.as_ref() {
                                            logger.info(format!(
                                                "Initial process discovery completed: {inserted} osu! instance(s)"
                                            ));
                                        }
                                    }
                                }
                                Err(error) => {
                                    retry_reconcile = true;
                                    let message = error.to_string();
                                    if last_discovery_error.as_deref() != Some(&message) {
                                        if let Some(logger) = loop_logger.as_ref() {
                                            logger.error(format!(
                                                "Process discovery scan failed: {message}"
                                            ));
                                        }
                                        last_discovery_error = Some(message);
                                    }
                                }
                            }
                            manager.poll_focus(loop_provider.as_ref());
                        } else {
                            for pid in exit {
                                let Some(instance) = manager.instances.get(&pid).cloned() else {
                                    continue;
                                };
                                let generation = instance.generation();
                                if instance.process().is_alive() {
                                    if !pending_exits.contains_key(&generation)
                                        && pending_exits.len() == MAX_PENDING_EXIT_CONFIRMATIONS
                                        && let Some(evicted) = pending_exits
                                            .iter()
                                            .max_by_key(|(_, (_, deadline, _))| *deadline)
                                            .map(|(&generation, _)| generation)
                                    {
                                        pending_exits.remove(&evicted);
                                        retry_reconcile = true;
                                    }
                                    pending_exits.entry(generation).or_insert((
                                        pid,
                                        now + EXIT_CONFIRMATION_INITIAL_BACKOFF,
                                        EXIT_CONFIRMATION_INITIAL_BACKOFF,
                                    ));
                                } else {
                                    pending_exits.remove(&generation);
                                    if manager
                                        .instances
                                        .get(&pid)
                                        .is_some_and(|current| current.generation() == generation)
                                        && let Some(instance) = manager.remove(pid)
                                    {
                                        removed.push(instance);
                                    }
                                }
                            }

                            let due = pending_exits
                                .iter()
                                .filter_map(|(&generation, &(_, deadline, _))| {
                                    (deadline <= now).then_some(generation)
                                })
                                .collect::<Vec<_>>();
                            for generation in due {
                                let Some(&(pid, _, backoff)) = pending_exits.get(&generation)
                                else {
                                    continue;
                                };
                                let current = manager.instances.get(&pid);
                                if current
                                    .is_none_or(|instance| instance.generation() != generation)
                                {
                                    pending_exits.remove(&generation);
                                } else if current
                                    .is_some_and(|instance| !instance.process().is_alive())
                                {
                                    pending_exits.remove(&generation);
                                    if let Some(instance) = manager.remove(pid) {
                                        removed.push(instance);
                                    }
                                } else if let Some(pending) = pending_exits.get_mut(&generation) {
                                    let next_backoff =
                                        (backoff * 2).min(EXIT_CONFIRMATION_MAX_BACKOFF);
                                    pending.1 = now + next_backoff;
                                    pending.2 = next_backoff;
                                }
                            }

                            for pid in inspect {
                                let dead = manager
                                    .instances
                                    .get(&pid)
                                    .is_some_and(|instance| !instance.process().is_alive());
                                if dead && let Some(instance) = manager.remove(pid) {
                                    removed.push(instance);
                                }
                                let result = if let Some(logger) = loop_logger.as_ref() {
                                    manager.discover_pid_with_logger(
                                        loop_provider.as_ref(),
                                        pid,
                                        logger,
                                    )
                                } else {
                                    manager.discover_pid(loop_provider.as_ref(), pid)
                                };
                                match result {
                                    Ok(_) => {
                                        loop_targeted_inspections.fetch_add(1, Ordering::Relaxed);
                                    }
                                    Err(error) => {
                                        retry_reconcile = true;
                                        if let Some(logger) = loop_logger.as_ref() {
                                            logger.error(format!(
                                                "Targeted process inspection failed for PID {pid}: {error}"
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        pending_exits.retain(|&generation, (pid, _, _)| {
                            manager
                                .instances
                                .get(pid)
                                .is_some_and(|instance| instance.generation() == generation)
                        });
                        let pending_resolvers = manager
                            .instances
                            .values()
                            .filter(|instance| {
                                instance.client() == ClientType::Lazer
                                    && instance.lazer_offsets().is_none()
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        let interval =
                            if retry_reconcile || !loop_provider.discovery_events_healthy() {
                                manager.discovery_interval()
                            } else {
                                EVENT_RECONCILIATION_INTERVAL
                            };
                        let tracked = manager.instances.keys().copied().collect::<Vec<_>>();
                        (
                            removed,
                            pending_resolvers,
                            interval,
                            tracked,
                            retry_reconcile,
                        )
                    };
                    loop_provider.replace_tracked_tgids(&tracked);
                    if full_reconcile || retry_reconcile {
                        next_reconcile = tokio::time::Instant::now() + interval;
                        full_reconcile = false;
                    }
                    for instance in removed {
                        instance.shutdown_lazer_resolver().await;
                    }
                    // Publish Add before a fast resolver can publish Ready.
                    publish_lifecycle(&loop_manager, &lifecycle_sender).await;
                    for instance in pending_resolvers {
                        if let Some(logger) = loop_logger.as_ref() {
                            instance
                                .start_lazer_resolver_with_logger(
                                    loop_loader.clone(),
                                    logger.clone(),
                                )
                                .await;
                        } else {
                            instance.start_lazer_resolver(loop_loader.clone()).await;
                        }
                    }

                    let wait_deadline = pending_exits
                        .values()
                        .map(|(_, deadline, _)| *deadline)
                        .min()
                        .map_or(next_reconcile, |deadline| deadline.min(next_reconcile));
                    if let Some(notifier) = loop_provider.discovery_notifier() {
                        tokio::select! {
                                () = loop_cancel.cancelled() => break,
                                () = lifecycle_changed.notified() => {}
                        () = notifier.notified() => {
                            loop_event_wakes.fetch_add(1, Ordering::Relaxed);
                            process_wake = true;
                        },
                                () = tokio::time::sleep_until(wait_deadline) => {
                                    full_reconcile = wait_deadline == next_reconcile;
                                },
                            }
                    } else {
                        tokio::select! {
                            () = loop_cancel.cancelled() => break,
                            () = lifecycle_changed.notified() => {}
                            () = tokio::time::sleep_until(wait_deadline) => {
                                full_reconcile = wait_deadline == next_reconcile;
                            },
                        }
                    }
                    continue;
                }

                #[cfg(not(all(target_os = "linux", feature = "linux-process-events")))]
                {
                    let (interval, removed, pending_resolvers) = {
                        let mut manager = loop_manager.write().await;
                        let result = if let Some(logger) = loop_logger.as_ref() {
                            manager.discover_with_logger(loop_provider.as_ref(), logger)
                        } else {
                            manager.discover(loop_provider.as_ref())
                        };
                        match result {
                            Ok(inserted) => {
                                last_discovery_error = None;
                                if !initial_scan_reported {
                                    initial_scan_reported = true;
                                    if let Some(logger) = loop_logger.as_ref() {
                                        logger.info(format!(
                                            "Initial process discovery completed: {inserted} osu! instance(s)"
                                        ));
                                    }
                                }
                            }
                            Err(error) => {
                                let message = error.to_string();
                                if last_discovery_error.as_deref() != Some(&message) {
                                    if let Some(logger) = loop_logger.as_ref() {
                                        logger.error(format!(
                                            "Process discovery scan failed: {message}"
                                        ));
                                    }
                                    last_discovery_error = Some(message);
                                }
                            }
                        }
                        let removed = manager.take_dead();
                        manager.poll_focus(loop_provider.as_ref());
                        let pending_resolvers = manager
                            .instances
                            .values()
                            .filter(|instance| {
                                instance.client() == ClientType::Lazer
                                    && instance.lazer_offsets().is_none()
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        (manager.discovery_interval(), removed, pending_resolvers)
                    };
                    for instance in pending_resolvers {
                        if let Some(logger) = loop_logger.as_ref() {
                            instance
                                .start_lazer_resolver_with_logger(
                                    loop_loader.clone(),
                                    logger.clone(),
                                )
                                .await;
                        } else {
                            instance.start_lazer_resolver(loop_loader.clone()).await;
                        }
                    }
                    for instance in removed {
                        instance.shutdown_lazer_resolver().await;
                    }
                    publish_lifecycle(&loop_manager, &lifecycle_sender).await;
                    tokio::select! {
                        () = loop_cancel.cancelled() => break,
                        () = lifecycle_changed.notified() => {}
                        () = tokio::time::sleep(interval) => {}
                    }
                }
            }

            let removed = {
                let mut manager = loop_manager.write().await;
                manager.take_all()
            };
            for instance in removed {
                instance.shutdown_lazer_resolver().await;
            }
            publish_lifecycle(&loop_manager, &lifecycle_sender).await;
        });
        Self {
            manager,
            lifecycle,
            cancel,
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            discovery_scans,
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            targeted_inspections,
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            event_wakes,
            task: tokio::sync::Mutex::new(Some(task)),
            #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
            provider,
            _provider: PhantomData,
            _loader: loader,
        }
    }

    #[must_use]
    pub fn manager(&self) -> Arc<tokio::sync::RwLock<InstanceManager>> {
        self.manager.clone()
    }

    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    #[must_use]
    pub fn discovery_scan_count(&self) -> u64 {
        self.discovery_scans.load(Ordering::Relaxed)
    }

    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    #[must_use]
    pub fn targeted_inspection_count(&self) -> u64 {
        self.targeted_inspections.load(Ordering::Relaxed)
    }

    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    #[must_use]
    pub fn process_event_count(&self) -> u64 {
        self.provider.discovery_event_count()
    }

    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    #[must_use]
    pub fn event_wake_count(&self) -> u64 {
        self.event_wakes.load(Ordering::Relaxed)
    }

    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    #[must_use]
    pub fn reconciliation_request_count(&self) -> u64 {
        self.provider.discovery_reconciliation_request_count()
    }

    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    #[must_use]
    pub fn process_events_healthy(&self) -> bool {
        self.provider.discovery_events_healthy()
    }

    /// Atomically subscribes to the current immutable lifecycle snapshot.
    #[must_use]
    pub fn subscribe_current(&self) -> watch::Receiver<Arc<InstanceLifecycleSnapshot>> {
        self.lifecycle.clone()
    }

    /// Cancels and joins discovery and all generation-owned resolver tasks.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }
}

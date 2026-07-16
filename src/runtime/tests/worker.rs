#[tokio::test]
async fn registry_add_remove_restart_and_complete_join() {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let exits = Arc::new(AtomicUsize::new(0));
    let mut registry = PidWorkerRegistry::default();
    let spawn = |pid, generation, cancel: CancellationToken| {
        starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((pid, generation));
        let exits = Arc::clone(&exits);
        tokio::spawn(async move {
            cancel.cancelled().await;
            exits.fetch_add(1, Ordering::AcqRel);
        })
    };

    registry
        .reconcile(&BTreeMap::from([(7, 1), (9, 1)]), spawn)
        .await;
    registry
        .reconcile(&BTreeMap::from([(7, 1), (9, 1)]), spawn)
        .await;
    assert_eq!(
        *starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![(7, 1), (9, 1)],
        "unchanged generations must not create duplicate workers"
    );

    registry.reconcile(&BTreeMap::from([(7, 2)]), spawn).await;
    assert_eq!(
        *starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![(7, 1), (9, 1), (7, 2)]
    );
    assert_eq!(exits.load(Ordering::Acquire), 2);

    registry.shutdown().await;
    assert!(registry.workers.is_empty());
    assert_eq!(exits.load(Ordering::Acquire), 3);
}

#[tokio::test]
async fn supervisor_shutdown_aborts_a_worker_tree_after_its_deadline() {
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        task_cancel.cancelled().await;
        std::future::pending::<()>().await;
    });
    let supervisor = ProductionStateSupervisor { cancel, task };

    tokio::time::timeout(
        Duration::from_millis(50),
        supervisor.shutdown_with_timeout(Duration::from_millis(5)),
    )
    .await
    .expect("supervisor shutdown deadline must abort a stuck worker tree");
}

#[tokio::test]
async fn four_hub_tasks_join_without_lingering() {
    let provider = Arc::new(SnapshotProviders::new(
        Arc::new(ManagerSnapshot::not_ready(
            0,
            crate::api::ApiRuntimeSettings::default(),
        )),
        Arc::new(ProductionSnapshotEncoder),
    ));
    let hubs = WebSocketHubs::start(provider);
    hubs.shutdown().await;
}


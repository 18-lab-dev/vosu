use super::*;
use crate::process::{MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};
use std::sync::{Mutex, atomic::AtomicUsize};

#[derive(Debug)]
struct FakeMemory {
    pid: Pid,
    alive: Arc<AtomicBool>,
    drops: Arc<AtomicUsize>,
}
impl Drop for FakeMemory {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl ProcessMemory for FakeMemory {
    fn read_exact(&self, _: usize, _: &mut [u8]) -> Result<(), MemoryError> {
        Ok(())
    }
    fn read_many(&self, _: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![])
    }
    fn process_id(&self) -> Option<Pid> {
        Some(self.pid)
    }
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
type EventBatch = (Vec<Pid>, Vec<Pid>, bool);

struct FakeProvider {
    platform: DiscoveryPlatform,
    processes: Vec<ProcessDescriptor>,
    inspected_processes: Vec<ProcessDescriptor>,
    drops: Arc<AtomicUsize>,
    alive: Arc<AtomicBool>,
    foreground: Option<Pid>,
    open_alives: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    notify: Option<Arc<Notify>>,
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    events_healthy: Arc<AtomicBool>,
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    enumerations: Arc<AtomicUsize>,
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    inspections: Arc<AtomicUsize>,
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    event_batch: Arc<Mutex<EventBatch>>,
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    enumeration_failures: Arc<AtomicUsize>,
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    inspection_failures: Arc<AtomicUsize>,
}
impl ProcessProvider for FakeProvider {
    fn platform(&self) -> DiscoveryPlatform {
        self.platform
    }
    fn enumerate(&self) -> Result<Vec<ProcessDescriptor>, MemoryError> {
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        self.enumerations.fetch_add(1, Ordering::SeqCst);
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        if self
            .enumeration_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(MemoryError::Backend {
                operation: "test enumerate",
                message: "transient failure".to_owned(),
            });
        }
        Ok(self.processes.clone())
    }
    fn inspect(&self, pid: Pid) -> Result<Option<ProcessDescriptor>, MemoryError> {
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        self.inspections.fetch_add(1, Ordering::SeqCst);
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        if self
            .inspection_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(MemoryError::Backend {
                operation: "test inspect",
                message: "transient failure".to_owned(),
            });
        }
        Ok(self
            .inspected_processes
            .iter()
            .find(|process| process.pid == pid)
            .cloned())
    }
    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError> {
        let alive = {
            let mut open_alives = self
                .open_alives
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if open_alives.is_empty() {
                Arc::clone(&self.alive)
            } else {
                open_alives.remove(0)
            }
        };
        Ok(ProcessHandle::new(
            Arc::new(FakeMemory {
                pid,
                alive,
                drops: Arc::clone(&self.drops),
            }),
            ProcessBitness::X64,
        ))
    }
    fn foreground_pid(&self) -> Option<Pid> {
        self.foreground
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn discovery_notifier(&self) -> Option<&Notify> {
        self.notify.as_deref()
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn discovery_events_healthy(&self) -> bool {
        self.events_healthy.load(Ordering::SeqCst)
    }
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    fn drain_discovery_events(&self) -> (Vec<Pid>, Vec<Pid>, bool) {
        std::mem::take(
            &mut *self
                .event_batch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

fn descriptor(pid: Pid, name: &str, bitness: ProcessBitness, args: &[&str]) -> ProcessDescriptor {
    ProcessDescriptor {
        pid,
        name: name.into(),
        bitness,
        arguments: StartupArgs::from_array(args.iter().map(OsString::from)),
        root: PathBuf::new(),
    }
}

fn provider(platform: DiscoveryPlatform, processes: Vec<ProcessDescriptor>) -> FakeProvider {
    FakeProvider {
        platform,
        inspected_processes: processes.clone(),
        processes,
        drops: Arc::new(AtomicUsize::new(0)),
        foreground: None,
        alive: Arc::new(AtomicBool::new(true)),
        open_alives: Arc::new(Mutex::new(Vec::new())),
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        notify: None,
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        events_healthy: Arc::new(AtomicBool::new(false)),
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        enumerations: Arc::new(AtomicUsize::new(0)),
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        inspections: Arc::new(AtomicUsize::new(0)),
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        event_batch: Arc::new(Mutex::new((Vec::new(), Vec::new(), false))),
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        enumeration_failures: Arc::new(AtomicUsize::new(0)),
        #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
        inspection_failures: Arc::new(AtomicUsize::new(0)),
    }
}

#[test]
fn windows_names_and_bitness_classify_and_btree_orders() {
    let provider = provider(
        DiscoveryPlatform::Windows,
        vec![
            descriptor(40, "OSULAZER.EXE", ProcessBitness::X64, &[]),
            descriptor(9, "osu!.exe", ProcessBitness::X86, &[]),
            descriptor(2, "other.exe", ProcessBitness::X64, &[]),
        ],
    );
    let mut manager = InstanceManager::default();
    assert_eq!(manager.discover(&provider).unwrap(), 2);
    assert_eq!(
        manager.instances.keys().copied().collect::<Vec<_>>(),
        vec![9, 40]
    );
    assert_eq!(manager.instances[&9].client(), ClientType::Stable);

    assert_eq!(manager.instances[&40].client(), ClientType::Lazer);
}

#[test]
fn targeted_discovery_inserts_only_the_requested_pid() {
    let provider = provider(
        DiscoveryPlatform::Linux,
        vec![
            descriptor(8, "osu!", ProcessBitness::X64, &[]),
            descriptor(9, "osu!", ProcessBitness::X64, &[]),
        ],
    );
    let mut manager = InstanceManager::default();

    assert!(manager.discover_pid(&provider, 9).unwrap());
    assert_eq!(manager.instances.keys().copied().collect::<Vec<_>>(), [9]);
    #[cfg(all(target_os = "linux", feature = "linux-process-events"))]
    {
        assert_eq!(provider.inspections.load(Ordering::SeqCst), 1);
        assert_eq!(provider.enumerations.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn targeted_discovery_replaces_a_dead_same_pid_generation() {
    let old_provider = provider(
        DiscoveryPlatform::Linux,
        vec![descriptor(9, "osu!", ProcessBitness::X64, &[])],
    );
    let old_alive = Arc::clone(&old_provider.alive);
    let mut manager = InstanceManager::default();
    assert!(manager.discover_pid(&old_provider, 9).unwrap());
    let old = manager.instances[&9].clone();

    old_alive.store(false, Ordering::SeqCst);
    let new_provider = provider(
        DiscoveryPlatform::Linux,
        vec![descriptor(9, "osu!", ProcessBitness::X64, &[])],
    );
    assert!(manager.discover_pid(&new_provider, 9).unwrap());

    assert!(old.lifecycle_token().is_cancelled());
    assert_ne!(manager.instances[&9].generation(), old.generation());
    assert!(manager.instances[&9].process().is_alive());
}

#[test]
fn linux_named_first_pass_is_stable_and_fallback_is_lazer() {
    let named = vec![
        descriptor(8, "osu!", ProcessBitness::X64, &[]),
        descriptor(4, "wine-osu!.exe", ProcessBitness::X64, &[]),
    ];
    let classified = classify_candidates(DiscoveryPlatform::Linux, &named);
    assert_eq!(
        classified
            .iter()
            .map(|candidate| (candidate.pid, candidate.client))
            .collect::<Vec<_>>(),
        [(4, ClientType::Stable), (8, ClientType::Stable)],
    );
    let fallback = classify_candidates(
        DiscoveryPlatform::Linux,
        &[descriptor(8, "osu!", ProcessBitness::X64, &[])],
    );
    assert_eq!(
        (fallback[0].pid, fallback[0].client),
        (8, ClientType::Lazer)
    );
}

#[test]
fn command_filters_follow_presence_and_truthiness() {
    assert!(should_skip(&StartupArgs::from_array([OsString::from(
        "--tournament="
    )])));
    assert!(!should_skip(&StartupArgs::from_array([OsString::from(
        "--debug-client-id=false"
    )])));
    assert!(!should_skip(&StartupArgs::from_array([OsString::from(
        "--debug-client-id=0"
    )])));
    assert!(should_skip(&StartupArgs::from_array([OsString::from(
        "--debug-client-id=1"
    )])));
    assert!(should_skip(&StartupArgs::from_array([OsString::from(
        "--debug-client-id=x"
    )])));
    assert!(!should_skip(&StartupArgs::parse([OsString::from(
        "/opt/osu!/osu!"
    )])));
    assert!(should_skip(&StartupArgs::parse([
        OsString::from("/opt/osu!/osu!"),
        OsString::from("--tournament"),
    ])));
}

#[test]
fn spectator_and_devserver_settings_are_typed() {
    let provider = provider(
        DiscoveryPlatform::Windows,
        vec![descriptor(
            3,
            "osu!.exe",
            ProcessBitness::X86,
            &["--spectateclient=12", "--devserver=https://example.test"],
        )],
    );
    let mut manager = InstanceManager::default();
    manager.discover(&provider).unwrap();
    let settings = manager.instances[&3].settings();
    assert_eq!(settings.tournament_ipc_id, Some(12.0));
    assert!(settings.tournament_spectator);
    assert_eq!(
        settings.devserver_endpoint.as_deref(),
        Some(std::ffi::OsStr::new("https://example.test"))
    );
}

#[test]
fn duplicate_discovery_and_removal_are_idempotent_and_close_once() {
    let provider = provider(
        DiscoveryPlatform::Windows,
        vec![descriptor(3, "osu!.exe", ProcessBitness::X86, &[])],
    );
    let drops = Arc::clone(&provider.drops);
    let mut manager = InstanceManager::default();
    assert_eq!(manager.discover(&provider).unwrap(), 1);
    assert_eq!(manager.instances[&3].process().process_id(), Some(3));
    assert!(manager.instances[&3].process().is_alive());
    assert_eq!(manager.discover(&provider).unwrap(), 0);
    let instance = manager.remove(3).unwrap();
    assert!(manager.remove(3).is_none());
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(instance);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn tournament_precedence_matching_and_fallback_selection() {
    let provider = provider(
        DiscoveryPlatform::Windows,
        vec![
            descriptor(30, "osu!.exe", ProcessBitness::X86, &[]),
            descriptor(20, "osu!.exe", ProcessBitness::X64, &[]),
            descriptor(10, "osu!.exe", ProcessBitness::X86, &[]),
        ],
    );
    let mut manager = InstanceManager::default();
    manager.discover(&provider).unwrap();
    manager.instances[&30].set_tournament_manager(true);
    manager.instances[&20].set_tournament_manager(true);
    assert_eq!(manager.select(None).unwrap().pid(), 20);
    assert_eq!(manager.select(Some(ClientType::Stable)).unwrap().pid(), 30);
    manager.instances[&30].set_tournament_manager(false);
    assert_eq!(manager.select(Some(ClientType::Stable)).unwrap().pid(), 10);
    manager.instances.remove(&20);
    assert!(manager.select(Some(ClientType::Lazer)).is_none());
}

#[test]
fn foreground_exact_match_and_unavailable_focus_fallback() {
    let mut provider = provider(
        DiscoveryPlatform::Windows,
        vec![
            descriptor(2, "osu!.exe", ProcessBitness::X64, &[]),
            descriptor(1, "osu!.exe", ProcessBitness::X86, &[]),
        ],
    );
    let mut manager = InstanceManager::default();
    manager.discover(&provider).unwrap();
    provider.foreground = Some(2);
    assert_eq!(
        manager.poll_focus(&provider),
        FocusState {
            focused_client: ClientType::Lazer,
            game_focused: true
        }
    );
    provider.foreground = Some(999);
    assert_eq!(
        manager.poll_focus(&provider),
        FocusState {
            focused_client: ClientType::Stable,
            game_focused: false
        }
    );
    provider.platform = DiscoveryPlatform::Linux;
    provider.foreground = Some(2);
    assert_eq!(
        manager.poll_focus(&provider),
        FocusState {
            focused_client: ClientType::Stable,
            game_focused: false
        }
    );
    manager.instances.remove(&1);
    assert_eq!(
        manager.poll_focus(&provider),
        FocusState {
            focused_client: ClientType::Lazer,
            game_focused: false
        }
    );
}

#[derive(Clone)]
struct OfflineFetcher;
impl crate::memory::lazer_loader::OffsetFetcher for OfflineFetcher {
    fn fetch<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Duration,
    ) -> crate::memory::lazer_loader::FetchFuture<'a> {
        Box::pin(async {
            Err(crate::memory::lazer_loader::FetchError {
                message: "offline".into(),
            })
        })
    }
}

#[tokio::test]
async fn lifecycle_snapshots_are_pid_ordered_and_semantic_exactly_once() {
    let mut provider = provider(
        DiscoveryPlatform::Windows,
        vec![
            descriptor(20, "osu!.exe", ProcessBitness::X64, &[]),
            descriptor(10, "osu!.exe", ProcessBitness::X86, &[]),
        ],
    );
    let manager = tokio::sync::RwLock::new(InstanceManager::default());
    let (sender, mut subscriber) = watch::channel(Arc::new(InstanceLifecycleSnapshot::default()));

    assert_eq!(subscriber.borrow().revision, 0);
    assert_eq!(manager.write().await.discover(&provider).unwrap(), 2);
    publish_lifecycle(&manager, &sender).await;
    subscriber.changed().await.unwrap();
    let added = subscriber.borrow_and_update().clone();
    assert_eq!(added.revision, 1);
    assert_eq!(
        added
            .instances
            .iter()
            .map(|entry| entry.pid)
            .collect::<Vec<_>>(),
        [10, 20],
    );
    assert_eq!(
        added
            .instances
            .iter()
            .map(|entry| entry.generation)
            .collect::<Vec<_>>(),
        [1, 2],
    );

    assert_eq!(manager.write().await.discover(&provider).unwrap(), 0);
    publish_lifecycle(&manager, &sender).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), subscriber.changed())
            .await
            .is_err()
    );

    provider.foreground = Some(20);
    manager.write().await.poll_focus(&provider);
    publish_lifecycle(&manager, &sender).await;
    subscriber.changed().await.unwrap();
    assert_eq!(subscriber.borrow_and_update().revision, 2);
    assert_eq!(
        subscriber.borrow().focus,
        FocusState {
            focused_client: ClientType::Lazer,
            game_focused: true
        },
    );

    let readiness = manager.read().await.lifecycle_changed.clone();
    let lazer = manager.read().await.instances[&20].clone();
    assert!(lazer.install_lazer_offsets(Arc::new(LazerOffsets::embedded().unwrap())));
    tokio::time::timeout(Duration::from_millis(10), readiness.notified())
        .await
        .expect("offset resolver readiness notification");
    publish_lifecycle(&manager, &sender).await;
    subscriber.changed().await.unwrap();
    let ready = subscriber.borrow_and_update().clone();
    assert_eq!(ready.revision, 3);
    assert!(
        ready
            .instances
            .iter()
            .find(|entry| entry.pid == 20)
            .unwrap()
            .offset_ready
    );
    assert!(!lazer.install_lazer_offsets(Arc::new(LazerOffsets::embedded().unwrap())));
    publish_lifecycle(&manager, &sender).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(10), subscriber.changed())
            .await
            .is_err()
    );

    manager.write().await.remove_and_join(10).await;
    publish_lifecycle(&manager, &sender).await;
    subscriber.changed().await.unwrap();
    let removed = subscriber.borrow_and_update().clone();
    assert_eq!(removed.revision, 4);
    assert_eq!(
        removed
            .instances
            .iter()
            .map(|entry| entry.pid)
            .collect::<Vec<_>>(),
        [20]
    );
}

#[tokio::test]
async fn lazer_discovery_starts_one_resolver_and_shutdown_joins_it() {
    let provider = provider(
        DiscoveryPlatform::Windows,
        vec![descriptor(2, "osu!.exe", ProcessBitness::X64, &[])],
    );
    let cache = tempfile::tempdir().unwrap();
    let embedded = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    let loader = Arc::new(LazerOffsetLoader::new(
        OfflineFetcher,
        cache.path().to_owned(),
        bytes::Bytes::copy_from_slice(&embedded.data),
    ));
    let mut manager = InstanceManager::default();
    assert_eq!(
        manager
            .discover_with_lazer_loader(&provider, loader.clone())
            .await
            .unwrap(),
        1
    );
    let instance = manager.instances()[&2].clone();
    assert!(instance.resolver_task.lock().await.is_some());
    assert_eq!(
        manager
            .discover_with_lazer_loader(&provider, loader)
            .await
            .unwrap(),
        0
    );
    assert!(instance.resolver_task.lock().await.is_some());
    assert!(manager.remove_and_join(2).await.is_some());
    assert!(instance.resolver_task.lock().await.is_none());
    assert!(instance.lifecycle_token().is_cancelled());
}

#[tokio::test]
async fn discovery_runtime_publishes_current_then_closes_after_joined_shutdown() {
    let provider = provider(
        DiscoveryPlatform::Windows,
        vec![descriptor(7, "osu!.exe", ProcessBitness::X64, &[])],
    );
    let cache = tempfile::tempdir().unwrap();
    let embedded = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    let runtime = InstanceRuntime::start(
        provider,
        LazerOffsetLoader::new(
            OfflineFetcher,
            cache.path().to_owned(),
            bytes::Bytes::copy_from_slice(&embedded.data),
        ),
    );
    let manager = runtime.manager();
    let mut subscriber = runtime.subscribe_current();
    tokio::time::timeout(Duration::from_secs(1), subscriber.changed())
        .await
        .unwrap()
        .unwrap();
    let added = subscriber.borrow_and_update().clone();
    assert_eq!(added.revision, 1);
    assert_eq!(
        added
            .instances
            .iter()
            .map(|entry| entry.pid)
            .collect::<Vec<_>>(),
        [7]
    );

    let late_subscriber = runtime.subscribe_current();
    assert_eq!(late_subscriber.borrow().revision, added.revision);
    assert_eq!(
        late_subscriber.borrow().instances[0].generation,
        added.instances[0].generation
    );

    let instance = manager.read().await.instances()[&7].clone();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if instance.resolver_task.lock().await.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown().await;
    assert!(manager.read().await.instances().is_empty());
    assert!(instance.lifecycle_token().is_cancelled());
    assert!(instance.resolver_task.lock().await.is_none());

    subscriber.changed().await.unwrap();
    let removed = subscriber.borrow_and_update().clone();
    assert_eq!(removed.revision, added.revision + 1);
    assert!(removed.instances.is_empty());
    assert!(subscriber.changed().await.is_err());
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
#[tokio::test]
async fn failed_full_scan_retries_early_and_counts_only_success() {
    let provider = provider(DiscoveryPlatform::Linux, Vec::new());
    provider.events_healthy.store(true, Ordering::SeqCst);
    provider.enumeration_failures.store(1, Ordering::SeqCst);
    let attempts = Arc::clone(&provider.enumerations);
    let health = Arc::clone(&provider.events_healthy);
    let cache = tempfile::tempdir().unwrap();
    let embedded = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    let runtime = InstanceRuntime::start(
        provider,
        LazerOffsetLoader::new(
            OfflineFetcher,
            cache.path().to_owned(),
            bytes::Bytes::copy_from_slice(&embedded.data),
        ),
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while attempts.load(Ordering::SeqCst) < 2 {
            assert_eq!(runtime.discovery_scan_count(), 0);
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed healthy scan must retry before the five-second reconciliation");
    assert_eq!(runtime.discovery_scan_count(), 1);
    assert!(runtime.process_events_healthy());

    health.store(false, Ordering::SeqCst);
    assert!(!runtime.process_events_healthy());
    runtime.shutdown().await;
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
#[tokio::test]
async fn failed_targeted_inspection_is_not_counted() {
    let mut provider = provider(DiscoveryPlatform::Linux, Vec::new());
    provider.inspected_processes = vec![descriptor(7, "osu!", ProcessBitness::X64, &[])];
    provider.inspection_failures.store(1, Ordering::SeqCst);
    let notify = Arc::new(Notify::new());
    let attempts = Arc::clone(&provider.inspections);
    let event_batch = Arc::clone(&provider.event_batch);
    provider.notify = Some(Arc::clone(&notify));
    provider.events_healthy.store(true, Ordering::SeqCst);
    let cache = tempfile::tempdir().unwrap();
    let embedded = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    let runtime = InstanceRuntime::start(
        provider,
        LazerOffsetLoader::new(
            OfflineFetcher,
            cache.path().to_owned(),
            bytes::Bytes::copy_from_slice(&embedded.data),
        ),
    );

    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (vec![7], Vec::new(), false);
    notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while attempts.load(Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed targeted inspection");
    assert_eq!(runtime.targeted_inspection_count(), 0);

    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (vec![7], Vec::new(), false);
    notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.targeted_inspection_count() < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successful targeted inspection");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    runtime.shutdown().await;
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
#[tokio::test]
async fn full_reconcile_replaces_a_dead_same_pid_in_one_pass() {
    let mut provider = provider(
        DiscoveryPlatform::Linux,
        vec![descriptor(7, "osu!", ProcessBitness::X64, &[])],
    );
    let old_alive = Arc::new(AtomicBool::new(true));
    let new_alive = Arc::new(AtomicBool::new(true));
    *provider
        .open_alives
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        vec![Arc::clone(&old_alive), Arc::clone(&new_alive)];
    let notify = Arc::new(Notify::new());
    let event_batch = Arc::clone(&provider.event_batch);
    provider.notify = Some(Arc::clone(&notify));
    provider.events_healthy.store(true, Ordering::SeqCst);
    let cache = tempfile::tempdir().unwrap();
    let embedded = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    let runtime = InstanceRuntime::start(
        provider,
        LazerOffsetLoader::new(
            OfflineFetcher,
            cache.path().to_owned(),
            bytes::Bytes::copy_from_slice(&embedded.data),
        ),
    );

    let old = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(instance) = runtime.manager().read().await.instances().get(&7) {
                break instance.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial generation");
    old_alive.store(false, Ordering::SeqCst);
    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (Vec::new(), Vec::new(), true);
    notify.notify_one();

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let replacement = runtime.manager().read().await.instances()[&7].clone();
            if replacement.generation() != old.generation() {
                assert!(replacement.process().is_alive());
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("same-pass replacement");
    assert!(old.lifecycle_token().is_cancelled());
    runtime.shutdown().await;
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
#[tokio::test]
async fn live_exit_is_retried_until_the_process_dies_without_another_event() {
    let mut provider = provider(
        DiscoveryPlatform::Linux,
        vec![descriptor(7, "osu!", ProcessBitness::X64, &[])],
    );
    let notify = Arc::new(Notify::new());
    let event_batch = Arc::clone(&provider.event_batch);
    let alive = Arc::clone(&provider.alive);
    provider.notify = Some(Arc::clone(&notify));
    provider.events_healthy.store(true, Ordering::SeqCst);
    let cache = tempfile::tempdir().unwrap();
    let embedded = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    let runtime = InstanceRuntime::start(
        provider,
        LazerOffsetLoader::new(
            OfflineFetcher,
            cache.path().to_owned(),
            bytes::Bytes::copy_from_slice(&embedded.data),
        ),
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while !runtime.manager().read().await.instances().contains_key(&7) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial discovery");

    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (Vec::new(), vec![7], false);
    notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let drained = event_batch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .1
                .is_empty();
            if drained {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("live exit revalidation");
    assert!(runtime.manager().read().await.instances().contains_key(&7));

    alive.store(false, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.manager().read().await.instances().contains_key(&7) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("pending exit confirmation");
    runtime.shutdown().await;
}

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
#[tokio::test]
async fn process_event_wakes_are_targeted_and_loss_reconciles_once() {
    let mut provider = provider(DiscoveryPlatform::Linux, Vec::new());
    provider.inspected_processes = vec![descriptor(7, "osu!", ProcessBitness::X64, &[])];
    let notify = Arc::new(Notify::new());
    let enumerations = Arc::clone(&provider.enumerations);
    let inspections = Arc::clone(&provider.inspections);
    let event_batch = Arc::clone(&provider.event_batch);
    let alive = Arc::clone(&provider.alive);
    provider.notify = Some(Arc::clone(&notify));
    provider.events_healthy.store(true, Ordering::SeqCst);
    let cache = tempfile::tempdir().unwrap();
    let embedded = crate::assets::DaemonAssets::get("offsets.json").unwrap();
    let runtime = InstanceRuntime::start(
        provider,
        LazerOffsetLoader::new(
            OfflineFetcher,
            cache.path().to_owned(),
            bytes::Bytes::copy_from_slice(&embedded.data),
        ),
    );

    tokio::time::timeout(Duration::from_secs(1), async {
        while enumerations.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("initial discovery");
    let initial = enumerations.load(Ordering::SeqCst);

    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (vec![7], vec![999], false);
    notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while inspections.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("targeted inspection");
    assert_eq!(enumerations.load(Ordering::SeqCst), initial);
    assert_eq!(inspections.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.targeted_inspection_count(), 1);
    assert!(runtime.manager().read().await.instances().contains_key(&7));

    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (Vec::new(), vec![7], false);
    notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let pending = event_batch
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .1
                .is_empty();
            if pending {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("live exit revalidation");
    assert!(runtime.manager().read().await.instances().contains_key(&7));

    alive.store(false, Ordering::SeqCst);
    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (Vec::new(), vec![7], false);
    notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.manager().read().await.instances().contains_key(&7) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dead exit removal");

    runtime
        .manager()
        .read()
        .await
        .lifecycle_changed
        .notify_one();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(enumerations.load(Ordering::SeqCst), initial);

    *event_batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = (Vec::new(), Vec::new(), true);
    notify.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while enumerations.load(Ordering::SeqCst) == initial {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("loss reconciliation");
    assert_eq!(enumerations.load(Ordering::SeqCst), initial + 1);

    notify.notify_one();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(enumerations.load(Ordering::SeqCst), initial + 1);
    runtime.shutdown().await;
}

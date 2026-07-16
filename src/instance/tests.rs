use super::*;
use crate::process::{MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};
use std::sync::atomic::AtomicUsize;

#[derive(Debug)]
struct FakeMemory {
    pid: Pid,
    alive: AtomicBool,
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

struct FakeProvider {
    platform: DiscoveryPlatform,
    processes: Vec<ProcessDescriptor>,
    drops: Arc<AtomicUsize>,
    foreground: Option<Pid>,
}
impl ProcessProvider for FakeProvider {
    fn platform(&self) -> DiscoveryPlatform {
        self.platform
    }
    fn enumerate(&self) -> Result<Vec<ProcessDescriptor>, MemoryError> {
        Ok(self.processes.clone())
    }
    fn open(&self, pid: Pid) -> Result<ProcessHandle, MemoryError> {
        Ok(ProcessHandle::new(
            Arc::new(FakeMemory {
                pid,
                alive: AtomicBool::new(true),
                drops: Arc::clone(&self.drops),
            }),
            ProcessBitness::X64,
        ))
    }
    fn foreground_pid(&self) -> Option<Pid> {
        self.foreground
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
        processes,
        drops: Arc::new(AtomicUsize::new(0)),
        foreground: None,
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

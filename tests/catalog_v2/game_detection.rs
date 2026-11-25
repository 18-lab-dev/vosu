//! M6b G0: catalog game offers driven by the actual core discovery service,
//! S0 contextual snapshots, the runtime matcher and P0 plans.
use semver::Version;
use serde_json::{Map, Value, json};
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use vosu_core::{
    catalog::{
        game::{GameOffers, InstalledPlugins, OfferError, PlanInputs},
        plans::{self, CatalogExecutor, CatalogOperation, PlanRegistry, Prerequisites, Reason},
        resolver::{BuiltinProviders, Candidate, Request, Snapshot},
        sources::{
            CandidateSnapshot, FetchedCatalog, SourceIdentity, SourceInput, SourceLifecycle,
            SourceStore, SourceStoreError,
        },
        trust::CurrentPolicy,
    },
    logging::{ConsoleSink, Logger, LoggerOptions},
    plugin::{
        DiscoveryObserver, DiscoveryService, MatchPlatform, PluginHost, Prefilter,
        ProcessEventBatch, ProcessRecord, ProcessSource, host_platform,
        settings::{PluginSettingsService, SettingsError, SettingsStorage},
    },
};
use vosu_plugin_api::{Bitness, manifest::PackageKind};

const TARGET: &str = "x86_64-unknown-linux-gnu";
const WAIT: Duration = Duration::from_secs(20);

struct Lifecycle;
impl SourceLifecycle for Lifecycle {
    fn reconcile_sources(&self, _: &[String]) -> Result<(), SourceStoreError> {
        Ok(())
    }
}
fn id(source: &str) -> SourceIdentity {
    SourceIdentity::parse(source).unwrap().identity
}
fn input(source: &str, enabled: bool) -> SourceInput {
    SourceInput {
        source: source.into(),
        enabled,
    }
}
fn game(plugin: &str, matcher: Value) -> Value {
    json!({"id":plugin,"name":format!("{plugin} support"),"version":"3.1.0","host":"^2","runtime":"process",
        "provides":[{"id":format!("field:{plugin}"),"version":"1.0.0","schema_version":"1.0.0","lanes":[]},
            {"id":"game:mygame","version":"*"}],
        "matcher":matcher,
        "packages":{TARGET:{"url":format!("https://packages.test/{plugin}.zip"),"sha256":"ef".repeat(32),"size":2048}}})
}
fn catalog(plugins: Vec<Value>) -> Vec<u8> {
    json!({"catalog_version":2,"name":"Games","published":"2026-11-24T00:00:00Z","plugins":plugins})
        .to_string()
        .into_bytes()
}
fn fetch(store: &SourceStore, results: Vec<(&str, Vec<u8>)>) -> Arc<CandidateSnapshot> {
    let ticket = store.begin_fetch().unwrap();
    let fetched = results
        .into_iter()
        .map(|(source, bytes)| FetchedCatalog {
            source: id(source),
            result: Ok(bytes),
        })
        .collect();
    store
        .complete_fetch(ticket, fetched, &[], &[], &Version::new(2, 1, 0), TARGET)
        .unwrap()
}
fn record(pid: u32, start_time: u64, name: &str) -> ProcessRecord {
    ProcessRecord {
        pid,
        name: name.into(),
        path: format!("/usr/lib/{name}/{name}"),
        command_line: format!("{name} --session-secret=argv-sentinel"),
        bitness: Bitness::X64,
        start_time,
        argv: None,
    }
}
fn listed(offers: &GameOffers) -> Vec<(String, String)> {
    offers
        .offers()
        .into_iter()
        .map(|view| (view.source.as_str().to_owned(), view.id))
        .collect()
}

/// Scripted [`ProcessSource`]: a forced reconcile per explicit `trigger`.
#[derive(Default)]
struct Source {
    processes: Mutex<Vec<ProcessRecord>>,
    prefilters: Mutex<Vec<Prefilter>>,
    reconcile: AtomicBool,
    notify: Notify,
}
impl Source {
    fn set(&self, processes: Vec<ProcessRecord>) {
        *self.processes.lock().unwrap() = processes;
        self.trigger();
    }
    fn trigger(&self) {
        self.reconcile.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
}
impl ProcessSource for Source {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        Ok(self.processes.lock().unwrap().clone())
    }
    fn drain_events(&self) -> ProcessEventBatch {
        ProcessEventBatch {
            needs_reconcile: self.reconcile.swap(false, Ordering::SeqCst),
            ..ProcessEventBatch::default()
        }
    }
    fn set_prefilter(&self, filter: &Prefilter) -> io::Result<()> {
        self.prefilters.lock().unwrap().push(filter.clone());
        Ok(())
    }
    fn notifier(&self) -> Option<&Notify> {
        Some(&self.notify)
    }
}

/// `Observed` carries the offers generation current *before* the delivery and
/// whether it was unchanged afterwards, proving which publication it used.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Observed(u32, u64, u64),
    Exited(u32),
}
/// Forwards to the real offers and acknowledges each delivered observation.
struct Acked {
    offers: Arc<GameOffers>,
    events: SyncSender<Seen>,
}
impl DiscoveryObserver for Acked {
    fn process_observed(&self, process: &ProcessRecord, claimed: bool) {
        let before = self.offers.generation();
        self.offers.process_observed(process, claimed);
        let generation = if self.offers.generation() == before {
            before
        } else {
            0
        };
        let _ = self
            .events
            .send(Seen::Observed(process.pid, process.start_time, generation));
    }
    fn process_exited(&self, pid: u32) {
        self.offers.process_exited(pid);
        let _ = self.events.send(Seen::Exited(pid));
    }
    fn prefilter_names(&self) -> Vec<String> {
        self.offers.prefilter_names()
    }
    fn generation(&self) -> u64 {
        self.offers.generation()
    }
}
fn await_event(events: &Receiver<Seen>, expected: Seen) {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(seen) if seen == expected => return,
            Ok(_) => {}
            Err(error) => panic!("discovery never delivered {expected:?}: {error}"),
        }
    }
}

struct Storage;
impl SettingsStorage for Storage {
    fn load(&self, _: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(Map::new())
    }
    fn store(&self, _: &str, _: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
}
struct Console;
impl ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn game_detection_existing_discovery_offers_and_invalidates_on_pid_reuse_source_and_install() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let _context = runtime.enter();
    let logs = tempfile::tempdir().unwrap();
    let logger = Logger::new(
        logs.path(),
        LoggerOptions {
            version: "0.1.0".into(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(Console),
    )
    .unwrap();
    let host = PluginHost::new(
        logger,
        Arc::new(PluginSettingsService::new(Arc::new(Storage))),
        runtime.handle().clone(),
    );
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[input("acme/games", true), input("other/games", true)])
        .unwrap();
    let matcher = json!({"process_names":["mygame"]});
    let acme = catalog(vec![game("acme.game", matcher.clone())]);
    let other = catalog(vec![game("other.game", matcher)]);
    let snapshot = fetch(
        &store,
        vec![("acme/games", acme.clone()), ("other/games", other)],
    );
    let none = InstalledPlugins {
        generation: 1,
        ids: &[],
    };
    let offers = Arc::new(GameOffers::new(host_platform()));
    offers
        .refresh(Some(snapshot.clone()), snapshot.generation(), &none)
        .unwrap();
    let (sender, events) = sync_channel(256);
    host.set_discovery_observer(Some(Arc::new(Acked {
        offers: offers.clone(),
        events: sender,
    })));
    let source = Arc::new(Source::default());
    *source.processes.lock().unwrap() = vec![record(100, 1, "mygame"), record(101, 1, "editor")];
    let service = DiscoveryService::start(&host, source.clone(), runtime.handle().clone());
    let current = offers.generation();
    await_event(&events, Seen::Observed(100, 1, current));
    await_event(&events, Seen::Observed(101, 1, current));
    assert_eq!(
        source.prefilters.lock().unwrap().first(),
        Some(&Prefilter::Names(vec!["mygame".into()]))
    );
    assert!(!host.has_enabled_matchers(), "catalog matcher registered");
    assert_eq!(
        listed(&offers),
        [
            ("acme/games".into(), "acme.game".into()),
            ("other/games".into(), "other.game".into()),
        ]
    );
    let old = offers.offers()[0].offer_id.clone();

    // PID reuse: discovery reports exit then the new occurrence.
    source.set(vec![record(100, 2, "mygame")]);
    await_event(&events, Seen::Exited(100));
    await_event(&events, Seen::Observed(100, 2, current));
    let renewed = offers.offers();
    assert_eq!(renewed.len(), 2);
    assert!(renewed.iter().all(|view| view.offer_id != old));

    // Disabling a source clears S0's snapshot and every live offer at once.
    store
        .replace(&[input("acme/games", true), input("other/games", false)])
        .unwrap();
    let generation = store.snapshot().unwrap().0;
    offers
        .refresh(store.candidate_snapshot().unwrap(), generation, &none)
        .unwrap();
    assert!(offers.offers().is_empty());
    let snapshot = fetch(&store, vec![("acme/games", acme)]);
    offers
        .refresh(Some(snapshot.clone()), snapshot.generation(), &none)
        .unwrap();
    // The interest generation change makes discovery re-offer tracked pids.
    source.trigger();
    await_event(&events, Seen::Observed(100, 2, offers.generation()));
    assert_eq!(listed(&offers), [("acme/games".into(), "acme.game".into())]);

    // Installing the offered plugin removes it from offers.
    let installed = InstalledPlugins {
        generation: 2,
        ids: &["acme.game"],
    };
    offers
        .refresh(Some(snapshot.clone()), snapshot.generation(), &installed)
        .unwrap();
    source.trigger();
    await_event(&events, Seen::Observed(100, 2, offers.generation()));
    assert!(offers.offers().is_empty());
    assert!(offers.prefilter_names().is_empty());

    source.set(Vec::new());
    await_event(&events, Seen::Exited(100));
    service.stop();
}

#[test]
fn game_detection_linux_comm_hint_is_not_a_match_and_overlaps_are_valid() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store.replace(&[input("acme/games", true)]).unwrap();
    let snapshot = fetch(
        &store,
        vec![(
            "acme/games",
            catalog(vec![
                game("acme.hint", json!({"comm_hints":["mygame"]})),
                game("acme.name", json!({"linux_name_contains":["mygame"]})),
                game("acme.exact", json!({"process_names":["mygame"]})),
            ]),
        )],
    );
    assert!(snapshot.sources()[0].catalog().is_ok(), "overlap rejected");
    let offers = GameOffers::new(MatchPlatform::Linux);
    offers
        .refresh(
            Some(snapshot.clone()),
            snapshot.generation(),
            &InstalledPlugins {
                generation: 0,
                ids: &[],
            },
        )
        .unwrap();
    // Linux name/comm observations: only userspace matchers accept.
    offers.process_observed(&record(200, 9, "mygame"), false);
    offers.process_observed(&record(201, 9, "mygame-x"), false);
    assert_eq!(
        listed(&offers)
            .into_iter()
            .map(|(_, plugin)| plugin)
            .collect::<Vec<_>>(),
        ["acme.name", "acme.exact", "acme.name"]
    );
    let serialized = serde_json::to_string(&offers.offers()).unwrap();
    assert!(!serialized.contains("argv-sentinel") && !serialized.contains("/usr/lib"));
}

#[derive(Default)]
struct Executor(Mutex<Vec<CatalogOperation>>);
impl CatalogExecutor for Executor {
    fn start(&self, operation: CatalogOperation) -> Result<(), String> {
        self.0.lock().unwrap().push(operation);
        Ok(())
    }
}

#[test]
fn game_detection_acceptance_is_requested_with_exact_source_and_downloads_after_confirm() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[input("first/games", true), input("chosen/games", true)])
        .unwrap();
    let matcher = json!({"process_names":["mygame"]});
    let tool = json!({"id":"mygame-tool","name":"Tool","version":"1.0.0","host":"^2",
        "requires":[{"id":"field:shared.game","version":"^1"}],
        "packages":{"any":{"url":"https://packages.test/tool.zip","sha256":"12".repeat(32),"size":64}}});
    let mut chosen = json!({"catalog_version":2,"name":"Chosen","published":"2026-11-24T00:00:00Z",
        "plugins":[game("shared.game", matcher.clone())],"tools":[tool]});
    chosen["plugins"][0]["version"] = json!("2.0.0");
    let snapshot = fetch(
        &store,
        vec![
            ("first/games", catalog(vec![game("shared.game", matcher)])),
            ("chosen/games", chosen.to_string().into_bytes()),
        ],
    );
    let records: Vec<_> = snapshot
        .sources()
        .iter()
        .map(|s| s.source().clone())
        .collect();
    let candidates: Vec<_> = snapshot
        .candidates()
        .map(|c| Candidate {
            kind: c.kind,
            entry: c.entry,
            source: &c.source.identity,
            source_order: c.source.order,
            catalog_order: c.catalog_index,
            trusted: c.source.trusted,
        })
        .collect();
    let builtins = BuiltinProviders::from_registry(
        &vosu_core::state::schema::SchemaRegistry::with_core().unwrap(),
        &[],
    );
    let application = Version::new(2, 1, 0);
    let resolver = Snapshot {
        candidates: &candidates,
        installed: &[],
        builtins: &builtins,
        application: &application,
        target: TARGET,
        max_package_bytes: 1 << 20,
    };
    let policy = CurrentPolicy {
        generation: snapshot.generation(),
        sources: &records,
    };
    let prerequisites = || Prerequisites {
        source_generation: snapshot.generation().0,
        trust_generation: 41,
        private_generation: 42,
        credential_generation: 43,
        installed_generation: 44,
        limit_generation: 45,
        snapshot_fingerprint: [46; 32],
        application: "2.1.0".into(),
        target: TARGET.into(),
        max_package_bytes: 1 << 20,
    };
    let registry = PlanRegistry::new();
    let control = registry
        .insert(plans::build(
            &resolver,
            &[Request {
                kind: PackageKind::Tool,
                id: "mygame-tool",
                source: &id("chosen/games"),
            }],
            &policy,
            prerequisites(),
        ))
        .unwrap();
    // Control: the first-ranked source supplies shared.game as a dependency.
    assert_eq!(
        (
            control.adds[0].source.as_str(),
            control.adds[0].reason,
            control.adds[0].version.as_str()
        ),
        ("first/games", Reason::Dependency, "3.1.0")
    );
    let offers = GameOffers::new(MatchPlatform::Linux);
    offers
        .refresh(
            Some(snapshot.clone()),
            snapshot.generation(),
            &InstalledPlugins {
                generation: 44,
                ids: &[],
            },
        )
        .unwrap();
    offers.process_observed(&record(300, 4, "mygame"), false);
    let chosen = offers
        .offers()
        .into_iter()
        .find(|view| view.source.as_str() == "chosen/games")
        .unwrap();
    let executor = Executor::default();
    let inputs = || PlanInputs {
        snapshot: &resolver,
        policy: &policy,
        prerequisites: prerequisites(),
        registry: &registry,
    };
    let plan = offers.accept(&chosen.offer_id, inputs()).unwrap();
    assert_eq!(plan.adds.len(), 1);
    let add = &plan.adds[0];
    assert_eq!(
        (
            add.id.as_str(),
            add.source.as_str(),
            add.version.as_str(),
            add.reason
        ),
        ("shared.game", "chosen/games", "2.0.0", Reason::Requested)
    );
    assert!(add.needs_approval, "process match granted approval");
    assert!(executor.0.lock().unwrap().is_empty(), "accept downloaded");
    offers.process_exited(300);
    assert_eq!(
        offers.accept(&chosen.offer_id, inputs()).err(),
        Some(OfferError::NotFound)
    );
    registry
        .confirm(
            &plan.plan_id,
            &["shared.game".into()],
            prerequisites,
            &executor,
        )
        .unwrap();
    let operations = executor.0.lock().unwrap();
    let pinned = &operations[0].confirmed().unwrap().plan.adds()[0];
    assert_eq!(
        (pinned.reason, pinned.source.as_str()),
        (Reason::Requested, "chosen/games")
    );
}

use super::*;
use crate::{
    catalog::{
        plans::{CatalogExecutor, CatalogOperation, PlanRegistry, Prerequisites, Reason},
        resolver::{BuiltinProviders, Candidate, Snapshot},
        sources::{
            CandidateSnapshot, FetchedCatalog, SourceFailure, SourceGeneration, SourceIdentity,
            SourceInput, SourceLifecycle, SourceStore, SourceStoreError,
        },
        trust::CurrentPolicy,
    },
    logging::{ConsoleSink, Logger, LoggerOptions},
    plugin::{
        DiscoveryObserver, MatchPlatform, OBSERVER_PREFILTER_BUDGET, PluginHost, Prefilter,
        ProcessRecord, SyntheticForegroundSource,
        settings::{PluginSettingsService, SettingsError, SettingsStorage},
    },
    state::schema::SchemaRegistry,
};
use semver::Version;
use serde_json::{Map, Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use vosu_plugin_api::{Bitness, manifest::PackageKind};

const TARGET: &str = "x86_64-unknown-linux-gnu";
const NONE: InstalledPlugins<'static> = InstalledPlugins {
    generation: 7,
    ids: &[],
};

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
/// A game plugin entry; `game` is the shared `game:` capability.
fn game(plugin: &str, game: &str, matcher: Value) -> Value {
    json!({"id":plugin,"name":format!("{plugin} support"),"version":"1.4.2","host":"^2","runtime":"process",
        "provides":[{"id":format!("field:{plugin}"),"version":"1.0.0","schema_version":"1.0.0","lanes":[]},
            {"id":format!("game:{game}"),"version":"*"}],
        "matcher":matcher,
        "packages":{TARGET:{"url":format!("https://packages.test/{plugin}.zip"),"sha256":"ab".repeat(32),"size":4096}}})
}
fn catalog(plugins: Vec<Value>, tools: Vec<Value>) -> Vec<u8> {
    json!({"catalog_version":2,"name":"Games","published":"2026-11-24T00:00:00Z","plugins":plugins,"tools":tools})
        .to_string()
        .into_bytes()
}
struct Sources {
    _root: tempfile::TempDir,
    store: SourceStore,
}
impl Sources {
    fn new(inputs: &[SourceInput]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
        store.replace(inputs).unwrap();
        Self { _root: root, store }
    }
    fn fetch(&self, results: Vec<(&str, Vec<u8>)>) -> Arc<CandidateSnapshot> {
        let ticket = self.store.begin_fetch().unwrap();
        let fetched = results
            .into_iter()
            .map(|(source, bytes)| FetchedCatalog {
                source: id(source),
                result: Ok(bytes),
            })
            .collect();
        self.store
            .complete_fetch(ticket, fetched, &[], &[], &Version::new(2, 1, 0), TARGET)
            .unwrap()
    }
    fn generation(&self) -> SourceGeneration {
        self.store.snapshot().unwrap().0
    }
}
fn process(pid: u32, start_time: u64, name: &str) -> ProcessRecord {
    ProcessRecord {
        pid,
        name: name.into(),
        path: format!("/opt/games/{name}"),
        command_line: format!("/opt/games/{name} --secret-token=sentinel-argv"),
        bitness: Bitness::X64,
        start_time,
        argv: Some(vec![format!("/opt/games/{name}")]),
    }
}
fn offered(offers: &GameOffers) -> Vec<(String, String)> {
    offers
        .offers()
        .into_iter()
        .map(|view| (view.source.as_str().to_owned(), view.id))
        .collect()
}
fn linux_offers(snapshot: &Arc<CandidateSnapshot>, installed: &InstalledPlugins<'_>) -> GameOffers {
    let offers = GameOffers::new(MatchPlatform::Linux);
    offers
        .refresh(Some(snapshot.clone()), snapshot.generation(), installed)
        .unwrap();
    offers
}

#[test]
fn catalog_comm_hint_alone_is_prefilter_interest_never_a_userspace_match() {
    let sources = Sources::new(&[input("acme/games", true)]);
    let snapshot = sources.fetch(vec![(
        "acme/games",
        catalog(
            vec![
                game("acme.hinted", "hinted", json!({"comm_hints":["mygame"]})),
                game(
                    "acme.named",
                    "named",
                    json!({"linux_name_contains":["named"]}),
                ),
            ],
            vec![],
        ),
    )]);
    let offers = linux_offers(&snapshot, &NONE);
    assert_eq!(
        offers.prefilter_names(),
        ["mygame".to_owned(), "named".to_owned()]
    );
    offers.process_observed(&process(10, 1, "mygame"), false);
    assert!(offers.offers().is_empty(), "comm hint accepted a process");
    // Positive control: a Linux name/comm substring userspace matcher.
    offers.process_observed(&process(11, 1, "xnamed-bin"), false);
    assert_eq!(
        offered(&offers),
        [("acme/games".into(), "acme.named".into())]
    );
    // Windows ignores linux_name_contains (portable semantics, native pending).
    let windows = GameOffers::new(MatchPlatform::Windows);
    windows
        .refresh(Some(snapshot.clone()), snapshot.generation(), &NONE)
        .unwrap();
    windows.process_observed(&process(11, 1, "xnamed-bin"), false);
    windows.process_observed(&process(10, 1, "mygame"), false);
    assert!(windows.offers().is_empty());
}

#[test]
fn linux_process_names_are_exact_and_case_sensitive_and_view_is_redacted() {
    let sources = Sources::new(&[input("acme/games", true)]);
    let snapshot = sources.fetch(vec![(
        "acme/games",
        catalog(
            vec![game(
                "acme.exact",
                "exact",
                json!({"process_names":["MyGame"]}),
            )],
            vec![],
        ),
    )]);
    let offers = linux_offers(&snapshot, &NONE);
    offers.process_observed(&process(20, 1, "mygame"), false);
    offers.process_observed(&process(21, 1, "MyGame2"), false);
    assert!(offers.offers().is_empty());
    offers.process_observed(&process(22, 1, "MyGame"), false);
    let views = offers.offers();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].process, "MyGame");
    assert_eq!(views[0].name, "acme.exact support");
    assert_eq!(views[0].version, "1.4.2");
    let serialized = serde_json::to_string(&views).unwrap();
    for leaked in ["/opt/games", "sentinel-argv", "--secret", "\"22\""] {
        assert!(
            !serialized.contains(leaked),
            "{leaked} leaked: {serialized}"
        );
    }
}

#[test]
fn overlapping_uninstalled_alternatives_pass_validation_and_both_offer() {
    let matcher = json!({"process_names":["mygame"],"comm_hints":["mygame"]});
    let sources = Sources::new(&[input("acme/games", true), input("other/games", true)]);
    let snapshot = sources.fetch(vec![
        (
            "acme/games",
            catalog(
                vec![
                    game("acme.game", "mygame", matcher.clone()),
                    game("acme.alt", "mygame", matcher.clone()),
                ],
                vec![],
            ),
        ),
        (
            "other/games",
            catalog(vec![game("other.game", "mygame", matcher.clone())], vec![]),
        ),
    ]);
    assert!(snapshot.sources().iter().all(|s| s.catalog().is_ok()));
    // These matchers really overlap under the installed D18 rule.
    let table: vosu_plugin_api::MatchTable = serde_json::from_value(matcher).unwrap();
    assert!(crate::plugin::overlaps(
        MatchPlatform::Linux,
        &table,
        &table
    ));
    let offers = linux_offers(&snapshot, &NONE);
    offers.process_observed(&process(30, 5, "mygame"), false);
    offers.process_observed(&process(30, 5, "mygame"), false);
    assert_eq!(
        offered(&offers),
        [
            ("acme/games".into(), "acme.game".into()),
            ("acme/games".into(), "acme.alt".into()),
            ("other/games".into(), "other.game".into()),
        ]
    );
}

#[test]
fn reused_pid_exit_rename_and_installed_match_flags_offers() {
    let sources = Sources::new(&[input("acme/games", true)]);
    let snapshot = sources.fetch(vec![(
        "acme/games",
        catalog(
            vec![game(
                "acme.game",
                "mygame",
                json!({"process_names":["mygame"]}),
            )],
            vec![],
        ),
    )]);
    let offers = linux_offers(&snapshot, &NONE);
    offers.process_observed(&process(40, 100, "mygame"), false);
    let first = offers.offers()[0].offer_id.clone();
    // Same pid, new start time, different program: the old offer must go.
    offers.process_observed(&process(40, 200, "shell"), false);
    assert!(offers.offers().is_empty());
    offers.process_observed(&process(40, 300, "mygame"), false);
    let second = offers.offers()[0].offer_id.clone();
    assert_ne!(first, second);
    offers.process_exited(40);
    assert!(offers.offers().is_empty());
    // An installed enabled plugin already matched this process: the spec still
    // offers the alternative, flagged for presentation.
    offers.process_observed(&process(41, 1, "mygame"), true);
    let views = offers.offers();
    assert_eq!(views.len(), 1);
    assert!(views[0].installed_match);
    let flagged = views[0].offer_id.clone();
    offers.process_observed(&process(41, 1, "mygame"), false);
    let views = offers.offers();
    assert_eq!((views.len(), views[0].installed_match), (1, false));
    assert_eq!(views[0].offer_id, flagged, "same occurrence deduplicated");
    // An in-place rename of the same occurrence that stops matching.
    offers.process_observed(&process(41, 1, "renamed"), false);
    assert!(offers.offers().is_empty());
}

#[test]
fn disabled_source_or_installed_id_never_offers_and_publication_invalidates() {
    let sources = Sources::new(&[input("acme/games", true), input("other/games", true)]);
    let bytes = |plugin: &str| {
        catalog(
            vec![game(plugin, "mygame", json!({"process_names":["mygame"]}))],
            vec![],
        )
    };
    let snapshot = sources.fetch(vec![
        ("acme/games", bytes("acme.game")),
        ("other/games", bytes("other.game")),
    ]);
    let offers = linux_offers(&snapshot, &NONE);
    offers.process_observed(&process(50, 1, "mygame"), false);
    assert_eq!(offers.offers().len(), 2);
    let generation = offers.generation();
    sources
        .store
        .replace(&[input("acme/games", true), input("other/games", false)])
        .unwrap();
    assert!(sources.store.candidate_snapshot().unwrap().is_none());
    offers.refresh(None, sources.generation(), &NONE).unwrap();
    assert!(offers.offers().is_empty());
    assert!(offers.prefilter_names().is_empty());
    assert!(offers.generation() > generation);
    let snapshot = sources.fetch(vec![("acme/games", bytes("acme.game"))]);
    assert_eq!(
        snapshot.sources()[1].catalog().unwrap_err(),
        SourceFailure::Disabled
    );
    offers
        .refresh(Some(snapshot.clone()), snapshot.generation(), &NONE)
        .unwrap();
    offers.process_observed(&process(50, 1, "mygame"), false);
    assert_eq!(
        offered(&offers),
        [("acme/games".into(), "acme.game".into())]
    );
    // Installing the plugin (new installed generation) clears and excludes it.
    let installed = InstalledPlugins {
        generation: 8,
        ids: &["acme.game"],
    };
    offers
        .refresh(Some(snapshot.clone()), snapshot.generation(), &installed)
        .unwrap();
    offers.process_observed(&process(50, 1, "mygame"), false);
    assert!(offers.offers().is_empty());
    // Older publications are refused rather than resurrecting offers.
    assert_eq!(
        offers.refresh(Some(snapshot.clone()), snapshot.generation(), &NONE),
        Err(OfferError::Stale)
    );
    assert_eq!(
        offers.refresh(None, SourceGeneration(0), &installed),
        Err(OfferError::Stale)
    );
}

#[test]
fn offers_are_bounded_without_eviction() {
    let names: Vec<String> = (0..70).map(|i| format!("game{i:02}")).collect();
    let plugins = names
        .iter()
        .map(|name| {
            game(
                &format!("acme.{name}"),
                name,
                json!({"process_names":[name]}),
            )
        })
        .collect();
    let sources = Sources::new(&[input("acme/games", true)]);
    let snapshot = sources.fetch(vec![("acme/games", catalog(plugins, vec![]))]);
    let offers = linux_offers(&snapshot, &NONE);
    for (pid, name) in (1..).zip(&names) {
        offers.process_observed(&process(pid, 1, name), false);
    }
    assert_eq!(offers.offers().len(), MAX_GAME_OFFERS);
    assert_eq!(offers.offers()[0].id, "acme.game00");
    offers.process_exited(1);
    offers.process_observed(&process(70, 1, "game69"), false);
    assert_eq!(offers.offers().len(), MAX_GAME_OFFERS);
    assert_eq!(offers.offers().last().unwrap().id, "acme.game69");
}

#[test]
fn catalog_interest_is_bounded_never_widens_and_never_displaces_installed() {
    // One hint-less (path-only) entry plus 300 single-hint entries.
    let mut plugins = vec![game(
        "acme.pathonly",
        "pathonly",
        json!({"process_path_patterns":["/opt/games/pathonly"]}),
    )];
    plugins.extend((0..300).map(|i| {
        game(
            &format!("acme.h{i:03}"),
            &format!("h{i:03}"),
            json!({"comm_hints":[format!("h{i:03}")]}),
        )
    }));
    let sources = Sources::new(&[input("acme/games", true)]);
    let snapshot = sources.fetch(vec![("acme/games", catalog(plugins, vec![]))]);
    let offers = linux_offers(&snapshot, &NONE);
    let interest = offers.prefilter_names();
    let expected: Vec<String> = (0..OBSERVER_PREFILTER_BUDGET)
        .map(|i| format!("h{i:03}"))
        .collect();
    assert_eq!(
        interest, expected,
        "first entries within the catalog budget"
    );
    for (platform, installed_count, total) in [
        (MatchPlatform::Linux, 10, 10 + OBSERVER_PREFILTER_BUDGET),
        // Only the kernel/CN_PROC capacity installed plugins leave free.
        (MatchPlatform::Linux, 230, 256),
        (MatchPlatform::Linux, 256, 256),
        (MatchPlatform::Windows, 230, 230 + OBSERVER_PREFILTER_BUDGET),
    ] {
        let installed: Vec<String> = (0..installed_count).map(|i| format!("i{i:03}")).collect();
        let Prefilter::Names(merged) =
            Prefilter::Names(installed.clone()).union(&interest, platform)
        else {
            panic!("catalog interest widened the {platform:?} pre-filter");
        };
        assert_eq!(merged.len(), total, "{platform:?}/{installed_count}");
        assert!(installed.iter().all(|name| merged.contains(name)));
    }
    // Invalid observer names never reach the kernel table.
    let invalid = [
        "".to_owned(),
        "a\0b".to_owned(),
        "sixteen-bytes-xx".to_owned(),
    ];
    assert_eq!(
        Prefilter::Names(Vec::new()).union(&invalid, MatchPlatform::Linux),
        Prefilter::Names(Vec::new())
    );
    // Installed-side rules are unchanged: unfiltered or >256 still widens.
    let excess: Vec<String> = (0..257).map(|i| format!("i{i:03}")).collect();
    assert_eq!(
        Prefilter::Names(excess).union(&[], MatchPlatform::Linux),
        Prefilter::Unfiltered
    );
    assert_eq!(
        Prefilter::Unfiltered.union(&interest, MatchPlatform::Linux),
        Prefilter::Unfiltered
    );
    let mut engine = crate::plugin::MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register(
            "installed.pathonly".into(),
            vosu_plugin_api::MatchTable {
                process_path_patterns: vec!["/opt/x".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(engine.prefilter(), Prefilter::Unfiltered);
    // Degraded but working: the hint-less entry still matches a process
    // discovery observes for another reason.
    offers.process_observed(&process(80, 1, "pathonly"), false);
    offers.process_observed(&process(81, 1, "anything"), false);
    assert_eq!(
        offered(&offers),
        [("acme/games".into(), "acme.pathonly".into())]
    );
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
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn host_seam_forwards_existing_observations_without_registering_matchers() {
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
    let host = PluginHost::new_for_tests(
        logger,
        Arc::new(PluginSettingsService::new(Arc::new(Storage))),
        runtime.handle().clone(),
        Arc::new(SyntheticForegroundSource::new(None)),
        Duration::from_secs(5),
    );
    let sources = Sources::new(&[input("acme/games", true)]);
    let snapshot = sources.fetch(vec![(
        "acme/games",
        catalog(
            vec![game(
                "acme.game",
                "mygame",
                json!({"process_names":["mygame"]}),
            )],
            vec![],
        ),
    )]);
    let offers = Arc::new(GameOffers::new(crate::plugin::host_platform()));
    let before = host.discovery_generation();
    host.set_discovery_observer(Some(offers.clone()));
    assert_ne!(host.discovery_generation(), before);
    assert_eq!(host.prefilter(), Prefilter::Names(Vec::new()));
    let observed = host.discovery_generation();
    offers
        .refresh(Some(snapshot.clone()), snapshot.generation(), &NONE)
        .unwrap();
    assert_ne!(host.discovery_generation(), observed);
    assert_eq!(host.prefilter(), Prefilter::Names(vec!["mygame".into()]));
    host.process_appeared(process(60, 1, "mygame"));
    assert_eq!(offers.offers().len(), 1);
    assert!(
        !host.has_enabled_matchers(),
        "catalog matcher was registered"
    );
    let first = offers.offers()[0].offer_id.clone();
    // Discovery reports a reused pid as exit then appearance.
    host.process_exited(60);
    host.process_appeared(process(60, 2, "mygame"));
    let views = offers.offers();
    assert_eq!(views.len(), 1);
    assert_ne!(views[0].offer_id, first);
    host.set_discovery_observer(None);
    host.process_exited(60);
    assert_eq!(offers.offers().len(), 1, "removed observer still notified");
}

#[derive(Default)]
struct Executor(Mutex<Vec<CatalogOperation>>);
impl CatalogExecutor for Executor {
    fn start(&self, operation: CatalogOperation) -> Result<(), String> {
        self.0.lock().unwrap().push(operation);
        Ok(())
    }
}
fn prerequisites(source: SourceGeneration, installed: u64) -> Prerequisites {
    Prerequisites {
        source_generation: source.0,
        trust_generation: 3,
        private_generation: 4,
        credential_generation: 5,
        installed_generation: installed,
        limit_generation: 6,
        snapshot_fingerprint: [9; 32],
        application: "2.1.0".into(),
        target: TARGET.into(),
        max_package_bytes: 1_048_576,
    }
}

#[test]
fn acceptance_is_a_requested_plan_and_nothing_downloads_before_confirmation() {
    let tool = json!({"id":"acme-overlay-tool","name":"Tool","version":"2.0.0","host":"^2",
        "requires":[{"id":"field:acme.game","version":"^1"}],
        "packages":{"any":{"url":"https://packages.test/tool.zip","sha256":"cd".repeat(32),"size":64}}});
    let sources = Sources::new(&[input("other/games", true), input("acme/games", true)]);
    let matcher = json!({"process_names":["mygame"]});
    let snapshot = sources.fetch(vec![
        (
            "other/games",
            catalog(vec![game("other.game", "mygame", matcher.clone())], vec![]),
        ),
        (
            "acme/games",
            catalog(vec![game("acme.game", "mygame", matcher)], vec![tool]),
        ),
    ]);
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
    let builtins = BuiltinProviders::from_registry(&SchemaRegistry::with_core().unwrap(), &[]);
    let application = Version::new(2, 1, 0);
    let resolver = Snapshot {
        candidates: &candidates,
        installed: &[],
        builtins: &builtins,
        application: &application,
        target: TARGET,
        max_package_bytes: 1_048_576,
    };
    let policy = CurrentPolicy {
        generation: snapshot.generation(),
        sources: &records,
    };
    let registry = PlanRegistry::new();
    let executor = Executor::default();
    // Discriminating control: the same plugin added for another request is
    // a dependency, so a wrong acceptance request would be visible.
    let control = registry
        .insert(crate::catalog::plans::build(
            &resolver,
            &[crate::catalog::resolver::Request {
                kind: PackageKind::Tool,
                id: "acme-overlay-tool",
                source: &id("acme/games"),
            }],
            &policy,
            prerequisites(snapshot.generation(), 7),
        ))
        .unwrap();
    assert_eq!(control.adds[0].id, "acme.game");
    assert_eq!(control.adds[0].reason, Reason::Dependency);

    let offers = linux_offers(&snapshot, &NONE);
    offers.process_observed(&process(70, 1, "mygame"), false);
    let views = offers.offers();
    assert_eq!(views.len(), 2);
    let acme = views.iter().find(|v| v.id == "acme.game").unwrap();
    assert!(executor.0.lock().unwrap().is_empty());
    let inputs = |installed| PlanInputs {
        snapshot: &resolver,
        policy: &policy,
        prerequisites: prerequisites(snapshot.generation(), installed),
        registry: &registry,
    };
    assert_eq!(
        offers.accept(&acme.offer_id, inputs(8)).err(),
        Some(OfferError::Stale)
    );
    assert_eq!(
        offers.accept("missing", inputs(7)).err(),
        Some(OfferError::NotFound)
    );
    let plan = offers.accept(&acme.offer_id, inputs(7)).unwrap();
    assert!(plan.errors.is_empty(), "{:?}", plan.errors);
    assert_eq!(plan.adds.len(), 1);
    let add = &plan.adds[0];
    assert_eq!(
        (add.id.as_str(), add.source.as_str(), add.reason),
        ("acme.game", "acme/games", Reason::Requested)
    );
    assert!(
        add.native && add.needs_approval,
        "no automatic approval/trust"
    );
    assert_eq!(add.trust, Some(crate::catalog::trust::TrustTier::User));
    assert!(executor.0.lock().unwrap().is_empty(), "accept started work");
    // Explicit approval is still required at confirmation.
    let current = || prerequisites(snapshot.generation(), 7);
    assert!(
        registry
            .confirm(&plan.plan_id, &[], current, &executor)
            .is_err()
    );
    registry
        .confirm(&plan.plan_id, &["acme.game".into()], current, &executor)
        .unwrap();
    let operations = executor.0.lock().unwrap();
    assert_eq!(operations.len(), 1);
    let pinned = &operations[0].confirmed().unwrap().plan.adds()[0];
    assert_eq!(pinned.reason, Reason::Requested);
    assert_eq!(pinned.source, id("acme/games"));
    assert_eq!(
        pinned.entry.packages[TARGET].url,
        "https://packages.test/acme.game.zip"
    );
}

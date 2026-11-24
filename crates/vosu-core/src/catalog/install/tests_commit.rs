//! Publication boundaries: injected crash/failure at each journal step,
//! recovery on the next open, and no-overwrite safety.
use super::commit::{FaultPoint, Faults};
use super::tests::*;
use super::*;
use serde_json::json;

fn crash(point: FaultPoint) -> Faults {
    Faults {
        crash: Some(point),
        fail: None,
    }
}

async fn graph_with(fault: Faults) -> Fixture {
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let (dependency, root) = dependency_graph(&fixture.server);
    let catalog = catalog(&[&root, &dependency], &[]);
    fixture.executor.inject(fault);
    let job = fixture
        .install(
            plan(
                &catalog,
                &record(OFFICIAL, true),
                PackageKind::Plugin,
                "acme.root",
            ),
            &[],
        )
        .await;
    assert_eq!(job.status, "failed");
    fixture
}

fn names(path: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(path)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test]
async fn crash_between_directory_moves_and_records_rolls_back_on_next_open() {
    let fixture = graph_with(crash(FaultPoint::DirectoryMoved(1))).await;
    // Both trees were published but the origin records were not.
    assert!(fixture.layout.state.join(JOURNAL_FILE).exists());
    assert_eq!(names(&fixture.layout.plugins), ["acme.dep", "acme.root"]);
    assert!(!fixture.layout.state.join(RECORDS_FILE).exists());
    // No load ever began for the uncommitted graph.
    assert!(fixture.recorder.calls.lock().unwrap().is_empty());
    assert_eq!(recover(&fixture.layout), Ok(Resolution::RolledBack));
    assert!(names(&fixture.layout.plugins).is_empty());
    assert!(!fixture.layout.state.join(JOURNAL_FILE).exists());
    let store = InstalledStore::open(fixture.layout.clone()).unwrap();
    assert_eq!(store.snapshot().records.len(), 0);
}

#[tokio::test]
async fn crash_after_first_directory_move_rolls_back_partial_graph() {
    let fixture = graph_with(crash(FaultPoint::DirectoryMoved(0))).await;
    let mut published = names(&fixture.layout.plugins);
    published.retain(|name| !name.starts_with('.'));
    assert_eq!(
        published,
        ["acme.dep"],
        "dependency exposed, root still staged"
    );
    // Recovery runs inside open, before any discovery can read records.
    let store = InstalledStore::open(fixture.layout.clone()).unwrap();
    assert!(store.snapshot().records.is_empty());
    assert!(names(&fixture.layout.plugins).is_empty());
}

#[tokio::test]
async fn crash_after_journal_publication_discards_all_staging() {
    let fixture = graph_with(crash(FaultPoint::JournalPublished)).await;
    assert_eq!(names(&fixture.layout.plugins).len(), 2, "two staged trees");
    assert!(
        names(&fixture.layout.plugins)
            .iter()
            .all(|name| name.starts_with(".stage-"))
    );
    assert_eq!(recover(&fixture.layout), Ok(Resolution::RolledBack));
    assert!(names(&fixture.layout.plugins).is_empty());
}

#[tokio::test]
async fn crash_after_records_publication_rolls_forward() {
    let fixture = graph_with(crash(FaultPoint::RecordsPublished)).await;
    assert!(fixture.layout.state.join(JOURNAL_FILE).exists());
    assert_eq!(recover(&fixture.layout), Ok(Resolution::RolledForward));
    let store = InstalledStore::open(fixture.layout.clone()).unwrap();
    let document = store.snapshot();
    assert_eq!(document.generation, 1);
    assert_eq!(document.records.len(), 2);
    assert_eq!(names(&fixture.layout.plugins), ["acme.dep", "acme.root"]);
    assert!(!fixture.layout.state.join(JOURNAL_FILE).exists());
}

#[tokio::test]
async fn injected_failure_rolls_back_in_process_without_load() {
    let fixture = graph_with(Faults {
        crash: None,
        fail: Some(FaultPoint::DirectoryMoved(1)),
    })
    .await;
    fixture.assert_untouched();
}

/// Install dep 1.0.0, then attempt 1.1.0 from the same source with `fault`.
async fn update_with(fault: Faults) -> (Fixture, CatalogJobResult) {
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let mut old = plugin("acme.dep", "1.0.0", "in-process", json!([]));
    old.publish(&fixture.server, "/dep.zip");
    let first = fixture
        .install(
            plan(
                &catalog(&[&old], &[]),
                &record(OFFICIAL, true),
                PackageKind::Plugin,
                "acme.dep",
            ),
            &[],
        )
        .await;
    assert_eq!(first.status, "completed", "{:?}", first.error);
    let mut new = plugin("acme.dep", "1.1.0", "in-process", json!([]));
    new.publish(&fixture.server, "/dep2.zip");
    fixture.executor.inject(fault);
    let job = fixture
        .install(
            plan(
                &catalog(&[&new], &[]),
                &record(OFFICIAL, true),
                PackageKind::Plugin,
                "acme.dep",
            ),
            &[],
        )
        .await;
    (fixture, (job.status, job.error))
}
type CatalogJobResult = (String, Option<String>);

fn installed_version(fixture: &Fixture) -> String {
    let text =
        std::fs::read_to_string(fixture.layout.plugins.join("acme.dep/manifest.toml")).unwrap();
    toml::from_str::<toml::Value>(&text).unwrap()["version"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn update_crash_after_backup_move_restores_previous_version() {
    for point in [FaultPoint::BackupMoved(0), FaultPoint::DirectoryMoved(0)] {
        let (fixture, (status, _)) = update_with(crash(point)).await;
        assert_eq!(status, "failed");
        assert_eq!(
            recover(&fixture.layout),
            Ok(Resolution::RolledBack),
            "{point:?}"
        );
        assert_eq!(installed_version(&fixture), "1.0.0", "{point:?}");
        assert_eq!(names(&fixture.layout.plugins), ["acme.dep"], "{point:?}");
        let store = InstalledStore::open(fixture.layout.clone()).unwrap();
        assert_eq!(store.snapshot().records[0].version, "1.0.0");
        assert_eq!(store.generation(), 1);
    }
}

#[tokio::test]
async fn update_failure_rolls_back_and_success_replaces_with_backup_removed() {
    let (fixture, (status, error)) = update_with(Faults {
        crash: None,
        fail: Some(FaultPoint::DirectoryMoved(0)),
    })
    .await;
    assert_eq!(status, "failed");
    assert_eq!(
        error.as_deref(),
        Some("filesystem operation failed: injected fault")
    );
    assert_eq!(installed_version(&fixture), "1.0.0");
    assert_eq!(fixture.records().generation, 1);
    let (fixture, (status, error)) = update_with(Faults::default()).await;
    assert_eq!(status, "completed", "{error:?}");
    assert_eq!(installed_version(&fixture), "1.1.0");
    assert_eq!(names(&fixture.layout.plugins), ["acme.dep"]);
    let document = fixture.records();
    assert_eq!(
        (document.generation, document.records[0].version.as_str()),
        (2, "1.1.0")
    );
}

#[tokio::test]
async fn unmanaged_and_foreign_destinations_are_never_overwritten() {
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let mut dependency = plugin("acme.dep", "1.0.0", "in-process", json!([]));
    dependency.publish(&fixture.server, "/dep.zip");
    let manual = fixture.layout.plugins.join("acme.dep");
    std::fs::create_dir_all(&manual).unwrap();
    std::fs::write(manual.join("user-file"), b"keep").unwrap();
    let job = fixture
        .install(
            plan(
                &catalog(&[&dependency], &[]),
                &record(OFFICIAL, true),
                PackageKind::Plugin,
                "acme.dep",
            ),
            &[],
        )
        .await;
    assert_eq!(
        job.error.as_deref(),
        Some("acme.dep exists but is not managed by the catalog; it is never overwritten")
    );
    assert_eq!(std::fs::read(manual.join("user-file")).unwrap(), b"keep");
    assert_eq!(names(&fixture.layout.plugins), ["acme.dep"]);
    assert!(!fixture.layout.state.join(RECORDS_FILE).exists());
}

#[test]
fn tampered_journal_and_invalid_records_are_refused_not_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path();
    let layout = InstallLayout::new(
        base.join("state"),
        base.join("plugins"),
        base.join("tools"),
        base.join("overlays"),
        base.join("downloads"),
    )
    .unwrap();
    let victim = base.join("victim");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::create_dir_all(&layout.state).unwrap();
    let journal = json!({"format":1,"previous_generation":0,"next_generation":1,
        "operations":[{"staging":base.join("plugins/.stage-x"),"destination":victim,"backup":null}]});
    std::fs::write(layout.state.join(JOURNAL_FILE), journal.to_string()).unwrap();
    assert_eq!(recover(&layout), Err(InstallError::RecoveryRequired));
    assert!(
        victim.is_dir(),
        "a path outside the package roots is never touched"
    );
    std::fs::remove_file(layout.state.join(JOURNAL_FILE)).unwrap();
    std::fs::write(layout.state.join(RECORDS_FILE), b"{not json").unwrap();
    assert!(matches!(
        InstalledStore::open(layout.clone()),
        Err(InstallError::RecordsInvalid)
    ));
    assert_eq!(
        std::fs::read(layout.state.join(RECORDS_FILE)).unwrap(),
        b"{not json"
    );
    // Overlapping or relative roots are a configuration error.
    assert_eq!(
        InstallLayout::new(
            base.join("s"),
            base.join("same"),
            base.join("same"),
            base.join("o"),
            base.join("d"),
        ),
        Err(InstallError::Layout)
    );
    assert!(layout.destination(PackageKind::Tool, "con").is_err());
    assert!(layout.destination(PackageKind::Tool, "../escape").is_err());
    assert!(
        layout
            .destination(PackageKind::Tool, "skill-tracker")
            .is_ok()
    );
}

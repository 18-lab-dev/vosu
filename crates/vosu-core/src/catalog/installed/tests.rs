//! U0 counterexamples over real committed directories, real manifests, I0's
//! records/journal and P0's gate. Expected closures are written out by hand,
//! independently of the planner.
use super::*;
use crate::catalog::{
    approvals::Artifact,
    install::{
        FaultPoint, Faults, HandoffFuture, InstallError, InstallLayout, InstalledDocument,
        InstalledFor, InstalledKind, InstalledRecord, InstalledStore, JOURNAL_FILE,
        publish_records,
    },
    plans::{CatalogJob, PlanRegistry},
    resolver::BuiltinProviders,
    sources::SourceIdentity,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use vosu_plugin_api::manifest::{PackageManifest, Runtime};

const TARGET: &str = "x86_64-unknown-linux-gnu";
const OFFICIAL: &str = "18-lab-dev/vosu-tools";
const USER: &str = "https://git.example.org/acme/catalog";
use InstalledFor::{Dependency as Dep, Requested as Req};

fn source(text: &str) -> SourceIdentity {
    SourceIdentity::parse(text).unwrap().identity
}
fn caps(list: &[&str]) -> Value {
    Value::Array(
        list.iter()
            .map(|entry| {
                let (id, version) = entry.split_once(' ').unwrap_or((entry, "1.0.0"));
                json!({"id":id,"version":version})
            })
            .collect(),
    )
}

/// One installed package: its manifest on disk plus its I0 record.
struct Pkg {
    kind: InstalledKind,
    id: &'static str,
    origin: InstalledFor,
    provides: Vec<&'static str>,
    requires: Vec<&'static str>,
    optional: Vec<&'static str>,
}
fn plugin(id: &'static str, origin: InstalledFor) -> Pkg {
    Pkg {
        kind: InstalledKind::Plugin,
        id,
        origin,
        provides: Vec::new(),
        requires: Vec::new(),
        optional: Vec::new(),
    }
}
fn tool(id: &'static str) -> Pkg {
    Pkg {
        kind: InstalledKind::Tool,
        ..plugin(id, Req)
    }
}
impl Pkg {
    fn provides(mut self, caps: &[&'static str]) -> Self {
        self.provides.extend(caps);
        self
    }
    fn requires(mut self, caps: &[&'static str]) -> Self {
        self.requires.extend(caps);
        self
    }
    fn optional(mut self, caps: &[&'static str]) -> Self {
        self.optional.extend(caps);
        self
    }
    fn manifest(&self) -> Value {
        let mut manifest = json!({"manifest_version":1,"id":self.id,"name":self.id,
            "version":"1.0.0","host":"^2","requires":caps(&self.requires),
            "optional":caps(&self.optional)});
        match self.kind {
            InstalledKind::Plugin => {
                manifest["kind"] = json!("plugin");
                manifest["abi_version"] = json!(1);
                manifest["runtime"] = json!("in-process");
                manifest["targets"] = json!({TARGET:{"library":"lib/fixture.so"}});
                let mut provides = caps(&self.provides);
                // A plugin's own whole namespace carries its schema (D7).
                for capability in provides.as_array_mut().unwrap() {
                    if capability["id"] == format!("field:{}", self.id) {
                        capability["schema"] = json!(format!("schemas/{}.json", self.id));
                    }
                }
                manifest["provides"] = provides;
            }
            InstalledKind::Tool => {
                manifest["kind"] = json!("tool");
                manifest["ui"] = json!({"root":"web"});
            }
            InstalledKind::Overlay => manifest["kind"] = json!("overlay"),
        }
        manifest
    }
    fn record(&self, index: usize) -> InstalledRecord {
        let sha256 = [u8::try_from(index + 1).unwrap(); 32];
        let identity = source(OFFICIAL);
        let artifact = (self.kind == InstalledKind::Plugin).then(|| Artifact {
            id: self.id.into(),
            source: identity.clone(),
            version: "1.0.0".into(),
            sha256,
            runtime: Runtime::InProcess,
            linux_user_namespaces: false,
        });
        InstalledRecord {
            kind: self.kind,
            id: self.id.into(),
            version: "1.0.0".into(),
            source: identity,
            target: TARGET.into(),
            sha256: sha256.iter().map(|b| format!("{b:02x}")).collect(),
            artifact,
            approval: None,
            installed_for: self.origin,
        }
    }
}

/// Records every runtime callback with the on-disk commit state it observed.
#[derive(Default)]
struct Recorder {
    layout: Mutex<Option<InstallLayout>>,
    data: Mutex<Option<PathBuf>>,
    events: Mutex<Vec<String>>,
    fail_stop: AtomicBool,
}
impl Recorder {
    fn present(&self, plan: &RemovalPlan) -> Vec<bool> {
        let layout = self.layout.lock().unwrap().clone().unwrap();
        plan.removals
            .iter()
            .map(|r| layout.destination(r.kind.into(), &r.id).unwrap().exists())
            .collect()
    }
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}
fn ids(plan: &RemovalPlan) -> String {
    plan.removals
        .iter()
        .map(|r| r.id.as_str())
        .collect::<Vec<_>>()
        .join(",")
}
impl RemovalRuntime for Recorder {
    fn stop<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, Result<(), String>> {
        let present = self.present(plan).iter().all(|p| *p);
        self.events
            .lock()
            .unwrap()
            .push(format!("stop {} installed={present}", ids(plan)));
        let fail = self.fail_stop.load(Ordering::SeqCst);
        Box::pin(async move {
            if fail {
                Err("instance refused to stop".into())
            } else {
                Ok(())
            }
        })
    }
    fn removed<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, ()> {
        let gone = self.present(plan).iter().all(|p| !*p);
        self.events
            .lock()
            .unwrap()
            .push(format!("removed {} gone={gone}", ids(plan)));
        Box::pin(async {})
    }
    fn remove_data<'a>(
        &'a self,
        kind: InstalledKind,
        id: &'a str,
    ) -> HandoffFuture<'a, Result<(), String>> {
        self.events
            .lock()
            .unwrap()
            .push(format!("data {kind:?} {id}"));
        let data = self.data.lock().unwrap().clone().unwrap().join(id);
        Box::pin(async move { std::fs::remove_dir_all(data).map_err(|e| e.kind().to_string()) })
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    layout: InstallLayout,
    data: PathBuf,
    recorder: Arc<Recorder>,
    registry: PlanRegistry,
    service: UninstallService,
}
fn builtins() -> BuiltinProviders {
    let bundled = PackageManifest::parse(
        &toml::to_string(
            &plugin("vosu.bundled", Req)
                .provides(&["field:bundled.feed"])
                .manifest(),
        )
        .unwrap(),
    )
    .unwrap();
    BuiltinProviders::from_registry(
        &crate::state::schema::SchemaRegistry::with_core().unwrap(),
        &[bundled],
    )
}
impl Fixture {
    fn new(packages: &[Pkg]) -> Self {
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
        let data = base.join("data");
        let mut records = Vec::new();
        for (index, package) in packages.iter().enumerate() {
            let directory = layout.destination(package.kind.into(), package.id).unwrap();
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("manifest.toml"),
                toml::to_string(&package.manifest()).unwrap(),
            )
            .unwrap();
            // Durable data/settings live outside every package root.
            std::fs::create_dir_all(data.join(package.id)).unwrap();
            std::fs::write(data.join(package.id).join("settings.json"), package.id).unwrap();
            records.push(package.record(index));
        }
        std::fs::create_dir_all(&layout.state).unwrap();
        publish_records(
            &layout,
            &InstalledDocument {
                format: 1,
                generation: 7,
                records,
            },
        )
        .unwrap();
        let recorder = Arc::new(Recorder::default());
        *recorder.layout.lock().unwrap() = Some(layout.clone());
        *recorder.data.lock().unwrap() = Some(data.clone());
        let service = UninstallService::new(
            Arc::new(InstalledStore::open(layout.clone()).unwrap()),
            Arc::new(builtins()),
            recorder.clone(),
            tokio::runtime::Handle::current(),
        );
        Self {
            _root: root,
            layout,
            data,
            recorder,
            registry: PlanRegistry::new(),
            service,
        }
    }
    fn request(&self, kind: InstalledKind, id: &str) -> UninstallRequest {
        UninstallRequest {
            kind,
            id: id.into(),
            source: source(OFFICIAL),
            remove_data: false,
        }
    }
    async fn uninstall(&self, request: UninstallRequest) -> Result<CatalogJob, UninstallError> {
        let response = self.service.start(&self.registry, request).await?;
        self.service.wait_idle().await;
        Ok(self.registry.job(&response.job_id).unwrap())
    }
    async fn remove(&self, id: &str) -> CatalogJob {
        let job = self
            .uninstall(self.request(InstalledKind::Plugin, id))
            .await
            .unwrap();
        assert_eq!((job.status.as_str(), &job.error), ("completed", &None));
        job
    }
    /// Reopened from disk, never the service's in-memory copy.
    fn installed(&self) -> Vec<(String, InstalledFor)> {
        let store = InstalledStore::open(self.layout.clone()).unwrap();
        store
            .snapshot()
            .records
            .iter()
            .map(|r| (r.id.clone(), r.installed_for))
            .collect()
    }
    fn ids(&self) -> Vec<String> {
        self.installed().into_iter().map(|(id, _)| id).collect()
    }
    fn directories(&self) -> Vec<String> {
        let mut names: Vec<_> = [&self.layout.plugins, &self.layout.tools]
            .iter()
            .flat_map(|root| std::fs::read_dir(root).into_iter().flatten())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
    fn generation(&self) -> u64 {
        InstalledStore::open(self.layout.clone())
            .unwrap()
            .generation()
    }
}

/// root → {left, right} → base, plus an independent requester of base.
fn diamond() -> Vec<Pkg> {
    vec![
        plugin("acme.root", Req).requires(&["field:acme.left", "field:acme.right"]),
        plugin("acme.left", Dep)
            .provides(&["field:acme.left"])
            .requires(&["field:acme.base"]),
        plugin("acme.right", Dep)
            .provides(&["field:acme.right"])
            .requires(&["field:acme.base"]),
        plugin("acme.base", Dep).provides(&["field:acme.base"]),
        plugin("acme.other", Req).requires(&["field:acme.base ^1"]),
    ]
}

#[tokio::test]
async fn diamond_base_survives_while_another_requirer_remains() {
    let fixture = Fixture::new(&diamond());
    fixture.remove("acme.root").await;
    assert_eq!(fixture.ids(), ["acme.base", "acme.other"]);
    assert_eq!(
        fixture.recorder.events(),
        [
            "stop acme.root,acme.left,acme.right installed=true",
            "removed acme.root,acme.left,acme.right gone=true",
        ]
    );
    fixture.remove("acme.other").await;
    assert!(fixture.ids().is_empty());
    assert!(fixture.directories().is_empty());
    // Two commits, each one generation.
    assert_eq!(fixture.generation(), 9);
}

#[tokio::test]
async fn removing_one_diamond_requirer_removes_only_it() {
    let fixture = Fixture::new(&diamond());
    fixture.remove("acme.other").await;
    assert_eq!(
        fixture.ids(),
        ["acme.root", "acme.left", "acme.right", "acme.base"]
    );
}

#[tokio::test]
async fn chain_is_removed_recursively_without_an_orphan() {
    let fixture = Fixture::new(&[
        plugin("acme.app", Req).requires(&["field:acme.x"]),
        plugin("acme.x", Dep)
            .provides(&["field:acme.x"])
            .requires(&["field:acme.y"]),
        plugin("acme.y", Dep)
            .provides(&["field:acme.y"])
            .requires(&["field:acme.z"]),
        plugin("acme.z", Dep).provides(&["field:acme.z"]),
        plugin("acme.keep", Req),
    ]);
    let plan = fixture
        .service
        .preview(&fixture.request(InstalledKind::Plugin, "acme.app"))
        .await
        .unwrap();
    let closure: Vec<_> = plan
        .removals
        .iter()
        .map(|r| (r.id.as_str(), r.reason))
        .collect();
    assert_eq!(
        closure,
        [
            ("acme.app", RemovalReason::Explicit),
            ("acme.x", RemovalReason::Unreferenced),
            ("acme.y", RemovalReason::Unreferenced),
            ("acme.z", RemovalReason::Unreferenced),
        ]
    );
    fixture.remove("acme.app").await;
    assert_eq!(fixture.ids(), ["acme.keep"]);
    assert_eq!(fixture.directories(), ["acme.keep"]);
}

#[tokio::test]
async fn requested_plugin_with_zero_references_is_never_autoremoved() {
    let fixture = Fixture::new(&[
        plugin("acme.app", Req).requires(&["field:acme.lib", "field:acme.dep"]),
        plugin("acme.lib", Req).provides(&["field:acme.lib"]),
        plugin("acme.dep", Dep).provides(&["field:acme.dep"]),
        plugin("acme.idle", Req).provides(&["field:acme.idle"]),
    ]);
    fixture.remove("acme.app").await;
    assert_eq!(
        fixture.installed(),
        [("acme.lib".to_owned(), Req), ("acme.idle".to_owned(), Req)]
    );
    // Only an explicit uninstall removes a requested plugin.
    fixture.remove("acme.lib").await;
    assert_eq!(fixture.ids(), ["acme.idle"]);
}

#[tokio::test]
async fn only_the_selected_provider_is_referenced_not_every_match() {
    let packages = || {
        vec![
            plugin("acme.app", Req).requires(&["field:shared.score ^1"]),
            plugin("acme.first", Dep).provides(&["field:shared.score"]),
            plugin("acme.second", Dep).provides(&["field:shared.score"]),
            plugin("acme.unrelated", Req),
        ]
    };
    // R0 selects the first installed provider; the second match is unreferenced.
    let fixture = Fixture::new(&packages());
    fixture.remove("acme.unrelated").await;
    assert_eq!(fixture.ids(), ["acme.app", "acme.first"]);
    // Explicitly removing the selected provider is allowed because another
    // kept provider takes over, which is then referenced and kept.
    let fixture = Fixture::new(&packages());
    fixture.remove("acme.first").await;
    assert_eq!(fixture.ids(), ["acme.app", "acme.second", "acme.unrelated"]);
}

#[tokio::test]
async fn unresolved_optional_is_no_reference_but_a_resolved_one_is() {
    let fixture = Fixture::new(&[
        plugin("acme.app", Req).requires(&["field:acme.dep ^1"]),
        plugin("acme.dep", Dep).provides(&["field:acme.dep"]),
        // Optional range no installed provider satisfies: absent, not a ref.
        plugin("acme.watcher", Req).optional(&["field:acme.dep ^2", "field:acme.none"]),
        plugin("acme.viewer", Req).optional(&["field:acme.extra"]),
        plugin("acme.extra", Dep).provides(&["field:acme.extra"]),
    ]);
    fixture.remove("acme.app").await;
    assert_eq!(fixture.ids(), ["acme.watcher", "acme.viewer", "acme.extra"]);
    // A resolved optional edge never blocks explicit removal; it is dropped.
    let plan = fixture
        .service
        .preview(&fixture.request(InstalledKind::Plugin, "acme.extra"))
        .await
        .unwrap();
    assert_eq!(
        plan.dropped_optional,
        [DroppedOptional {
            consumer: "acme.viewer".into(),
            provider: "acme.extra".into(),
            capability: "field:acme.extra".into(),
        }]
    );
    fixture.remove("acme.extra").await;
    assert_eq!(fixture.ids(), ["acme.watcher", "acme.viewer"]);
}

#[tokio::test]
async fn removing_a_required_provider_is_refused_naming_every_dependent() {
    let fixture = Fixture::new(&[
        plugin("acme.app", Req).requires(&["field:acme.core ^1"]),
        plugin("acme.core", Req).provides(&["field:acme.core"]),
        tool("score-panel").requires(&["field:acme.core.score >=1.0"]),
    ]);
    let before = (
        fixture.installed(),
        fixture.directories(),
        fixture.generation(),
    );
    let error = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.core"))
        .await
        .unwrap_err();
    assert_eq!(error.status_code(), 409);
    assert_eq!(
        error,
        UninstallError::Required {
            id: "acme.core".into(),
            dependents: vec![
                Dependent {
                    kind: InstalledKind::Plugin,
                    id: "acme.app".into(),
                    capability: "field:acme.core".into(),
                    range: "^1".into(),
                },
                Dependent {
                    kind: InstalledKind::Tool,
                    id: "score-panel".into(),
                    capability: "field:acme.core.score".into(),
                    range: ">=1.0".into(),
                },
            ],
        }
    );
    assert_eq!(
        error.to_string(),
        "acme.core is still required by acme.app (field:acme.core ^1), \
         score-panel (field:acme.core.score >=1.0)"
    );
    // Nothing stopped or changed, no job left behind, and the gate is free.
    assert!(fixture.recorder.events().is_empty());
    assert_eq!(
        (
            fixture.installed(),
            fixture.directories(),
            fixture.generation()
        ),
        before
    );
    drop(fixture.registry.begin_operation().unwrap());
    // Removing the consumers first makes the provider removable.
    fixture.remove("acme.app").await;
    let job = fixture
        .uninstall(fixture.request(InstalledKind::Tool, "score-panel"))
        .await
        .unwrap();
    assert_eq!(job.status, "completed");
    fixture.remove("acme.core").await;
    assert!(fixture.ids().is_empty());
}

#[tokio::test]
async fn autoremove_preserves_data_and_only_explicit_choice_removes_target_data() {
    let fixture = Fixture::new(&[
        plugin("acme.app", Req).requires(&["field:acme.dep"]),
        plugin("acme.dep", Dep).provides(&["field:acme.dep"]),
    ]);
    let mut request = fixture.request(InstalledKind::Plugin, "acme.app");
    request.remove_data = true;
    let job = fixture.uninstall(request).await.unwrap();
    assert_eq!(job.status, "completed", "{:?}", job.error);
    assert!(fixture.ids().is_empty());
    assert!(!fixture.data.join("acme.app").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.data.join("acme.dep/settings.json")).unwrap(),
        "acme.dep"
    );
    assert_eq!(
        fixture.recorder.events(),
        [
            "stop acme.app,acme.dep installed=true",
            "removed acme.app,acme.dep gone=true",
            "data Plugin acme.app",
        ]
    );
    // Default: an explicit uninstall preserves its own data too.
    let fixture = Fixture::new(&[plugin("acme.solo", Req)]);
    fixture.remove("acme.solo").await;
    assert!(fixture.data.join("acme.solo/settings.json").is_file());
}

async fn crashed(point: FaultPoint) -> Fixture {
    let fixture = Fixture::new(&diamond()[..4]);
    fixture.service.inject(Faults {
        crash: Some(point),
        fail: None,
    });
    let job = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.root"))
        .await
        .unwrap();
    assert_eq!(job.error.as_deref(), Some("simulated crash"));
    assert!(fixture.layout.state.join(JOURNAL_FILE).exists());
    fixture
}

#[tokio::test]
async fn crash_mid_removal_rolls_back_on_next_open() {
    let fixture = crashed(FaultPoint::BackupMoved(1)).await;
    // Two trees were moved aside before the process died.
    assert_eq!(
        fixture.directories(),
        [
            ".vosu-backup-acme.left-1",
            ".vosu-backup-acme.root-0",
            "acme.base",
            "acme.right"
        ]
    );
    let store = InstalledStore::open(fixture.layout.clone()).unwrap();
    assert_eq!(store.snapshot().records.len(), 4);
    assert_eq!(store.generation(), 7);
    assert!(!fixture.layout.state.join(JOURNAL_FILE).exists());
    assert_eq!(
        fixture.directories(),
        ["acme.base", "acme.left", "acme.right", "acme.root"]
    );
    // The recovered graph is fully usable again.
    let fresh = UninstallService::new(
        Arc::new(store),
        Arc::new(builtins()),
        fixture.recorder.clone(),
        tokio::runtime::Handle::current(),
    );
    let response = fresh
        .start(
            &fixture.registry,
            fixture.request(InstalledKind::Plugin, "acme.root"),
        )
        .await
        .unwrap();
    fresh.wait_idle().await;
    assert_eq!(
        fixture.registry.job(&response.job_id).unwrap().status,
        "completed"
    );
    assert!(fixture.directories().is_empty());
}

#[tokio::test]
async fn crash_after_records_commit_rolls_forward_on_next_open() {
    let fixture = crashed(FaultPoint::RecordsPublished).await;
    let store = InstalledStore::open(fixture.layout.clone()).unwrap();
    assert!(store.snapshot().records.is_empty());
    assert_eq!(store.generation(), 8);
    assert!(fixture.directories().is_empty());
    assert!(!fixture.layout.state.join(JOURNAL_FILE).exists());
    // Data/settings are outside the journal and survive both outcomes.
    for id in ["acme.root", "acme.left", "acme.right", "acme.base"] {
        assert!(
            fixture.data.join(id).join("settings.json").is_file(),
            "{id}"
        );
    }
}

#[tokio::test]
async fn failure_mid_removal_rolls_back_in_process() {
    let fixture = Fixture::new(&diamond()[..4]);
    fixture.service.inject(Faults {
        crash: None,
        fail: Some(FaultPoint::BackupMoved(2)),
    });
    let job = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.root"))
        .await
        .unwrap();
    assert_eq!(
        job.error.as_deref(),
        Some("filesystem operation failed: injected fault")
    );
    assert_eq!(fixture.ids().len(), 4);
    assert_eq!(
        fixture.directories(),
        ["acme.base", "acme.left", "acme.right", "acme.root"]
    );
    assert!(!fixture.layout.state.join(JOURNAL_FILE).exists());
    assert_eq!(
        fixture.recorder.events().len(),
        1,
        "no post-commit notification"
    );
}

#[tokio::test]
async fn stop_precedes_commit_and_its_failure_removes_nothing() {
    let fixture = Fixture::new(&diamond());
    fixture.recorder.fail_stop.store(true, Ordering::SeqCst);
    let job = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.root"))
        .await
        .unwrap();
    assert_eq!(
        job.error.as_deref(),
        Some("runtime activity could not be stopped: instance refused to stop")
    );
    assert_eq!(fixture.ids().len(), 5);
    assert_eq!(
        fixture.recorder.events(),
        ["stop acme.root,acme.left,acme.right installed=true"]
    );
}

#[tokio::test]
async fn built_ins_are_never_nodes_references_or_targets() {
    let fixture = Fixture::new(&[
        // Built-in first: the installed duplicate provider is not selected.
        plugin("acme.app", Req).requires(&["field:bundled.feed", "field:vosu.status *"]),
        plugin("acme.shadow", Dep).provides(&["field:bundled.feed"]),
        plugin("acme.unrelated", Req),
    ]);
    fixture.remove("acme.unrelated").await;
    assert_eq!(fixture.ids(), ["acme.app"]);
    for id in ["vosu.bundled", "vosu.status"] {
        let error = fixture
            .uninstall(fixture.request(InstalledKind::Plugin, id))
            .await
            .unwrap_err();
        assert_eq!(error, UninstallError::NotInstalled(id.into()));
        assert_eq!(error.status_code(), 404);
    }
}

#[tokio::test]
async fn exact_installed_source_is_required() {
    let fixture = Fixture::new(&[plugin("acme.solo", Req)]);
    let mut request = fixture.request(InstalledKind::Plugin, "acme.solo");
    request.source = source(USER);
    assert_eq!(
        fixture.uninstall(request).await.unwrap_err(),
        UninstallError::NotInstalled("acme.solo".into())
    );
    // Kind is part of the identity as well.
    assert_eq!(
        fixture
            .uninstall(fixture.request(InstalledKind::Tool, "acme.solo"))
            .await
            .unwrap_err(),
        UninstallError::NotInstalled("acme.solo".into())
    );
    assert_eq!(fixture.ids(), ["acme.solo"]);
}

#[tokio::test]
async fn uninstall_shares_the_catalog_operation_gate() {
    let fixture = Fixture::new(&[plugin("acme.solo", Req)]);
    let held = fixture.registry.begin_operation().unwrap();
    let error = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.solo"))
        .await
        .unwrap_err();
    assert_eq!(error.status_code(), 409);
    assert!(matches!(error, UninstallError::Plan(_)));
    drop(held);
    fixture.remove("acme.solo").await;
}

#[tokio::test]
async fn explicit_request_promotes_an_installed_dependency() {
    let fixture = Fixture::new(&[
        plugin("acme.app", Req).requires(&["field:acme.dep"]),
        plugin("acme.dep", Dep).provides(&["field:acme.dep"]),
    ]);
    let official = source(OFFICIAL);
    let promote = |id: &'static str| {
        fixture
            .service
            .promote(&fixture.registry, InstalledKind::Plugin, id, &official)
    };
    assert_eq!(promote("acme.dep").await, Ok(true));
    assert_eq!(fixture.generation(), 8);
    assert_eq!(promote("acme.dep").await, Ok(false));
    assert_eq!(fixture.generation(), 8, "no-op promotion publishes nothing");
    assert_eq!(
        promote("acme.missing").await,
        Err(UninstallError::NotInstalled("acme.missing".into()))
    );
    fixture.remove("acme.app").await;
    assert_eq!(fixture.installed(), [("acme.dep".to_owned(), Req)]);
    // The promotion released the gate.
    drop(fixture.registry.begin_operation().unwrap());
}

#[tokio::test]
async fn manifest_drift_refuses_rather_than_guessing_edges() {
    let fixture = Fixture::new(&[
        plugin("acme.app", Req).requires(&["field:acme.dep"]),
        plugin("acme.dep", Dep).provides(&["field:acme.dep"]),
    ]);
    let manifest = fixture.layout.plugins.join("acme.dep/manifest.toml");
    let edited = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("version = \"1.0.0\"", "version = \"1.0.1\"");
    std::fs::write(&manifest, edited).unwrap();
    let error = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.app"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        UninstallError::Install(InstallError::InstalledManifest("acme.dep".into()))
    );
    assert_eq!(fixture.ids(), ["acme.app", "acme.dep"]);
}

#[tokio::test]
async fn removal_journal_without_a_backup_is_refused_not_applied() {
    let fixture = Fixture::new(&[plugin("acme.solo", Req)]);
    let victim = fixture.layout.plugins.join("acme.solo");
    let journal = json!({"format":1,"previous_generation":7,"next_generation":8,
        "operations":[{"staging":null,"destination":victim,"backup":null}]});
    std::fs::write(fixture.layout.state.join(JOURNAL_FILE), journal.to_string()).unwrap();
    assert_eq!(
        crate::catalog::install::recover(&fixture.layout),
        Err(InstallError::RecoveryRequired)
    );
    let error = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.solo"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        UninstallError::Install(InstallError::RecoveryRequired)
    );
    assert!(victim.join("manifest.toml").is_file());
}

/// Native Windows only: an open file without FILE_SHARE_DELETE (like a
/// mapped library) inside a dependency makes its move fail; the whole
/// closure rolls back.
#[cfg(windows)]
#[tokio::test]
async fn windows_locked_dependency_rolls_back_the_whole_closure() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new(&diamond()[..4]);
    std::fs::write(fixture.layout.plugins.join("acme.base/lib.dll"), b"x").unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(fixture.layout.plugins.join("acme.base/lib.dll"))
        .unwrap();
    let job = fixture
        .uninstall(fixture.request(InstalledKind::Plugin, "acme.root"))
        .await
        .unwrap();
    drop(lock);
    assert_eq!(job.status, "failed");
    assert_eq!(fixture.ids().len(), 4);
    assert_eq!(
        fixture.directories(),
        ["acme.base", "acme.left", "acme.right", "acme.root"]
    );
    assert!(!fixture.layout.state.join(JOURNAL_FILE).exists());
}

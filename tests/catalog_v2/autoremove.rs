//! U0 composition through the public core API: the real P0 plans and I0
//! executor install each graph from verified packages; the core uninstall
//! facade removes it through the same installed store, operation gate and
//! journal. Expected closures are written out independently.
use super::install::{Handoff, Memory, OFFICIAL, Package, TARGET, package, prerequisites, source};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use vosu_core::catalog::{
    install::{
        HandoffFuture, InstallExecutor, InstallLayout, InstalledFor, InstalledKind, InstalledStore,
    },
    installed::{
        Dependent, RemovalPlan, RemovalRuntime, UninstallError, UninstallRequest, UninstallService,
    },
    plans::{self, CatalogJob, PlanRegistry},
    resolver::{BuiltinProviders, Candidate, Installed, Request, Snapshot},
    sources::{SourceGeneration, SourceRecord},
    trust::CurrentPolicy,
};
use vosu_plugin_api::{
    catalog::Catalog,
    manifest::{PackageKind, PackageManifest},
};

/// Records runtime callbacks; `remove_data` deletes only the named data root.
struct Lifecycle {
    data: PathBuf,
    events: Mutex<Vec<String>>,
}
fn ids(plan: &RemovalPlan) -> String {
    let ids: Vec<_> = plan.removals.iter().map(|r| r.id.as_str()).collect();
    ids.join(",")
}
impl RemovalRuntime for Lifecycle {
    fn stop<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, Result<(), String>> {
        self.events
            .lock()
            .unwrap()
            .push(format!("stop {}", ids(plan)));
        Box::pin(async { Ok(()) })
    }
    fn removed<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, ()> {
        self.events
            .lock()
            .unwrap()
            .push(format!("removed {}", ids(plan)));
        Box::pin(async {})
    }
    fn remove_data<'a>(
        &'a self,
        _: InstalledKind,
        id: &'a str,
    ) -> HandoffFuture<'a, Result<(), String>> {
        self.events.lock().unwrap().push(format!("data {id}"));
        let path = self.data.join(id);
        Box::pin(async move { std::fs::remove_dir_all(path).map_err(|e| e.kind().to_string()) })
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    layout: InstallLayout,
    data: PathBuf,
    official: SourceRecord,
    memory: Arc<Memory>,
    registry: PlanRegistry,
    executor: InstallExecutor,
    uninstall: UninstallService,
    lifecycle: Arc<Lifecycle>,
}
impl Fixture {
    fn new() -> Self {
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
        let official = source(OFFICIAL, true);
        let policy = official.clone();
        let current = move || Some((SourceGeneration(1), vec![policy.clone()]));
        let store = Arc::new(InstalledStore::open(layout.clone()).unwrap());
        let memory = Arc::new(Memory::default());
        let executor = InstallExecutor::new(
            store.clone(),
            memory.clone(),
            Arc::new(Handoff {
                state: layout.state.clone(),
                loads: Mutex::default(),
            }),
            Arc::new(current),
            tokio::runtime::Handle::current(),
        );
        let data = base.join("data");
        let lifecycle = Arc::new(Lifecycle {
            data: data.clone(),
            events: Mutex::default(),
        });
        let builtins = BuiltinProviders::from_registry(
            &vosu_core::state::schema::SchemaRegistry::with_core().unwrap(),
            &[],
        );
        let uninstall = UninstallService::new(
            store,
            Arc::new(builtins),
            lifecycle.clone(),
            tokio::runtime::Handle::current(),
        );
        Self {
            _root: root,
            layout,
            data,
            official,
            memory,
            registry: PlanRegistry::new(),
            executor,
            uninstall,
            lifecycle,
        }
    }
    /// Plan against the currently installed manifests, as P0 composition does.
    fn plan(&self, packages: &[&Package], request: &str) -> plans::PinnedPlan {
        let catalog = Catalog::parse(
            &json!({"catalog_version":2,"name":"Autoremove","published":"informational",
                "plugins":packages.iter().map(|p| p.entry.clone()).collect::<Vec<_>>()})
            .to_string(),
        )
        .unwrap();
        let candidates: Vec<_> = catalog
            .plugins
            .iter()
            .enumerate()
            .map(|(index, entry)| Candidate {
                kind: PackageKind::Plugin,
                entry,
                source: &self.official.identity,
                source_order: 0,
                catalog_order: index,
                trusted: true,
            })
            .collect();
        let manifests: Vec<_> = self
            .ids()
            .iter()
            .map(|id| {
                let text =
                    std::fs::read_to_string(self.layout.plugins.join(id).join("manifest.toml"))
                        .unwrap();
                PackageManifest::parse(&text).unwrap()
            })
            .collect();
        let installed: Vec<_> = manifests
            .iter()
            .map(|manifest| Installed { manifest })
            .collect();
        let builtins = BuiltinProviders::from_registry(
            &vosu_core::state::schema::SchemaRegistry::with_core().unwrap(),
            &[],
        );
        let plan = plans::build(
            &Snapshot {
                candidates: &candidates,
                installed: &installed,
                builtins: &builtins,
                application: vosu::application_version(),
                target: TARGET,
                max_package_bytes: 512 * 1024 * 1024,
            },
            &[Request {
                kind: PackageKind::Plugin,
                id: request,
                source: &self.official.identity,
            }],
            &CurrentPolicy {
                generation: SourceGeneration(1),
                sources: std::slice::from_ref(&self.official),
            },
            prerequisites(),
        );
        assert!(plan.view().errors.is_empty(), "{:?}", plan.view().errors);
        plan
    }
    async fn install(&self, packages: &[&Package], request: &str) -> Vec<String> {
        let plan = self.plan(packages, request);
        let adds = plan.view().adds.iter().map(|a| a.id.clone()).collect();
        let view = self.registry.insert(plan).unwrap();
        let response = self
            .registry
            .confirm(&view.plan_id, &[], prerequisites, &self.executor)
            .unwrap();
        self.executor.wait_idle().await;
        let job = self.registry.job(&response.job_id).unwrap();
        assert_eq!((job.status.as_str(), job.error), ("completed", None));
        for id in self.ids() {
            // Runtime-owned data/settings live outside every package root.
            std::fs::create_dir_all(self.data.join(&id)).unwrap();
            std::fs::write(self.data.join(&id).join("settings.json"), &id).unwrap();
        }
        adds
    }
    fn request(&self, id: &str, remove_data: bool) -> UninstallRequest {
        UninstallRequest {
            kind: InstalledKind::Plugin,
            id: id.into(),
            source: self.official.identity.clone(),
            remove_data,
        }
    }
    async fn uninstall(&self, id: &str, remove_data: bool) -> Result<CatalogJob, UninstallError> {
        let response = self
            .uninstall
            .start(&self.registry, self.request(id, remove_data))
            .await?;
        self.uninstall.wait_idle().await;
        Ok(self.registry.job(&response.job_id).unwrap())
    }
    /// Records reopened from disk, never an in-memory copy.
    fn installed(&self) -> Vec<(String, InstalledFor)> {
        InstalledStore::open(self.layout.clone())
            .unwrap()
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
        let mut names: Vec<_> = std::fs::read_dir(&self.layout.plugins)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
    fn events(&self) -> Vec<String> {
        self.lifecycle.events.lock().unwrap().clone()
    }
    fn settings(&self, id: &str) -> bool {
        self.data.join(id).join("settings.json").is_file()
    }
}

fn published(fixture: &Fixture, id: &str, requires: serde_json::Value) -> Package {
    let mut package = package(id, "1.0.0", "in-process", requires);
    package.publish(&fixture.memory, None);
    package
}

#[tokio::test]
async fn autoremove_diamond_keeps_shared_base_until_its_last_requirer_leaves() {
    let fixture = Fixture::new();
    let base = published(&fixture, "acme.base", json!([]));
    let left = published(
        &fixture,
        "acme.left",
        json!([{"id":"field:acme.base","version":"^1"}]),
    );
    let right = published(
        &fixture,
        "acme.right",
        json!([{"id":"field:acme.base","version":"^1"}]),
    );
    let app = published(
        &fixture,
        "acme.app",
        json!([{"id":"field:acme.left","version":"^1"},{"id":"field:acme.right","version":"^1"}]),
    );
    let other = published(
        &fixture,
        "acme.other",
        json!([{"id":"field:acme.base","version":"^1"}]),
    );
    let all = [&app, &left, &right, &base, &other];
    assert_eq!(
        fixture.install(&all, "acme.app").await,
        ["acme.base", "acme.left", "acme.right", "acme.app"]
    );
    // The second requester reuses the installed base: one add only.
    assert_eq!(fixture.install(&all, "acme.other").await, ["acme.other"]);
    let job = fixture.uninstall("acme.app", false).await.unwrap();
    assert_eq!((job.status.as_str(), job.error), ("completed", None));
    assert_eq!(fixture.ids(), ["acme.base", "acme.other"]);
    assert_eq!(fixture.directories(), ["acme.base", "acme.other"]);
    let job = fixture.uninstall("acme.other", false).await.unwrap();
    assert_eq!(job.status, "completed");
    assert!(fixture.ids().is_empty());
    assert!(fixture.directories().is_empty());
    assert_eq!(
        fixture.events(),
        [
            "stop acme.app,acme.left,acme.right",
            "removed acme.app,acme.left,acme.right",
            "stop acme.other,acme.base",
            "removed acme.other,acme.base",
        ]
    );
}

#[tokio::test]
async fn autoremove_refuses_a_needed_provider_and_keeps_a_promoted_one() {
    let fixture = Fixture::new();
    let base = published(&fixture, "acme.base", json!([]));
    let app = published(
        &fixture,
        "acme.app",
        json!([{"id":"field:acme.base","version":"^1"}]),
    );
    fixture.install(&[&app, &base], "acme.app").await;
    let error = fixture.uninstall("acme.base", false).await.unwrap_err();
    assert_eq!(error.status_code(), 409);
    assert_eq!(
        error,
        UninstallError::Required {
            id: "acme.base".into(),
            dependents: vec![Dependent {
                kind: InstalledKind::Plugin,
                id: "acme.app".into(),
                capability: "field:acme.base".into(),
                range: "^1".into(),
            }],
        }
    );
    assert!(fixture.events().is_empty(), "nothing stopped");
    assert_eq!(fixture.directories(), ["acme.app", "acme.base"]);
    // An explicit request for the installed dependency promotes it.
    assert_eq!(
        fixture
            .uninstall
            .promote(
                &fixture.registry,
                InstalledKind::Plugin,
                "acme.base",
                &fixture.official.identity
            )
            .await,
        Ok(true)
    );
    fixture.uninstall("acme.app", false).await.unwrap();
    assert_eq!(
        fixture.installed(),
        [("acme.base".to_owned(), InstalledFor::Requested)]
    );
}

#[tokio::test]
async fn autoremove_never_deletes_data_or_settings() {
    let fixture = Fixture::new();
    let base = published(&fixture, "acme.base", json!([]));
    let app = published(
        &fixture,
        "acme.app",
        json!([{"id":"field:acme.base","version":"^1"}]),
    );
    fixture.install(&[&app, &base], "acme.app").await;
    // Even when the user removes the target's data, the autoremoved
    // dependency keeps its data/settings.
    let job = fixture.uninstall("acme.app", true).await.unwrap();
    assert_eq!((job.status.as_str(), job.error), ("completed", None));
    assert!(fixture.ids().is_empty());
    assert!(!fixture.settings("acme.app"));
    assert!(fixture.settings("acme.base"));
    assert_eq!(
        fixture.events(),
        [
            "stop acme.app,acme.base",
            "removed acme.app,acme.base",
            "data acme.app",
        ]
    );
}

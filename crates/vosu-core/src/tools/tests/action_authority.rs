use super::*;
use crate::actions::{ActionDescriptor, ActionRegistry, JobError, JobRegistry};
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
};
use serde_json::{Value, json};
use vosu_proptest::bounded;

const ACTION: &str = "fixture.game.beatmap.import";
const OPTIONAL: &str = "fixture.game.utility.inspect";
const FIXTURE: &str = include_str!("../../../../../tests/fixtures/tools/action-web/manifest.toml");
#[derive(Default)]
struct Registry(Mutex<HashMap<String, Arc<ActionDescriptor>>>);
impl Registry {
    fn insert(&self, action: &str, version: &str) {
        let schema = Arc::new(
            vosu_plugin_api::manifest::action_schema::CompiledActionSchema::compile("true")
                .unwrap(),
        );
        self.0.lock().insert(
            action.into(),
            Arc::new(ActionDescriptor {
                id: action.into(),
                version: version.parse().unwrap(),
                provider: crate::identity::PluginInstanceId::new(77001),
                provider_id: "fixture.game".into(),
                title: action.into(),
                session: vosu_plugin_api::ActionSessionMode::Optional,
                cli: false,
                args_schema: Arc::clone(&schema),
                result_schema: schema,
            }),
        );
    }
}
impl ActionRegistry for Registry {
    fn get(&self, action: &str) -> Option<Arc<ActionDescriptor>> {
        self.0.lock().get(action).cloned()
    }
}
impl capabilities::InstalledCapabilities for Registry {
    fn version(&self, id: &str) -> Option<semver::Version> {
        self.get(id).map(|descriptor| descriptor.version.clone())
    }

    fn validation_snapshot(&self) -> crate::plugin::ValidationSnapshot {
        let providers = self
            .0
            .lock()
            .values()
            .map(|descriptor| crate::plugin::ProviderRegistration {
                id: descriptor.provider_id.clone(),
                official: false,
                builtin: false,
                replaces: Vec::new(),
                revision: None,
            })
            .collect();
        crate::plugin::ValidationSnapshot {
            providers,
            core_revisions: Vec::new(),
            packages: Vec::new(),
        }
    }
}
fn fixture() -> String {
    FIXTURE.replace(">=2.0.0-alpha.1, <3.0.0", &format!("={FIXTURE_VERSION}"))
}
fn request(method: Method, path: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap()
}
async fn value(response: axum::response::Response) -> Value {
    serde_json::from_slice(&bounded!(to_bytes(response.into_body(), 1024 * 1024)).unwrap()).unwrap()
}
async fn host(root: &Path, paths: Vec<PathBuf>) -> Arc<ToolRegistry> {
    bounded!(ToolRegistry::new(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        ExtraRoots {
            dev: paths,
            catalog: None
        },
        "http://127.0.0.1:25150".into(),
        Some("http://127.0.0.1:25151".into()),
        semver::Version::parse(FIXTURE_VERSION).unwrap(),
    ))
    .unwrap()
}
fn put(root: &Path, text: &str) -> PathBuf {
    let path = root.join("action-web");
    fs::create_dir_all(path.join("web")).unwrap();
    fs::write(path.join("manifest.toml"), text).unwrap();
    fs::write(path.join("web/index.html"), "v2 tool UI").unwrap();
    path
}
async fn launch(host: &Arc<ToolRegistry>, id: &str) -> String {
    let response = bounded!(host.dashboard(
        request(Method::POST, &format!("/api/tools/{id}/launch")),
        Bytes::new()
    ));
    assert_eq!(response.status(), StatusCode::OK);
    let body = bounded!(value(response));
    let code = body["url"]
        .as_str()
        .unwrap()
        .split("vosu-launch=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let response = host.exchange_session(json!({"tool":id,"code":code}).to_string().as_bytes());
    assert_eq!(response.status(), StatusCode::OK);
    bounded!(value(response))["session"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn v2_manifest_rejects_wrong_kind_legacy_fields_internal_and_duplicates() {
    let text = fixture();
    let version = semver::Version::parse(FIXTURE_VERSION).unwrap();
    assert!(manifest::ToolPackage::parse(&text, &version).is_ok());
    for invalid in [
        text.replace("kind = \"tool\"", "kind = \"overlay\""),
        text.replace("[ui]", "[service]\ncommand=['run']\n[ui]"),
        text.replace("[ui]", "[permissions]\nhost=['songs:write']\n[ui]"),
        text.replace("[ui]", "[api]\nwrite=['/']\n[ui]"),
        text.replace(
            "action:fixture.game.utility.inspect",
            "field:vosu.osu.internal.paths",
        ),
        text.replace(
            "action:fixture.game.utility.inspect",
            "action:fixture.game.beatmap.import",
        ),
        text.replace("^1.0.0", "not-semver"),
        text.replace("action:fixture.game.beatmap.import", "unknown:fixture.game"),
        text.replace("action:fixture.game.beatmap.import", "game:osu"),
        text.replace("root = \"web\"", "root = '../outside'"),
        text.replace(&format!("={FIXTURE_VERSION}"), "=0.0.0"),
        format!("{text}\n[[provides]]\nid='field:fixture.game'\nversion='1.0.0'"),
    ] {
        assert!(
            manifest::ToolPackage::parse(&invalid, &version).is_err(),
            "{invalid}"
        );
    }
}

#[tokio::test]
async fn v2_preference_never_falls_back_and_required_providers_block_launch() {
    let root = tempfile::tempdir().unwrap();
    let path = put(root.path(), &fixture());
    fs::write(
        path.join("tool.toml"),
        "manifest_version=1\nid='action-web'\nname='legacy'\nversion='1.0.0'\n[ui]\nroot='web'",
    )
    .unwrap();
    let host = bounded!(host(root.path(), vec![path.clone()]));
    assert_eq!(
        host.entry("action-web")
            .unwrap()
            .manifest
            .as_ref()
            .unwrap()
            .tool
            .shared
            .name,
        "Action web fixture"
    );
    assert_eq!(host.status(&host.entry("action-web").unwrap()).0, "blocked");
    assert_eq!(
        bounded!(host.dashboard(
            request(Method::POST, "/api/tools/action-web/launch"),
            Bytes::new()
        ))
        .status(),
        StatusCode::BAD_REQUEST
    );
    fs::write(path.join("manifest.toml"), "invalid v2").unwrap();
    bounded!(host.rescan());
    assert_eq!(host.status(&host.entry("action-web").unwrap()).0, "invalid");
    assert!(host.entry("action-web").unwrap().manifest.is_none());
    bounded!(host.shutdown());
}

#[tokio::test]
async fn live_versions_exact_ids_optional_grants_and_exact_launch_job_ownership() {
    let root = tempfile::tempdir().unwrap();
    let path = put(root.path(), &fixture());
    let host = bounded!(host(root.path(), vec![path]));
    let registry = Arc::new(Registry::default());
    registry.insert(ACTION, "1.4.2");
    host.set_action_providers(registry.clone(), registry.clone());
    let view = host.tool_view(&host.entry("action-web").unwrap());
    assert_eq!(view["requires"][0]["id"], format!("action:{ACTION}"));
    assert_eq!(view["requires"][0]["satisfied"], true);
    assert_eq!(view["optional"][0]["satisfied"], false);
    let first = bounded!(launch(&host, "action-web"));
    let second = bounded!(launch(&host, "action-web"));
    let a = host.authorize_action_session(&first, ACTION).unwrap();
    assert!(host.authenticate_action_session("forged-token").is_err());
    let b = host.authenticate_action_session(&second).unwrap();
    let crate::actions::Principal::Tool(owner) = &a else {
        panic!("launch must authenticate a tool principal");
    };
    assert_eq!(owner.tool_id(), Some("action-web"));
    let descriptor = registry.get(ACTION).unwrap();
    assert!(crate::actions::ToolAuthority::authorizes(
        host.as_ref(),
        owner,
        &descriptor
    ));
    let unrelated = crate::actions::JobOwner::for_tool(Arc::from("action-web"));
    assert!(!crate::actions::ToolAuthority::authorizes(
        host.as_ref(),
        &unrelated,
        &descriptor
    ));
    let jobs = JobRegistry::new();
    let job = jobs.create(a.clone(), ACTION.into(), None).unwrap();
    assert!(jobs.get(&a, &job.id).is_ok());
    assert_eq!(jobs.get(&b, &job.id).unwrap_err(), JobError::UnknownJob);
    assert_eq!(jobs.cancel(&b, &job.id).unwrap_err(), JobError::UnknownJob);
    assert!(host.authorize_action_session(&first, OPTIONAL).is_err());
    registry.insert(OPTIONAL, "2.1.0");
    assert_eq!(
        host.tool_view(&host.entry("action-web").unwrap())["optional"][0]["satisfied"],
        true
    );
    assert!(host.authorize_action_session(&first, OPTIONAL).is_ok());
    for action in [
        "fixture.game.beatmap",
        "fixture.game.beatmap.import.child",
        "action:fixture.game.beatmap.import",
    ] {
        registry.insert(action, "1.0.0");
        assert!(host.authorize_action_session(&first, action).is_err());
    }
    registry.insert(ACTION, "2.0.0");
    assert!(host.authorize_action_session(&first, ACTION).is_err());
    registry.insert(ACTION, "1.5.0");
    assert!(!crate::actions::ToolAuthority::authorizes(
        host.as_ref(),
        owner,
        &descriptor
    ));
    registry.0.lock().remove(ACTION);
    assert!(host.authorize_action_session(&first, ACTION).is_err());
    registry.insert(ACTION, "1.5.0");
    host.revoke_action_session(&first);
    assert!(host.authenticate_action_session(&first).is_err());
    assert!(host.authorize_action_session(&first, ACTION).is_err());
    assert!(!crate::actions::ToolAuthority::authorizes(
        host.as_ref(),
        owner,
        &registry.get(ACTION).unwrap()
    ));
    assert!(host.authorize_action_session(&second, ACTION).is_ok());
    let third = bounded!(launch(&host, "action-web"));
    let c = host.authorize_action_session(&third, ACTION).unwrap();
    assert_eq!(jobs.get(&c, &job.id).unwrap_err(), JobError::UnknownJob);
    bounded!(host.shutdown());
    assert!(host.authorize_action_session(&second, ACTION).is_err());
}

#[tokio::test]
async fn legacy_permissions_never_grant_and_disabled_replaced_sessions_fail() {
    let root = tempfile::tempdir().unwrap();
    let path = put(root.path(), &fixture());
    let legacy = root.path().join("legacy");
    fs::create_dir_all(legacy.join("web")).unwrap();
    fs::write(legacy.join("tool.toml"), "manifest_version=1\nid='legacy'\nname='Legacy'\nversion='1.0.0'\n[ui]\nroot='web'\n[service]\ncommand=['run']\n[api]\nwrite=['/']\n[permissions]\nhost=['songs:write']").unwrap();
    let host = bounded!(host(root.path(), vec![path.clone(), legacy]));
    let registry = Arc::new(Registry::default());
    registry.insert(ACTION, "1.0.0");
    host.set_action_providers(registry.clone(), registry);
    // Counterexample: a legacy package's 1.x permissions never grant an
    // action; it cannot even mint a launch code.
    let legacy_launch = bounded!(host.dashboard(
        request(Method::POST, "/api/tools/legacy/launch"),
        Bytes::new()
    ));
    assert_eq!(legacy_launch.status(), StatusCode::BAD_REQUEST);
    assert!(host.codes.lock().is_empty());
    assert!(host.authenticate_action_session("legacy").is_err());
    assert!(host.authorize_action_session("legacy", ACTION).is_err());
    let token = bounded!(launch(&host, "action-web"));
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    bounded!(host.dashboard(
        request(Method::POST, "/api/tools/action-web/disable"),
        Bytes::new()
    ));
    assert!(host.authorize_action_session(&token, ACTION).is_err());
    bounded!(host.dashboard(
        request(Method::POST, "/api/tools/action-web/enable"),
        Bytes::new()
    ));
    assert!(host.authorize_action_session(&token, ACTION).is_err());
    let token = bounded!(launch(&host, "action-web"));
    fs::write(
        path.join("manifest.toml"),
        fixture().replace("Action web fixture", "Replaced tool"),
    )
    .unwrap();
    bounded!(host.rescan());
    assert!(host.authorize_action_session(&token, ACTION).is_err());
    bounded!(host.shutdown());
}

#[tokio::test]
async fn user_trust_and_required_non_action_metadata_are_rechecked() {
    let root = tempfile::tempdir().unwrap();
    let path = put(&root.path().join("data"), &fixture());
    let host = bounded!(host(root.path(), vec![]));
    let registry = Arc::new(Registry::default());
    registry.insert(ACTION, "1.0.0");
    host.set_action_providers(registry.clone(), registry.clone());
    assert_eq!(
        host.status(&host.entry("action-web").unwrap()).0,
        "untrusted"
    );
    let denied = bounded!(host.dashboard(
        request(Method::POST, "/api/tools/action-web/launch"),
        Bytes::new()
    ));
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    bounded!(host.dashboard(
        request(Method::POST, "/api/tools/action-web/trust"),
        Bytes::new()
    ));
    let token = bounded!(launch(&host, "action-web"));
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    host.saved.lock().get_mut("action-web").unwrap().trusted = false;
    assert!(host.authorize_action_session(&token, ACTION).is_err());
    host.saved.lock().get_mut("action-web").unwrap().trusted = true;
    fs::write(
        path.join("manifest.toml"),
        format!(
            "{}\n[[requires]]\nid='field:fixture.game'\nversion='^1.0.0'\n",
            fixture()
        ),
    )
    .unwrap();
    bounded!(host.rescan());
    assert_eq!(host.status(&host.entry("action-web").unwrap()).0, "blocked");
    registry.insert("field:fixture.game", "1.0.0");
    let token = bounded!(launch(&host, "action-web"));
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    registry.0.lock().remove("field:fixture.game");
    assert!(host.authorize_action_session(&token, ACTION).is_err());
    bounded!(host.shutdown());
}

#[tokio::test]
async fn launch_sessions_are_bounded_and_revocation_releases_capacity() {
    let root = tempfile::tempdir().unwrap();
    let path = put(root.path(), &fixture());
    let host = bounded!(host(root.path(), vec![path]));
    let registry = Arc::new(Registry::default());
    registry.insert(ACTION, "1.0.0");
    host.set_action_providers(registry.clone(), registry);
    let entry = host.entry("action-web").unwrap();
    for index in 0..1024 {
        host.sessions.lock().insert(
            index.to_string(),
            action_authority::LaunchSession::new(&entry),
        );
    }
    host.codes.lock().insert(
        "capacity-code".into(),
        Code {
            tool: "action-web".into(),
            expires: Instant::now() + Duration::from_secs(60),
        },
    );
    let response = host.exchange_session(b"{\"tool\":\"action-web\",\"code\":\"capacity-code\"}");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(host.sessions.lock().len(), 1024);
    host.revoke_action_session("0");
    let token = bounded!(launch(&host, "action-web"));
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    assert_eq!(host.sessions.lock().len(), 1024);
    bounded!(host.shutdown());
}

#[test]
fn game_requirements_use_literal_star_and_presence_not_semver() {
    let registry = Registry::default();
    registry.insert("game:osu", "1.0.0-alpha.1");
    let requirement =
        capabilities::Requirement::from_capability(&vosu_plugin_api::manifest::Capability {
            id: "game:osu".into(),
            version: "*".into(),
            schema: None,
        })
        .unwrap();
    assert!(requirement.resolved(&registry, &registry));
    for version in ["*.*", ">=0", "^1"] {
        assert!(
            capabilities::Requirement::from_capability(&vosu_plugin_api::manifest::Capability {
                id: "game:osu".into(),
                version: version.into(),
                schema: None,
            },)
            .is_err()
        );
    }
}

#[tokio::test]
async fn validation_context_cache_hits_on_unchanged_topology_and_invalidates_immediately() {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    struct VersionedRegistry {
        inner: Registry,
        generation: AtomicU64,
        snapshot_calls: AtomicUsize,
        conflict: AtomicBool,
    }
    impl ActionRegistry for VersionedRegistry {
        fn get(&self, action: &str) -> Option<Arc<ActionDescriptor>> {
            self.inner.get(action)
        }
    }
    impl capabilities::InstalledCapabilities for VersionedRegistry {
        fn version(&self, id: &str) -> Option<semver::Version> {
            self.inner.version(id)
        }
        fn validation_generation(&self) -> Option<u64> {
            Some(self.generation.load(Ordering::Acquire))
        }
        fn validation_snapshot(&self) -> crate::plugin::ValidationSnapshot {
            self.snapshot_calls.fetch_add(1, Ordering::AcqRel);
            let mut snapshot = self.inner.validation_snapshot();
            if self.conflict.load(Ordering::Acquire) {
                snapshot
                    .providers
                    .push(crate::plugin::ProviderRegistration {
                        id: "action-web".into(),
                        official: false,
                        builtin: false,
                        replaces: Vec::new(),
                        revision: None,
                    });
            }
            snapshot
        }
    }

    let root = tempfile::tempdir().unwrap();
    let path = put(root.path(), &fixture());
    let host = bounded!(host(root.path(), vec![path]));
    let registry = Arc::new(VersionedRegistry {
        inner: Registry::default(),
        generation: AtomicU64::new(1),
        snapshot_calls: AtomicUsize::new(0),
        conflict: AtomicBool::new(false),
    });
    registry.inner.insert(ACTION, "1.0.0");
    host.set_action_providers(registry.clone(), registry.clone());
    let token = bounded!(launch(&host, "action-web"));
    let calls_before = registry.snapshot_calls.load(Ordering::Acquire);
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    assert_eq!(
        registry.snapshot_calls.load(Ordering::Acquire),
        calls_before,
        "repeated authorizations on unchanged topology must hit the context cache"
    );

    // Provider topology mutation immediately invalidates and re-runs validation.
    registry.conflict.store(true, Ordering::Release);
    registry.generation.fetch_add(1, Ordering::AcqRel);
    assert!(
        host.authorize_action_session(&token, ACTION).is_err(),
        "bumped provider generation must immediately invalidate cached validation"
    );
    assert_eq!(
        registry.snapshot_calls.load(Ordering::Acquire),
        calls_before + 1
    );

    // Clearing the conflict and bumping generation restores authorization.
    registry.conflict.store(false, Ordering::Release);
    registry.generation.fetch_add(1, Ordering::AcqRel);
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    let calls_after_restore = registry.snapshot_calls.load(Ordering::Acquire);
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    assert_eq!(
        registry.snapshot_calls.load(Ordering::Acquire),
        calls_after_restore
    );

    // Rescan bumps tool topology generation and forces revalidation once.
    bounded!(host.rescan());
    assert!(host.authorize_action_session(&token, ACTION).is_ok());
    assert_eq!(
        registry.snapshot_calls.load(Ordering::Acquire),
        calls_after_restore + 1
    );
    bounded!(host.shutdown());
}

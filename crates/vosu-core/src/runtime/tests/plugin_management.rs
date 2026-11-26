//! `/api/plugins` behavior over a real `PluginHost` whose launches pass
//! through the approval gate (catalog-v2.md A5.5 "Management routes",
//! "States"). Routing's dashboard-only and CSRF rules are
//! `server::routes` tests; native children are `plugin_host_native`'s
//! `approval` module.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use crate::plugin::approval::{ApprovalGate, ApprovalStore, ApprovedLaunch};
use crate::plugin::test_support::{
    LaunchBehavior, RecordingLauncher, write_development_package,
};
use crate::plugin::{
    self, HostError, PluginHost, PluginInstanceId, ProcessEndpoint, ProcessLaunch, ProcessLauncher,
};

use super::*;

#[test]
fn ids_are_one_percent_decoded_segment() {
    assert_eq!(decode_segment("test.u0").as_deref(), Some("test.u0"));
    assert_eq!(decode_segment("test%2Eu0").as_deref(), Some("test.u0"));
    assert_eq!(decode_segment("a%2Fb"), None, "an id never spans segments");
    assert_eq!(decode_segment(""), None);
    assert_eq!(decode_segment("bad%2"), None);
    assert_eq!(decode_segment("bad%zz"), None);
    assert_eq!(decode_segment("%ff"), None, "ids are UTF-8");
}

struct Approved {
    inner: Arc<RecordingLauncher>,
    digests: Mutex<Vec<[u8; 32]>>,
}

/// Records each approved digest, then launches nothing real.
struct ApprovedLauncher(Arc<Approved>);

impl ApprovedLaunch for ApprovedLauncher {
    fn launch_approved(
        &self,
        launch: ProcessLaunch<'_>,
        library_sha256: [u8; 32],
    ) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        self.0
            .digests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(library_sha256);
        self.0.inner.launch(launch)
    }
}

/// Owns what the daemon's shutdown would stop. The config store's reload
/// worker is a blocking task that runs until `ConfigStore::shutdown` (or
/// its last sender drops), and the runtime's drop waits for every blocking
/// task, so [`Fixture::shutdown`] must run before the test returns.
struct Fixture {
    _root: tempfile::TempDir,
    package: tempfile::TempDir,
    store: Arc<ApprovalStore>,
    approved: Arc<Approved>,
    host: Arc<PluginHost>,
    config: Arc<crate::config::ConfigStore>,
    logger: crate::logging::Logger,
    management: Arc<PluginManagement>,
}

const PROCESS_ID: &str = "test.u0.managed";
const LOCAL_ID: &str = "test.u0.local";

async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let logger = crate::logging::Logger::new(
        root.path(),
        crate::logging::LoggerOptions::default(),
        Arc::new(crate::logging::StdoutConsole),
    )
    .unwrap();
    let config = root.path().join("config");
    let settings = Arc::new(
        crate::config::ConfigStore::initialize(root.path().join("vosu.toml"))
            .await
            .unwrap(),
    );
    let host = PluginHost::new(
        logger.clone(),
        Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
            plugin::settings::ConfigSettingsBridgeB2::new(Arc::clone(&settings)),
        ))),
        tokio::runtime::Handle::current(),
    );
    let store = Arc::new(ApprovalStore::open(&config));
    let approved = Arc::new(Approved {
        inner: RecordingLauncher::new(LaunchBehavior::Ready),
        digests: Mutex::new(Vec::new()),
    });
    host.install_process_launcher(Arc::new(ApprovalGate::new(
        Arc::clone(&store),
        ApprovedLauncher(Arc::clone(&approved)),
    )))
    .unwrap();

    let mut roster = Roster::default();
    let local = plugin::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &plugin::ValidationSnapshot::default(),
        &[plugin::BundledPlugin {
            manifest: "kind='plugin'\nmanifest_version=1\nid='test.u0.local'\nname='Local'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n",
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let instance = PluginInstanceId::new(0);
    let created = host.create_instance(instance, &local[0], root.path().into(), None);
    roster.loaded(
        instance,
        Trust::Bundled,
        None,
        &local[0],
        created.map_err(|error| error.to_string()),
    );

    let package = tempfile::tempdir().unwrap();
    write_development_package(
        package.path(),
        PROCESS_ID,
        "u0-target",
        "process",
        "plugin.bin",
        b"process library bytes",
    )
    .unwrap();
    let loaded = plugin::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &plugin::ValidationSnapshot::default(),
        package.path(),
        false,
    )
    .unwrap();
    let instance = PluginInstanceId::new(1);
    let created = host.create_instance(instance, &loaded, root.path().into(), None);
    assert!(created.is_err(), "an unapproved package must not start");
    roster.loaded(
        instance,
        Trust::User,
        Some(package.path().to_owned()),
        &loaded,
        created.map_err(|error| error.to_string()),
    );
    roster.refused(
        PluginInstanceId::new(2),
        Trust::User,
        Some(root.path().join("broken")),
        "manifest.toml: not found".into(),
    );
    let management = Arc::new(PluginManagement::new(
        Arc::clone(&host),
        Arc::clone(&store),
        roster,
    ));
    Fixture {
        _root: root,
        package,
        store,
        approved,
        host,
        config: settings,
        logger,
        management,
    }
}

impl Fixture {
    /// The daemon's shutdown order: plugins, then config, then logging.
    async fn shutdown(self) {
        let host = Arc::clone(&self.host);
        tokio::task::spawn_blocking(move || host.destroy_all(Duration::from_secs(2)))
            .await
            .unwrap();
        self.config.shutdown().await.unwrap();
        self.logger.shutdown().await.unwrap();
    }

    async fn call(&self, method: Method, path: &str, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let body = if body.is_null() {
            Bytes::new()
        } else {
            Bytes::from(body.to_string())
        };
        let response = Arc::clone(&self.management).handle(request, body).await;
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store",
            "{path}"
        );
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn plugin(&self, id: &str) -> Value {
        let (status, list) = self.call(Method::GET, "/api/plugins", Value::Null).await;
        assert_eq!(status, StatusCode::OK);
        list["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|plugin| plugin["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is not listed: {list}"))
    }

    fn launches(&self) -> usize {
        self.approved.inner.launches().len()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approval_lifecycle_through_the_management_routes() {
    vosu_proptest::bounded!(async {
        let fixture = fixture().await;
        let path = |action: &str| format!("/api/plugins/{PROCESS_ID}/{action}");

        // Listed: every plugin, the unapproved one with its pending digests.
        let (_, list) = fixture.call(Method::GET, "/api/plugins", Value::Null).await;
        assert_eq!(list["plugins"].as_array().unwrap().len(), 3);
        let broken = &list["plugins"][2];
        assert_eq!(broken["id"], Value::Null);
        assert_eq!(broken["state"], "failed");
        assert_eq!(broken["reason"], "load");
        let local = fixture.plugin(LOCAL_ID).await;
        assert_eq!(local["state"], "running");
        assert_eq!(local["trust"], "bundled");
        let pending = fixture.plugin(PROCESS_ID).await;
        assert_eq!(pending["state"], "approval_required");
        assert_eq!(pending["trust"], "user");
        assert_eq!(pending["runtime"], "process");
        assert_eq!(pending["reason"], "approval");
        assert_eq!(pending["generation"], 1);
        assert_eq!(
            pending["provenance"],
            json!({"kind": "development", "path": fixture.package.path().canonicalize().unwrap()})
        );
        assert_eq!(pending["requires_internal_paths"], false);
        let approval = pending["approval"].clone();
        assert_eq!(approval["library_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(fixture.launches(), 0);

        // Not applicable, malformed and stale approvals change nothing.
        let (status, body) = fixture
            .call(
                Method::POST,
                &format!("/api/plugins/{LOCAL_ID}/approval"),
                approval.clone(),
            )
            .await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::BAD_REQUEST, &json!("approval_not_applicable"))
        );
        let (status, body) = fixture
            .call(
                Method::POST,
                &path("approval"),
                json!({"manifest_sha256": "x"}),
            )
            .await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::BAD_REQUEST, &json!("invalid_request"))
        );
        let mut stale = approval.clone();
        stale["library_sha256"] = json!("0".repeat(64));
        let (status, body) = fixture.call(Method::POST, &path("approval"), stale).await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::CONFLICT, &json!("approval_stale"))
        );
        assert_eq!(fixture.launches(), 0);
        assert!(
            !fixture.store.path().exists(),
            "a refused approval wrote the store"
        );

        // A restart or stop of the unapproved plugin never executes it.
        let (status, _) = fixture
            .call(Method::POST, &path("restart"), json!({"generation": 1}))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fixture.launches(), 0);
        let pending = fixture.plugin(PROCESS_ID).await;
        assert_eq!(pending["state"], "approval_required");
        let generation = pending["generation"].clone();

        // Approval persists the record and starts it with that digest.
        let (status, body) = fixture
            .call(Method::POST, &path("approval"), approval.clone())
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "running");
        assert_eq!(body["approved"], true);
        assert_eq!(fixture.launches(), 1);
        let approved_digest: String = fixture.approved.digests.lock().unwrap()[0]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(json!(approved_digest), approval["library_sha256"]);
        assert!(fixture.store.path().exists());

        // Restart and stop act on exactly the generation shown.
        let (status, body) = fixture
            .call(
                Method::POST,
                &path("restart"),
                json!({"generation": generation}),
            )
            .await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::CONFLICT, &json!("generation_changed"))
        );
        let current = fixture.plugin(PROCESS_ID).await["generation"].clone();
        let (status, body) = fixture
            .call(
                Method::POST,
                &path("restart"),
                json!({"generation": current}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "running");
        assert_eq!(body["generation"], current.as_u64().unwrap() + 1);
        assert_eq!(fixture.launches(), 2);
        let (status, body) = fixture
            .call(
                Method::POST,
                &path("stop"),
                json!({"generation": body["generation"]}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "stopped");

        // In-process plugins restart only with the daemon.
        for action in ["restart", "stop"] {
            let (status, body) = fixture
                .call(
                    Method::POST,
                    &format!("/api/plugins/{LOCAL_ID}/{action}"),
                    json!({"generation": 1}),
                )
                .await;
            assert_eq!(
                (status, &body["code"]),
                (StatusCode::CONFLICT, &json!("requires_daemon_restart"))
            );
        }
        let (status, body) = fixture
            .call(
                Method::POST,
                "/api/plugins/test.u0.unknown/stop",
                json!({"generation": 1}),
            )
            .await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::NOT_FOUND, &json!("unknown_plugin"))
        );

        // Revocation removes the record and the next start is refused.
        let current = fixture.plugin(PROCESS_ID).await["generation"].clone();
        let (status, _) = fixture
            .call(
                Method::POST,
                &path("restart"),
                json!({"generation": current}),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(fixture.launches(), 3);
        let (status, body) = fixture
            .call(Method::DELETE, &path("approval"), Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "approval_required");
        let current = fixture.plugin(PROCESS_ID).await["generation"].clone();
        fixture
            .call(
                Method::POST,
                &path("restart"),
                json!({"generation": current}),
            )
            .await;
        assert_eq!(fixture.launches(), 3, "a revoked package executed");

        // Changed bytes need a new approval: the old digests are stale.
        std::fs::write(
            fixture.package.path().join("lib/plugin.bin"),
            b"changed library bytes",
        )
        .unwrap();
        let (status, body) = fixture
            .call(Method::POST, &path("approval"), approval.clone())
            .await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::CONFLICT, &json!("approval_stale"))
        );
        let changed = fixture.plugin(PROCESS_ID).await["approval"].clone();
        assert_ne!(changed["library_sha256"], approval["library_sha256"]);
        assert_eq!(changed["manifest_sha256"], approval["manifest_sha256"]);
        let (status, body) = fixture.call(Method::POST, &path("approval"), changed).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["state"], "running");
        assert_eq!(fixture.launches(), 4);
        fixture.shutdown().await;
    });
}

/// D1 counterexample: an approved id never authorizes a changed library.
/// The approval binds both digests; a running, approved plugin whose
/// library changes is not started again by a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_approved_id_never_authorizes_a_changed_library() {
    vosu_proptest::bounded!(async {
        let fixture = fixture().await;
        let path = |action: &str| format!("/api/plugins/{PROCESS_ID}/{action}");
        let approval = fixture.plugin(PROCESS_ID).await["approval"].clone();
        let (status, body) = fixture
            .call(Method::POST, &path("approval"), approval.clone())
            .await;
        assert_eq!(
            (status, &body["state"]),
            (StatusCode::OK, &json!("running"))
        );
        assert_eq!(fixture.launches(), 1);

        std::fs::write(
            fixture.package.path().join("lib/plugin.bin"),
            b"replaced after approval",
        )
        .unwrap();
        let generation = fixture.plugin(PROCESS_ID).await["generation"].clone();
        fixture
            .call(
                Method::POST,
                &path("restart"),
                json!({"generation": generation}),
            )
            .await;
        assert_eq!(fixture.launches(), 1, "the changed library executed");
        let listed = fixture.plugin(PROCESS_ID).await;
        assert_eq!(listed["state"], "approval_required", "{listed}");
        assert_eq!(listed["approved"], false);
        assert_ne!(
            listed["approval"]["library_sha256"],
            approval["library_sha256"]
        );
        // The earlier approval body is now stale, never silently reused.
        let (status, body) = fixture
            .call(Method::POST, &path("approval"), approval)
            .await;
        assert_eq!(
            (status, &body["code"]),
            (StatusCode::CONFLICT, &json!("approval_stale"))
        );
        assert_eq!(fixture.launches(), 1);
        fixture.shutdown().await;
    });
}

const SETTINGS_ID: &str = "test.d1.settings";
const SETTINGS_SECRET: &str = "sentinel-d1-plugin-password-93f2";

#[derive(Default)]
struct RecordedSettings(Mutex<Vec<(String, vosu_plugin_api::Message)>>);

impl crate::plugin::settings::SettingsChangeDelivery for RecordedSettings {
    fn settings_changed(&self, plugin_id: &str, message: vosu_plugin_api::Message) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((plugin_id.to_owned(), message));
    }
}

impl RecordedSettings {
    fn taken(&self) -> Vec<(String, vosu_plugin_api::Message)> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

async fn send(
    management: &Arc<PluginManagement>,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let body = if body.is_null() {
        Bytes::new()
    } else {
        Bytes::from(body.to_string())
    };
    let response = Arc::clone(management).handle(request, body).await;
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// M7-P05: `/api/plugins/<id>/settings` never echoes a password, even after
/// a failed save; an invalid POST changes nothing and sends no
/// `SETTINGS_CHANGED`; a valid POST sends one complete object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_settings_are_write_only_validated_and_delivered_once() {
    vosu_proptest::bounded!(async {
        let root = tempfile::tempdir().unwrap();
        let logger = crate::logging::Logger::new(
            root.path(),
            crate::logging::LoggerOptions::default(),
            Arc::new(crate::logging::StdoutConsole),
        )
        .unwrap();
        let store = Arc::new(plugin::settings::PluginSettingsStore::open(root.path()));
        let host = PluginHost::new(
            logger.clone(),
            Arc::new(plugin::settings::PluginSettingsService::new(store.clone())),
            tokio::runtime::Handle::current(),
        );
        let loaded = plugin::load_bundled(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &plugin::ValidationSnapshot::default(),
            &[plugin::BundledPlugin {
                manifest: "kind='plugin'\nmanifest_version=1\nid='test.d1.settings'\nname='Settings'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nsettings_schema='settings.schema.json'\n",
                settings_schema: Some(
                    r#"[{"uniqueID":"LABEL","type":"text","title":"Label","value":"hello"},
                        {"uniqueID":"RATE","type":"number","title":"Rate","value":150},
                        {"uniqueID":"TOKEN","type":"password","title":"Token","value":""}]"#,
                ),
                namespace_schemas: &[],
                action_schemas: &[],
                vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
            }],
        )
        .unwrap();
        let mut roster = Roster::default();
        let instance = PluginInstanceId::new(0);
        let created = host.create_instance(instance, &loaded[0], root.path().into(), None);
        roster.loaded(
            instance,
            Trust::Bundled,
            None,
            &loaded[0],
            created.map_err(|error| error.to_string()),
        );
        let recorded = Arc::new(RecordedSettings::default());
        let management = Arc::new(
            PluginManagement::new(
                Arc::clone(&host),
                Arc::new(ApprovalStore::open(&root.path().join("config"))),
                roster,
            )
            .with_settings_delivery(recorded.clone()),
        );
        let path = format!("/api/plugins/{SETTINGS_ID}/settings");

        let (status, body) = send(&management, Method::GET, &path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let view: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            view["values"],
            json!({"LABEL":"hello","RATE":150,"TOKEN":""})
        );
        assert_eq!(view["secrets"], json!({"TOKEN": false}));
        assert_eq!(view["schema"].as_array().unwrap().len(), 3);

        // Valid: stored, one complete object delivered, the secret withheld.
        let (status, body) = send(
            &management,
            Method::POST,
            &path,
            json!([{"uniqueID":"LABEL","value":"set"},{"uniqueID":"TOKEN","value":SETTINGS_SECRET}]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(!body.contains(SETTINGS_SECRET), "{body}");
        let delivered = recorded.taken();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0, SETTINGS_ID);
        let vosu_plugin_api::Message::SettingsChanged(changed) = &delivered[0].1 else {
            panic!("expected SETTINGS_CHANGED");
        };
        let vosu_plugin_api::CborValue::Map(entries) = &changed.values else {
            panic!("expected a CBOR map");
        };
        assert_eq!(entries.len(), 3, "complete effective object: {entries:?}");
        let stored = std::fs::read(store.path()).unwrap();

        // Invalid: nothing stored, nothing delivered, the secret not echoed.
        for bad in [
            json!([{"uniqueID":"TOKEN","value":SETTINGS_SECRET},{"uniqueID":"RATE","value":"fast"}]),
            json!([{"uniqueID":"TOKEN","value":SETTINGS_SECRET},{"uniqueID":"NOPE","value":1}]),
            json!([{"uniqueID":"TOKEN","value":SETTINGS_SECRET,"extra":true}]),
            json!({"TOKEN": SETTINGS_SECRET}),
        ] {
            let (status, body) = send(&management, Method::POST, &path, bad.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
            assert!(!body.contains(SETTINGS_SECRET), "{body}");
        }
        assert!(recorded.taken().is_empty(), "an invalid POST delivered");
        assert_eq!(std::fs::read(store.path()).unwrap(), stored);
        let (_, body) = send(&management, Method::GET, &path, Value::Null).await;
        assert!(!body.contains(SETTINGS_SECRET), "{body}");
        let view: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(view["values"], json!({"LABEL":"set","RATE":150,"TOKEN":""}));
        assert_eq!(view["secrets"], json!({"TOKEN": true}));

        // An unknown id has no settings route; bridged ids are read-only here.
        let (status, _) = send(
            &management,
            Method::GET,
            "/api/plugins/test.d1.unknown/settings",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let destroy = Arc::clone(&host);
        tokio::task::spawn_blocking(move || destroy.destroy_all(Duration::from_secs(2)))
            .await
            .unwrap();
        logger.shutdown().await.unwrap();
    });
}

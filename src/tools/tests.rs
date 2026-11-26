use super::*;
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::{Method, StatusCode, header},
};
use serde_json::{Value, json};
use vosu_proptest::bounded;
use vosu_surface::files::{FileError, RelativeChild};

fn manifest(id: &str, extra: &str) -> String {
    format!(
        "manifest_version=1\nkind='tool'\nid='{id}'\nname='Test {id}'\nversion='1.0.0'\nhost='={}'\nicon='art/icon.svg'\n[ui]\nroot='web'\n{extra}",
        env!("CARGO_PKG_VERSION")
    )
}
fn put(root: &Path, id: &str, extra: &str) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(dir.join("web")).unwrap();
    fs::create_dir_all(dir.join("art")).unwrap();
    fs::write(dir.join("manifest.toml"), manifest(id, extra)).unwrap();
    fs::write(dir.join("web/index.html"), "tool-ui").unwrap();
    fs::write(dir.join("art/icon.svg"), "<svg/>").unwrap();
    dir
}
/// A 1.x package whose service would leave a marker if anything ran it.
fn put_legacy(root: &Path, id: &str, text: &str) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(dir.join("web")).unwrap();
    fs::write(dir.join("tool.toml"), text).unwrap();
    fs::write(dir.join("web/index.html"), "legacy-ui").unwrap();
    dir
}
fn legacy_toml(id: &str, marker: &Path) -> String {
    format!(
        "manifest_version=1\nid='{id}'\nname='Legacy'\nversion='1.0.0'\n[ui]\nroot='web'\n[service]\ncommand=['sh','-c','touch {}']\nstartup='always'\n[cli]\ncommand=['sh','-c','touch {}']\n[permissions]\nhost=['context']\n",
        marker.display(),
        marker.display()
    )
}
async fn host(root: &Path, dev: Vec<PathBuf>) -> Arc<ToolRegistry> {
    let host = bounded!(ToolRegistry::new(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        ExtraRoots { dev },
        "http://127.0.0.1:25150".into(),
        Some("http://127.0.0.1:25151".into()),
    ))
    .unwrap();
    host.activate(None);
    host
}
async fn value(response: axum::response::Response) -> Value {
    serde_json::from_slice(&bounded!(to_bytes(response.into_body(), 1024 * 1024)).unwrap()).unwrap()
}
fn request(method: Method, path: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap()
}
async fn post(
    host: &Arc<ToolRegistry>,
    path: &str,
    body: &'static [u8],
) -> axum::response::Response {
    bounded!(host.dashboard(request(Method::POST, path), Bytes::from_static(body)))
}
async fn code(host: &Arc<ToolRegistry>, id: &str) -> String {
    let response = post(host, &format!("/api/tools/{id}/launch"), b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    let url = bounded!(value(response))["url"]
        .as_str()
        .unwrap()
        .to_owned();
    url.split("vosu-launch=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_owned()
}
fn exchange(host: &ToolRegistry, tool: &str, code: &str) -> axum::response::Response {
    host.exchange_session(json!({"tool": tool, "code": code}).to_string().as_bytes())
}

#[tokio::test]
async fn legacy_tool_toml_is_listed_disabled_and_never_parsed_run_or_served() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("ran");
    let data = root.path().join("data");
    let legacy = put_legacy(&data, "legacy", &legacy_toml("legacy", &marker));
    // Malformed `tool.toml` is not parsed either; it is the same diagnosis.
    put_legacy(&data, "garbled", "not = [valid toml");
    let config = root.path().join("config");
    let state = json!({"legacy": {"enabled": true, "trusted": true, "autostart": true,
        "catalog_source": "owner/tools", "replaces": []}});
    atomic_json(&config.join("state.json"), &state).unwrap();
    atomic_json(
        &config.join("settings/legacy.json"),
        &json!({"key": "kept"}),
    )
    .unwrap();
    let tool_data = root.path().join("tool-data/legacy");
    fs::create_dir_all(&tool_data).unwrap();
    fs::write(tool_data.join("save"), "untouched").unwrap();
    let host = bounded!(host(root.path(), Vec::new()));
    for id in ["legacy", "garbled"] {
        let view = host.tool_view(&host.entry(id).unwrap());
        assert_eq!(view["status"], "disabled", "{id}");
        assert_eq!(view["status_detail"], NEEDS_UPDATE, "{id}");
        assert_eq!(view["enabled"], false);
        assert_eq!(view["ui_url"], Value::Null);
        assert_eq!(view["icon_url"], Value::Null);
    }
    assert_eq!(
        host.tool_view(&host.entry("legacy").unwrap())["trusted"],
        true
    );
    for action in ["enable", "trust", "launch"] {
        let response = post(&host, &format!("/api/tools/legacy/{action}"), b"{}").await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{action}");
        assert_eq!(bounded!(value(response))["error"], NEEDS_UPDATE);
    }
    // Never presented to the tool host: no UI, art or `_settings` is served.
    assert!(host.tool_host_views().borrow().tools.is_empty());
    bounded!(host.rescan());
    bounded!(host.rescan());
    bounded!(host.shutdown());
    // Counterexample: no `tool.toml` service or CLI ever runs.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!marker.exists());
    // Files, settings, trust/autostart record and data stay for M8.
    let saved: Value =
        serde_json::from_slice(&fs::read(config.join("state.json")).unwrap()).unwrap();
    assert_eq!(saved, state);
    assert_eq!(
        fs::read_to_string(config.join("settings/legacy.json")).unwrap(),
        "{\n  \"key\": \"kept\"\n}\n"
    );
    assert_eq!(
        fs::read_to_string(tool_data.join("save")).unwrap(),
        "untouched"
    );
    assert!(legacy.join("tool.toml").is_file());
}

#[tokio::test]
async fn invalid_manifest_never_falls_back_to_tool_toml() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("ran");
    let dir = put(&root.path().join("dev"), "both", "");
    fs::write(dir.join("tool.toml"), legacy_toml("both", &marker)).unwrap();
    let host = bounded!(host(root.path(), vec![dir.clone()]));
    assert_eq!(host.status(&host.entry("both").unwrap()).0, "running");
    fs::write(dir.join("manifest.toml"), "invalid v2").unwrap();
    bounded!(host.rescan());
    let entry = host.entry("both").unwrap();
    assert_eq!(host.status(&entry).0, "invalid");
    assert!(!entry.legacy && entry.manifest.is_none());
    assert!(host.tool_host_views().borrow().tools.is_empty());
    bounded!(host.shutdown());
    assert!(!marker.exists());
}

#[tokio::test]
async fn tool_view_has_exactly_the_2_0_fields() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(&root.path().join("dev"), "demo", "");
    let host = bounded!(host(root.path(), vec![dev]));
    let listed = bounded!(value(
        host.dashboard(request(Method::GET, "/api/tools"), Bytes::new())
            .await
    ));
    let view = &listed["tools"][0];
    let mut keys: Vec<_> = view.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "author",
            "banner_url",
            "description",
            "enabled",
            "has_settings",
            "homepage",
            "icon_url",
            "id",
            "name",
            "optional",
            "requires",
            "source",
            "status",
            "status_detail",
            "trusted",
            "ui_url",
            "update_available",
            "version",
        ]
    );
    assert_eq!(view["status"], "running");
    assert_eq!(view["ui_url"], "http://127.0.0.1:25150/tools/demo/");
    assert_eq!(view["icon_url"], "http://127.0.0.1:25150/tools/demo/_icon");
    assert_eq!(view["banner_url"], Value::Null);
    // No path, process, data, service, CLI or permission field remains.
    assert!(
        !view
            .to_string()
            .contains(&root.path().display().to_string())
    );
    for (method, path) in [
        (Method::GET, "/api/tools/demo/logs"),
        (Method::POST, "/api/tools/demo/start"),
        (Method::POST, "/api/tools/demo/open-data"),
    ] {
        assert_eq!(
            bounded!(host.dashboard(request(method, path), Bytes::new())).status(),
            StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
    bounded!(host.shutdown());
}

#[tokio::test]
async fn discovery_precedence_and_invalid_settings() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(&root.path().join("dev"), "demo", "");
    put(&root.path().join("data"), "demo", "");
    let bad = put(&root.path().join("data"), "bad", "");
    fs::write(bad.join("settings.json"), "not json").unwrap();
    let wrong = put(&root.path().join("data"), "wrong", "");
    fs::rename(&wrong, root.path().join("data/renamed")).unwrap();
    fs::create_dir_all(root.path().join("data/.stage-x")).unwrap();
    fs::create_dir_all(root.path().join("data/empty")).unwrap();
    let host = bounded!(host(root.path(), vec![dev.clone()]));
    assert_eq!(host.entry("demo").unwrap().root, dev);
    assert_eq!(host.entry("demo").unwrap().source, "dev");
    assert!(
        host.status(&host.entry("bad").unwrap())
            .1
            .unwrap()
            .contains("settings.json")
    );
    assert_eq!(host.status(&host.entry("wrong").unwrap()).0, "invalid");
    assert!(host.entry("empty").is_err());
    assert!(host.entry(".stage-x").is_err());
    bounded!(host.shutdown());
}

#[tokio::test]
async fn launch_codes_are_bound_single_use_and_expiring() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(root.path(), "demo", "");
    let other = put(root.path(), "other", "");
    let host = bounded!(host(root.path(), vec![dev, other]));
    let launch = bounded!(value(
        post(&host, "/api/tools/demo/launch", b"{\"theme\":\"light\"}").await
    ));
    let url = launch["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:25150/tools/demo/#vosu-launch="));
    assert!(url.ends_with("&theme=light"));
    let code = url
        .split("vosu-launch=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    assert_eq!(code.len(), 64);
    // Another tool's code is consumed and refused.
    let theirs = bounded!(super::tests::code(&host, "other"));
    assert_eq!(
        exchange(&host, "demo", &theirs).status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        exchange(&host, "other", &theirs).status(),
        StatusCode::FORBIDDEN
    );
    let response = exchange(&host, "demo", code);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    let session = bounded!(value(response))["session"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(host.sessions.lock().contains_key(&session));
    // Counterexample: the same launch code replayed.
    assert_eq!(
        exchange(&host, "demo", code).status(),
        StatusCode::FORBIDDEN
    );
    host.codes.lock().insert(
        "expired".into(),
        Code {
            tool: "demo".into(),
            expires: Instant::now() - Duration::from_secs(1),
        },
    );
    assert_eq!(
        exchange(&host, "demo", "expired").status(),
        StatusCode::FORBIDDEN
    );
    for body in [
        &b"not json"[..],
        b"{\"tool\":\"demo\"}",
        b"{\"code\":\"x\"}",
    ] {
        assert_eq!(
            host.exchange_session(body).status(),
            StatusCode::BAD_REQUEST
        );
    }
    // A disabled tool cannot launch, and its pending codes die with it.
    let pending = bounded!(super::tests::code(&host, "demo"));
    assert_eq!(
        post(&host, "/api/tools/demo/disable", b"").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        exchange(&host, "demo", &pending).status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&host, "/api/tools/demo/launch", b"").await.status(),
        StatusCode::BAD_REQUEST
    );
    bounded!(host.shutdown());
}

#[tokio::test]
async fn pending_code_registry_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(root.path(), "demo", "");
    let host = bounded!(host(root.path(), vec![dev]));
    for index in 0..1024 {
        host.codes.lock().insert(
            index.to_string(),
            Code {
                tool: "demo".into(),
                expires: Instant::now() + Duration::from_secs(60),
            },
        );
    }
    assert_eq!(
        post(&host, "/api/tools/demo/launch", b"").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(host.codes.lock().len(), 1024);
    bounded!(host.shutdown());
}

#[tokio::test]
async fn disable_uninstall_and_replacement_invalidate_sessions_and_cancel_their_jobs() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(root.path(), "demo", "");
    let host = bounded!(host(root.path(), vec![dev.clone()]));
    let jobs = vosu_core::actions::JobRegistry::new();
    host.set_jobs(jobs.clone());
    let open = |host: &Arc<ToolRegistry>| {
        let host = host.clone();
        async move {
            let code = code(&host, "demo").await;
            bounded!(value(exchange(&host, "demo", &code)))["session"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let owner = |token: &str| {
        let vosu_core::actions::Principal::Tool(owner) =
            host.authenticate_action_session(token).unwrap()
        else {
            panic!("launch sessions authenticate tool principals");
        };
        owner
    };
    for invalidate in ["disable", "uninstall", "replace"] {
        let token = bounded!(open(&host));
        let kept = bounded!(open(&host));
        let revoked_owner = owner(&token);
        let job = jobs
            .create(
                vosu_core::actions::Principal::Tool(revoked_owner.clone()),
                "fixture.work".into(),
                None,
            )
            .unwrap();
        let other = owner(&kept);
        let pending = bounded!(code(&host, "demo"));
        match invalidate {
            "disable" => {
                post(&host, "/api/tools/demo/disable", b"").await;
            }
            "uninstall" => bounded!(host.revoke_installed("demo")),
            _ => {
                fs::write(
                    dev.join("manifest.toml"),
                    manifest("demo", "").replace("Test demo", "Replaced"),
                )
                .unwrap();
                bounded!(host.rescan());
            }
        }
        assert!(
            host.authenticate_action_session(&token).is_err(),
            "{invalidate}"
        );
        assert!(
            host.authenticate_action_session(&kept).is_err(),
            "{invalidate}"
        );
        assert_eq!(
            exchange(&host, "demo", &pending).status(),
            StatusCode::FORBIDDEN,
            "{invalidate}"
        );
        // Advisory cancellation was already requested for the revoked owner.
        assert!(jobs.cancel_owner(&revoked_owner).is_empty(), "{invalidate}");
        assert!(jobs.cancel_owner(&other).is_empty());
        assert!(
            jobs.get(&vosu_core::actions::Principal::Tool(other), &job.id)
                .is_err()
        );
        post(&host, "/api/tools/demo/enable", b"").await;
        bounded!(host.rescan());
    }
    bounded!(host.shutdown());
}

#[tokio::test]
async fn tool_host_view_presents_servable_packages_and_revokes_replaced_leases() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(root.path(), "demo", "");
    let legacy = put_legacy(root.path(), "old", "manifest_version=1");
    let host = bounded!(host(root.path(), vec![dev.clone(), legacy]));
    let views = host.tool_host_views();
    let first = views.borrow().clone();
    assert_eq!(
        first.dashboard_origin.as_deref(),
        Some("http://127.0.0.1:25151")
    );
    assert_eq!(first.tools.len(), 1);
    let tool = first.tools[0].clone();
    assert_eq!((tool.id.as_str(), tool.servable), ("demo", true));
    assert_eq!(
        (tool.ui_root.as_str(), tool.ui_entry.as_str()),
        ("web", "index.html")
    );
    assert_eq!(tool.icon.as_deref(), Some("art/icon.svg"));
    assert_eq!(tool.settings.as_deref(), Some(&b"{}"[..]));
    let file = bounded!(
        tool.root
            .open(&RelativeChild::parse("web/index.html").unwrap())
    )
    .unwrap();
    assert_eq!(bounded!(file.read(0, 64)).unwrap(), b"tool-ui");
    // An unchanged registry is not republished.
    bounded!(host.rescan());
    assert_eq!(views.borrow().generation, first.generation);
    post(&host, "/api/tools/demo/disable", b"").await;
    let disabled = views.borrow().clone();
    assert!(disabled.generation > first.generation);
    assert!(!disabled.tools[0].servable);
    post(&host, "/api/tools/demo/enable", b"").await;
    assert!(views.borrow().tools[0].servable);
    fs::write(
        dev.join("manifest.toml"),
        manifest("demo", "").replace("1.0.0", "1.1.0"),
    )
    .unwrap();
    bounded!(host.rescan());
    assert_eq!(views.borrow().tools[0].version, "1.1.0");
    // The replaced package's lease stops opening files.
    assert_eq!(
        bounded!(
            tool.root
                .open(&RelativeChild::parse("web/index.html").unwrap())
        )
        .err(),
        Some(FileError::TargetGone)
    );
    bounded!(host.shutdown());
}

/// A7.3: with the dashboard off nothing serves its origin, so the bridge's
/// `dashboardUrl` is `null` rather than a dead configured address.
#[tokio::test]
async fn tool_host_view_has_no_dashboard_origin_while_the_dashboard_is_off() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(root.path(), "demo", "");
    let host = bounded!(host(root.path(), vec![dev]));
    let views = host.tool_host_views();
    let origins = crate::server::origins::Origins::new(
        "http://127.0.0.1:25150".into(),
        "http://127.0.0.1:25151".into(),
    )
    .unwrap();
    host.activate(Some(Arc::new(origins.without_dashboard())));
    let off = views.borrow().clone();
    assert_eq!(off.dashboard_origin, None);
    assert_eq!(off.tools.len(), 1);
    bounded!(host.shutdown());

    let unset = bounded!(ToolRegistry::new(
        root.path().join("config"),
        root.path().join("data"),
        root.path().join("cache"),
        ExtraRoots { dev: Vec::new() },
        "http://127.0.0.1:25150".into(),
        None,
    ))
    .unwrap();
    assert_eq!(unset.tool_host_views().borrow().dashboard_origin, None);
    bounded!(unset.shutdown());
}

#[tokio::test]
async fn package_lease_is_confined_to_the_package() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(root.path(), "demo", "");
    fs::write(root.path().join("secret.txt"), "outside").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path().join("secret.txt"), dev.join("web/link.txt")).unwrap();
    let host = bounded!(host(root.path(), vec![dev]));
    let tool = host.tool_host_views().borrow().tools[0].clone();
    assert!(RelativeChild::parse("../secret.txt").is_err());
    assert!(
        bounded!(tool.root.open(&RelativeChild::parse("web").unwrap())).is_err(),
        "directories are not files"
    );
    #[cfg(unix)]
    assert!(
        bounded!(
            tool.root
                .open(&RelativeChild::parse("web/link.txt").unwrap())
        )
        .is_err()
    );
    bounded!(host.shutdown());
}

#[tokio::test]
async fn organization_defaults_migrate_at_startup_without_expanding_identity_trust() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    fs::create_dir_all(&config).unwrap();
    // Former organization spellings below are migration inputs and near-miss fixtures.
    let sources = json!([
        {"source":"owner/custom","enabled":true},
        {"source":"shiruviaa/vosulay","enabled":false},
        {"source":"shiruviaa/vosu-tools","enabled":true},
        {"source":"shiruviaa/vosu-overlays","enabled":false},
        {"source":"shiruviaa/vosu-tools@v1","enabled":true},
        {"source":"https://github.com/shiruviaa/vosulay/releases/latest/download/catalog.json","enabled":true},
        {"source":"Shiruviaa/vosulay","enabled":true}
    ]);
    atomic_json(&config.join("catalog-sources.json"), &sources).unwrap();
    // The earlier two-to-three-source migration marker must not inhibit renaming.
    atomic_json(&config.join("catalog-sources-v2.json"), &true).unwrap();
    let mut states = BTreeMap::new();
    for (id, source) in [
        ("official-tools", "shiruviaa/vosu-tools"),
        ("official-vosulay", "shiruviaa/vosulay"),
        ("overlay-tool", "shiruviaa/vosu-overlays"),
        ("custom", "owner/custom"),
        ("tagged", "shiruviaa/vosu-tools@v1"),
        ("approved-overlay-tool", "shiruviaa/vosu-overlays"),
    ] {
        let state = Saved {
            catalog_source: Some(source.into()),
            trusted: id == "approved-overlay-tool",
            ..Saved::default()
        };
        states.insert(id, state);
    }
    atomic_json(&config.join("state.json"), &states).unwrap();
    atomic_json(
        &config.join("overlay-catalog-state.json"),
        &json!({"overlay":{"source":"shiruviaa/vosu-overlays","version":"1.0.0"}}),
    )
    .unwrap();
    let first = bounded!(host(root.path(), Vec::new()));
    {
        let saved = first.saved.lock();
        for id in ["official-tools", "official-vosulay"] {
            assert!(saved[id].official && saved[id].trusted);
            assert!(
                saved[id]
                    .catalog_source
                    .as_deref()
                    .unwrap()
                    .starts_with("18-lab-dev/")
            );
        }
        for id in ["overlay-tool", "custom", "tagged"] {
            assert!(!saved[id].official && !saved[id].trusted);
        }
        assert_eq!(
            saved["custom"].catalog_source.as_deref(),
            Some("owner/custom")
        );
        assert_eq!(
            saved["tagged"].catalog_source.as_deref(),
            Some("shiruviaa/vosu-tools@v1")
        );
        assert!(saved["approved-overlay-tool"].trusted);
        assert!(!saved["approved-overlay-tool"].official);
        assert_eq!(
            saved["approved-overlay-tool"].catalog_source.as_deref(),
            Some("shiruviaa/vosu-overlays")
        );
    }
    // Catalog v2 owns sources; the 1.x list and overlay state are M8
    // migration inputs and are never rewritten by the tool registry.
    let legacy: Value =
        serde_json::from_slice(&fs::read(config.join("catalog-sources.json")).unwrap()).unwrap();
    assert_eq!(legacy, sources);
    let overlay: Value =
        serde_json::from_slice(&fs::read(config.join("overlay-catalog-state.json")).unwrap())
            .unwrap();
    assert_eq!(overlay["overlay"]["source"], "shiruviaa/vosu-overlays");
    assert!(!config.join("catalog-sources-v3.json").exists());
    bounded!(first.shutdown());
    let paths = [
        "catalog-sources.json",
        "state.json",
        "overlay-catalog-state.json",
    ];
    let before = paths.map(|name| fs::read(config.join(name)).unwrap());
    let second = bounded!(host(root.path(), Vec::new()));
    for (name, bytes) in paths.iter().zip(before) {
        assert_eq!(fs::read(config.join(name)).unwrap(), bytes);
    }
    bounded!(second.shutdown());
}

#[tokio::test]
async fn tool_settings_and_approval_persist_outside_overlay_settings() {
    let root = tempfile::tempdir().unwrap();
    let tool = put(&root.path().join("data"), "user-tool", "");
    fs::write(tool.join("settings.json"), r#"[{"uniqueID":"count","type":"number","value":"12"},{"uniqueID":"enabled","type":"checkbox","value":"true"}]"#).unwrap();
    let first = bounded!(host(root.path(), Vec::new()));
    assert_eq!(
        first.status(&first.entry("user-tool").unwrap()).0,
        "untrusted"
    );
    // Untrusted: listed, never presented as servable or launchable.
    assert!(!first.tool_host_views().borrow().tools[0].servable);
    for action in ["trust", "disable"] {
        assert_eq!(
            post(&first, &format!("/api/tools/user-tool/{action}"), b"")
                .await
                .status(),
            StatusCode::OK
        );
    }
    let settings = bounded!(value(
        first
            .dashboard(
                request(Method::GET, "/api/tools/user-tool/settings"),
                Bytes::new(),
            )
            .await,
    ));
    assert_eq!(settings["values"]["count"], 12.0);
    assert_eq!(settings["values"]["enabled"], true);
    let saved = post(
        &first,
        "/api/tools/user-tool/settings",
        br#"[{"uniqueID":"count","value":"25"}]"#,
    )
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert!(root.path().join("config/settings/user-tool.json").is_file());
    assert!(!tool.join("settings.schema.toml").exists());
    bounded!(first.shutdown());
    drop(first);
    let second = bounded!(host(root.path(), Vec::new()));
    let entry = second.entry("user-tool").unwrap();
    assert!(second.trusted(&entry));
    assert_eq!(second.status(&entry).0, "disabled");
    assert_eq!(second.settings(&entry).unwrap()["count"], 25.0);
    bounded!(second.shutdown());
}

#[tokio::test]
async fn password_settings_never_reach_the_dashboard_or_the_tool_host() {
    let root = tempfile::tempdir().unwrap();
    let tool = put(&root.path().join("dev"), "secretive", "");
    fs::write(
        tool.join("settings.json"),
        r#"[{"uniqueID":"secret","type":"password","value":"default-secret"},{"uniqueID":"name","type":"text","value":"x"}]"#,
    )
    .unwrap();
    let host = bounded!(host(root.path(), vec![tool]));
    let read = || async {
        bounded!(value(
            host.dashboard(
                request(Method::GET, "/api/tools/secretive/settings"),
                Bytes::new(),
            )
            .await,
        ))
    };
    let save = |body: &'static [u8]| post(&host, "/api/tools/secretive/settings", body);
    let effective = || host.settings(&host.entry("secretive").unwrap()).unwrap()["secret"].clone();
    let settings = bounded!(read());
    assert_eq!(settings["values"]["secret"], "");
    assert_eq!(settings["settings"][0]["value"], "");
    assert_eq!(settings["secrets"]["secret"], true);
    assert!(!settings.to_string().contains("default-secret"));
    assert_eq!(
        bounded!(save(br#"[{"uniqueID":"secret","value":"hunter2"}]"#)).status(),
        StatusCode::OK
    );
    assert_eq!(effective(), "hunter2");
    // Blank or omitted keeps the secret; null clears it.
    bounded!(save(
        br#"[{"uniqueID":"secret","value":""},{"uniqueID":"name","value":"y"}]"#
    ));
    bounded!(save(br#"[{"uniqueID":"name","value":"z"}]"#));
    assert_eq!(effective(), "hunter2");
    assert!(!bounded!(read()).to_string().contains("hunter2"));
    // The tool host's `_settings` values omit password settings entirely.
    let presented = host.tool_host_views().borrow().tools[0]
        .settings
        .clone()
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&presented).unwrap(),
        json!({"name": "z"})
    );
    bounded!(save(br#"[{"uniqueID":"secret","value":null}]"#));
    assert_eq!(effective(), "");
    assert_eq!(bounded!(read())["secrets"]["secret"], false);
    bounded!(host.shutdown());
}

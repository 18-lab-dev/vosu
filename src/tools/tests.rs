use super::*;
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::{HeaderMap, Method, StatusCode},
};

fn manifest(id: &str, requires: &str) -> String {
    format!(
        "manifest_version=1\nid='{id}'\nname='Test'\nversion='1.0.0'\n[ui]\nroot='web'\n{requires}"
    )
}
fn put(root: &Path, id: &str, requires: &str) -> PathBuf {
    let dir = root.join(id);
    fs::create_dir_all(dir.join("web")).unwrap();
    fs::write(dir.join("tool.toml"), manifest(id, requires)).unwrap();
    fs::write(dir.join("web/index.html"), "tool-ui").unwrap();
    dir
}
async fn host(root: &Path, dev: Vec<PathBuf>) -> Arc<ToolHost> {
    let host = ToolHost::new(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        ExtraRoots { dev },
        "http://127.0.0.1:25150".into(),
        "http://127.0.0.1:25151".into(),
        Arc::new(|| json!({"client":null,"songs_dir":null,"beatmap":null})),
    )
    .await
    .unwrap();
    host.activate(None);
    host
}
async fn value(response: axum::response::Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
}
fn request(method: Method, path: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap()
}

#[test]
fn service_roles_validate_defaults_and_backend_startup() {
    let base = manifest("roles", "[service]\ncommand=['test']");
    assert_eq!(
        Manifest::parse(&base).unwrap().service.unwrap().role,
        "background"
    );
    for (suffix, valid) in [
        ("\nrole='backend'", true),
        ("\nrole='backend'\nstartup='always'", false),
        ("\nrole='background'\nstartup='always'", true),
        ("\nrole='unknown'", false),
    ] {
        assert_eq!(
            Manifest::parse(&format!("{base}{suffix}")).is_ok(),
            valid,
            "{suffix}"
        );
    }
}

#[tokio::test]
async fn autostart_precedence_persistence_backend_rejection_and_dependency_order() {
    let root = tempfile::tempdir().unwrap();
    let dev = root.path().join("dev");
    let always = put(
        &dev,
        "always",
        "[service]\ncommand=['test']\nstartup='always'",
    );
    let demand = put(
        &dev,
        "demand",
        "[service]\ncommand=['test']\n[[requires]]\nid='always'\nversion='^1'",
    );
    let backend = put(
        &dev,
        "backend",
        "[service]\ncommand=['test']\nrole='backend'",
    );
    let roots = vec![always, demand, backend];
    // No activation: assert launch intent without spawning fixture processes.
    let make = || {
        ToolHost::new(
            root.path().join("config"),
            root.path().join("data"),
            root.path().join("cache"),
            ExtraRoots { dev: roots.clone() },
            "http://127.0.0.1:25150".into(),
            "http://127.0.0.1:25151".into(),
            Arc::new(|| json!({})),
        )
    };
    let host = make().await.unwrap();
    for (id, default) in [("always", true), ("demand", false)] {
        let entry = host.entry(id).unwrap();
        assert_eq!(host.view(&entry)["starts_with_vosu"], default);
        for (autostart, expected) in [
            (json!(true), true),
            (json!(false), false),
            (Value::Null, default),
        ] {
            let response = host
                .dashboard(
                    request(Method::PUT, &format!("/api/tools/{id}/autostart")),
                    Bytes::from(json!({"autostart":autostart}).to_string()),
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let view = value(response).await;
            assert_eq!(view["autostart"], autostart);
            assert_eq!(view["starts_with_vosu"], expected);
            let reloaded = make().await.unwrap();
            assert_eq!(
                reloaded.view(&reloaded.entry(id).unwrap())["autostart"],
                autostart
            );
            assert_eq!(reloaded.entry(id).unwrap().state.lock().desired, expected);
            reloaded.shutdown().await;
        }
    }
    for body in [
        r#"{"autostart":true}"#,
        r#"{"autostart":false}"#,
        r#"{"autostart":null}"#,
    ] {
        assert_eq!(
            host.dashboard(
                request(Method::PUT, "/api/tools/backend/autostart"),
                Bytes::from_static(body.as_bytes())
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for action in ["start", "stop", "restart"] {
        assert_eq!(
            host.dashboard(
                request(Method::POST, &format!("/api/tools/backend/{action}")),
                Bytes::new()
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let view = host.view(&host.entry("backend").unwrap());
    assert_eq!(view["interfaces"], json!(["ui"]));
    assert_eq!(view["service_role"], "backend");
    let mut order = Vec::new();
    host.visit(
        "demand",
        &mut HashSet::new(),
        &mut HashSet::new(),
        &mut order,
    )
    .unwrap();
    assert_eq!(order, ["always", "demand"]);
    host.shutdown().await;
}

#[tokio::test]
async fn overlays_route_needs_a_token_and_the_context_permission() {
    let root = tempfile::tempdir().unwrap();
    let reader = put(
        &root.path().join("dev"),
        "reader",
        "[permissions]\nhost=['context']",
    );
    let plain = put(&root.path().join("dev"), "plain", "");
    let host = host(root.path(), vec![reader, plain]).await;
    for (tool, token) in [("reader", "reader-cli-token"), ("plain", "plain-cli-token")] {
        host.cli_sessions.lock().insert(
            format!("{tool}-cli"),
            CliSession {
                tool: tool.into(),
                token: token.into(),
                expires: Instant::now() + Duration::from_secs(60),
            },
        );
    }
    let get = |token: Option<&'static str>| {
        let mut req = request(Method::GET, "/tools/_host/v1/overlays");
        if let Some(token) = token {
            req.headers_mut()
                .insert("x-vosu-tool-token", token.parse().unwrap());
        }
        host.overlay(req, Bytes::new())
    };
    assert_eq!(get(None).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        get(Some("plain-cli-token")).await.status(),
        StatusCode::FORBIDDEN
    );
    // Before the daemon wires its overlay catalog in.
    assert_eq!(
        get(Some("reader-cli-token")).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let static_root = root.path().join("static");
    fs::create_dir_all(static_root.join("hud")).unwrap();
    fs::write(
        static_root.join("hud/metadata.txt"),
        "Name: HUD\nAuthor: 18-LAB\nVersion: 1.0.2\nResolution: 640x120\n",
    )
    .unwrap();
    fs::write(static_root.join("hud/index.html"), "hud").unwrap();
    fs::write(
        static_root.join("hud/settings.json"),
        r#"[{"uniqueID":"color","type":"color","value":"secret-ish"}]"#,
    )
    .unwrap();
    host.set_counters(Arc::new(
        crate::counters::ReloadableCounterService::production(
            static_root.clone(),
            root.path().join("settings"),
            root.path().join("counter-cache"),
        ),
    ));
    let response = get(Some("reader-cli-token")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut listing = value(response).await;
    let folder = listing["overlays"][0]["folder"].take();
    let folder = PathBuf::from(folder.as_str().unwrap());
    assert!(folder.is_absolute() && folder.join("index.html").is_file());
    assert!(!folder.to_string_lossy().starts_with(r"\\?\"));
    assert_eq!(
        listing,
        json!({"overlays":[{
            "id":"hud","name":"HUD","nickname":null,"alias":null,"author":"18-LAB",
            "version":"1.0.2","folder":null,"width":640,"height":120,
        }]})
    );
    assert_eq!(
        get(Some("plain-cli-token")).await.status(),
        StatusCode::FORBIDDEN
    );
    // Read-only.
    let mut post = request(Method::POST, "/tools/_host/v1/overlays");
    post.headers_mut()
        .insert("x-vosu-tool-token", "reader-cli-token".parse().unwrap());
    assert_eq!(
        host.overlay(post, Bytes::new()).await.status(),
        StatusCode::NOT_FOUND
    );
    host.shutdown().await;
}

#[tokio::test]
async fn theme_serves_shared_tokens_and_components_and_the_reference_page() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), Vec::new()).await;
    let get = |path: &'static str| host.overlay(request(Method::GET, path), Bytes::new());
    let theme = get("/tools/_host/v1/theme.css").await;
    assert_eq!(theme.status(), StatusCode::OK);
    assert_eq!(theme.headers()["content-type"], "text/css; charset=utf-8");
    let css =
        String::from_utf8(to_bytes(theme.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    // One source: the dashboard's token file comes first, unchanged.
    assert!(css.starts_with(include_str!("../../assets/server/tokens.css")));
    for class in [
        ".v-app",
        ".v-bar",
        ".v-panel",
        ".v-btn--primary",
        ".v-select",
        ".v-file",
        ".v-help",
        ".v-drawer",
    ] {
        assert!(css.contains(class), "{class}");
    }
    let design = get("/tools/_host/v1/design").await;
    assert_eq!(design.status(), StatusCode::OK);
    assert_eq!(design.headers()["content-type"], "text/html; charset=utf-8");
    let html = String::from_utf8(
        to_bytes(design.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains(r#"href="/tools/_host/v1/theme.css""#));
    host.shutdown().await;
}

#[tokio::test]
async fn art_is_served_in_every_status_and_sandboxed() {
    let root = tempfile::tempdir().unwrap();
    let dir = put(&root.path().join("data"), "artful", "");
    fs::create_dir_all(dir.join("art")).unwrap();
    fs::write(
        dir.join("tool.toml"),
        "manifest_version=1\nid='artful'\nname='Art'\nversion='1.0.0'\nicon='art/icon.svg'\nbanner='art/banner.png'\n[ui]\nroot='web'\n",
    )
    .unwrap();
    fs::write(
        dir.join("art/icon.svg"),
        "<svg xmlns='http://www.w3.org/2000/svg'/>",
    )
    .unwrap();
    fs::write(dir.join("art/banner.png"), b"\x89PNG").unwrap();
    fs::write(dir.join("art/other.svg"), "<svg/>").unwrap();
    let host = host(root.path(), Vec::new()).await;
    let entry = host.entry("artful").unwrap();
    // A user tool starts untrusted: its UI is unavailable, its art is not.
    assert_eq!(host.status(&entry).0, "untrusted");
    for status in ["untrusted", "failed"] {
        entry.state.lock().status = status.into();
        for path in ["/tools/artful/_icon", "/tools/artful/_banner"] {
            let response = host.overlay(request(Method::GET, path), Bytes::new()).await;
            assert_eq!(response.status(), StatusCode::OK, "{status} {path}");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .ends_with("sandbox")
            );
        }
        for path in ["/tools/artful/", "/tools/artful/art/other.svg"] {
            assert_eq!(
                host.overlay(request(Method::GET, path), Bytes::new())
                    .await
                    .status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{status} {path}"
            );
        }
    }
    host.shutdown().await;
}

#[tokio::test]
async fn discovery_precedence_invalid_settings_and_cycles() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(&root.path().join("dev"), "demo", "");
    put(&root.path().join("data"), "demo", "");
    let a = put(
        &root.path().join("data"),
        "a",
        "[[requires]]\nid='b'\nversion='>=1.0.0'",
    );
    put(
        &root.path().join("data"),
        "b",
        "[[requires]]\nid='a'\nversion='>=1.0.0'",
    );
    let bad = put(&root.path().join("data"), "bad", "");
    fs::write(bad.join("settings.json"), "not json").unwrap();
    let host = host(root.path(), vec![dev.clone()]).await;
    assert_eq!(host.entry("demo").unwrap().root, dev);
    assert_eq!(host.entry("demo").unwrap().source, "dev");
    assert_eq!(host.status(&host.entry("a").unwrap()).0, "invalid");
    assert_eq!(host.status(&host.entry("b").unwrap()).0, "invalid");
    assert!(
        host.status(&host.entry("bad").unwrap())
            .1
            .unwrap()
            .contains("settings.json")
    );
    assert!(a.exists());
    host.shutdown().await;
}
#[tokio::test]
async fn launch_codes_are_bound_single_use_and_expiring_and_ui_is_confined() {
    let root = tempfile::tempdir().unwrap();
    let dev = put(root.path(), "demo", "");
    let host = host(root.path(), vec![dev]).await;
    let launch = value(
        host.dashboard(
            request(Method::POST, "/api/tools/demo/launch"),
            Bytes::from_static(b"{\"theme\":\"light\"}"),
        )
        .await,
    )
    .await;
    let url = launch["url"].as_str().unwrap();
    assert!(url.ends_with("&theme=light"));
    let code = url
        .split("vosu-launch=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let payload = Bytes::from(json!({"tool":"demo","code":code}).to_string());
    let response = host
        .overlay(
            request(Method::POST, "/tools/_host/v1/session"),
            payload.clone(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let session = value(response).await["session"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(host.sessions.lock().contains_key(&session));
    assert_eq!(
        host.overlay(request(Method::POST, "/tools/_host/v1/session"), payload)
            .await
            .status(),
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
        host.overlay(
            request(Method::POST, "/tools/_host/v1/session"),
            Bytes::from_static(b"{\"tool\":\"demo\",\"code\":\"expired\"}")
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    for path in [
        "/tools/demo/../tool.toml",
        "/tools/demo/%2e%2e/tool.toml",
        "/tools/demo/%2fetc/passwd",
    ] {
        assert_eq!(
            host.overlay(request(Method::GET, path), Bytes::new())
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        host.overlay(request(Method::GET, "/tools/demo/"), Bytes::new())
            .await
            .status(),
        StatusCode::OK
    );
    host.shutdown().await;
}
#[test]
fn reservation_covers_validation_and_encoded_static_paths() {
    for name in ["tools", "Tools", "TOOLS"] {
        assert!(crate::counters::validate_component(name).is_err());
    }
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("tools")).unwrap();
    fs::write(root.path().join("tools/index.html"), "secret").unwrap();
    for path in ["tools/index.html", "%74ools/index.html", "TOOLS/index.html"] {
        assert!(matches!(
            crate::server::files::resolve_encoded_counter_resource(
                Some(root.path()),
                path,
                path,
                crate::server::files::MissingKind::NotFound
            )
            .unwrap(),
            crate::server::files::ResourceOutcome::Missing(_)
        ));
    }
}
#[tokio::test]
async fn access_matrix_includes_sessions_dependencies_and_method_overlap() {
    let root = tempfile::tempdir().unwrap();
    let target = put(root.path(), "target", "");
    let dependent = put(
        root.path(),
        "dependent",
        "[[requires]]\nid='target'\nversion='>=1.0.0'",
    );
    let stranger = put(root.path(), "stranger", "");
    let host = host(root.path(), vec![target, dependent, stranger]).await;
    let target = host.entry("target").unwrap();
    let dep = host.entry("dependent").unwrap();
    let stranger = host.entry("stranger").unwrap();
    for (entry, token) in [(&dep, "dependency-secret"), (&stranger, "stranger-secret")] {
        let mut state = entry.state.lock();
        state.status = "running".into();
        state.token = Some(token.into());
    }
    let mut headers = HeaderMap::new();
    assert_eq!(
        host.caller(&target, &headers, false).as_deref(),
        Some("overlay")
    );
    assert!(host.caller(&target, &headers, true).is_none());
    headers.insert("x-vosu-tool-token", "stranger-secret".parse().unwrap());
    assert!(host.caller(&target, &headers, true).is_none());
    // Optional dependencies: a valid non-dependent token reads like an overlay.
    assert_eq!(
        host.caller(&target, &headers, false).as_deref(),
        Some("tool:stranger")
    );
    headers.insert("x-vosu-tool-token", "forged-secret".parse().unwrap());
    assert!(host.caller(&target, &headers, false).is_none());
    headers.insert("x-vosu-tool-token", "dependency-secret".parse().unwrap());
    assert_eq!(
        host.caller(&target, &headers, true).as_deref(),
        Some("tool:dependent")
    );
    headers.clear();
    host.sessions
        .lock()
        .insert("session-secret".into(), "target".into());
    headers.insert("x-vosu-tool-session", "session-secret".parse().unwrap());
    assert_eq!(host.caller(&target, &headers, true).as_deref(), Some("ui"));
    assert!(host.caller(&stranger, &headers, true).is_none());
    let mut manifest = target.manifest.clone().unwrap();
    manifest.api.read = vec!["/v1/jobs".into()];
    manifest.api.write = vec!["/v1/jobs".into(), "/v1/private".into()];
    for method in ["GET", "HEAD"] {
        assert_eq!(manifest.access(method, "/v1/jobs/1"), Some(false));
        assert_eq!(manifest.access(method, "/v1/private"), Some(true));
    }
    for method in ["POST", "PUT", "DELETE", "PATCH"] {
        assert_eq!(manifest.access(method, "/v1/jobs/1"), Some(true));
        assert_eq!(manifest.access(method, "/v1/no"), None);
    }
    host.shutdown().await;
}
#[cfg(unix)]
#[tokio::test]
async fn supervisor_readiness_backoff_logs_and_stdin_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool-host");
    let host = host(root.path(), vec![fixture]).await;
    let response = host
        .overlay(
            request(Method::GET, "/tools/fixture/api/v1/read?hello=world"),
            Bytes::new(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let first = value(response).await;
    assert_eq!(first["path"], "/v1/read?hello=world");
    assert_eq!(first["caller"], "overlay");
    assert_eq!(
        host.overlay(
            request(Method::POST, "/tools/fixture/api/v1/write"),
            Bytes::new()
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    host.sessions
        .lock()
        .insert("fixture-session".into(), "fixture".into());
    let mut write = request(Method::POST, "/tools/fixture/api/v1/write");
    write
        .headers_mut()
        .insert("x-vosu-tool-session", "fixture-session".parse().unwrap());
    assert_eq!(
        host.overlay(write, Bytes::new()).await.status(),
        StatusCode::OK
    );
    let entry = host.entry("fixture").unwrap();
    let mut crash = request(Method::POST, "/tools/fixture/api/v1/crash");
    crash
        .headers_mut()
        .insert("x-vosu-tool-session", "fixture-session".parse().unwrap());
    let before = Instant::now();
    assert_eq!(
        host.overlay(crash, Bytes::new()).await.status(),
        StatusCode::BAD_GATEWAY
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if entry.state.lock().restarts > 0 && entry.state.lock().status == "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(before.elapsed() >= Duration::from_secs(1));
    assert_ne!(
        entry.state.lock().pid.unwrap(),
        first["pid"].as_u64().unwrap() as u32
    );
    assert!(
        entry
            .logs
            .lock()
            .iter()
            .any(|line| line.text.contains("fixture started"))
    );
    let pid = entry.state.lock().pid.unwrap();
    host.shutdown().await;
    assert_eq!(
        fs::read_to_string(entry.data_dir.join("stdin-closed")).unwrap(),
        "yes"
    );
    #[cfg(target_os = "linux")]
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}
#[cfg(unix)]
#[tokio::test]
async fn voluntary_exit_stays_stopped_and_can_be_started_again() {
    let root = tempfile::tempdir().unwrap();
    let fixture = copied_fixture(root.path());
    let manifest = fs::read_to_string(fixture.join("tool.toml")).unwrap();
    fs::write(
        fixture.join("tool.toml"),
        manifest.replace("on-demand", "always"),
    )
    .unwrap();
    let host = host(root.path(), vec![fixture]).await;
    host.start("fixture", true).await.unwrap();
    let entry = host.entry("fixture").unwrap();
    let first_pid = entry.state.lock().pid.unwrap();
    let (port, token) = {
        let state = entry.state.lock();
        (state.port.unwrap(), state.token.clone().unwrap())
    };
    let _ = host
        .http
        .post(format!("http://127.0.0.1:{port}/v1/exit"))
        .header("x-vosu-tool-token", token)
        .send()
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if entry.state.lock().status == "stopped" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Cross multiple watcher ticks: an always service must not be resurrected.
    tokio::time::sleep(Duration::from_millis(2200)).await;
    {
        let state = entry.state.lock();
        assert_eq!(state.status, "stopped");
        assert_eq!(state.detail.as_deref(), Some("exited"));
        assert!(!state.desired);
        assert_eq!(state.restarts, 0);
        assert!(state.port.is_none() && state.token.is_none() && state.pid.is_none());
    }
    assert_eq!(
        host.overlay(
            request(Method::GET, "/tools/fixture/api/v1/read"),
            Bytes::new(),
        )
        .await
        .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(entry.state.lock().status, "stopped");
    host.start("fixture", true).await.unwrap();
    assert_eq!(entry.state.lock().status, "running");
    assert_ne!(entry.state.lock().pid, Some(first_pid));
    host.stop_serialized(&entry, true).await;
    assert!(!entry.state.lock().desired);
    assert_eq!(entry.state.lock().detail, None);
    assert_eq!(entry.state.lock().restarts, 0);
    // A vosux invocation's own CLI token is an explicit start request, unlike
    // overlay polling or another tool's dependency request.
    entry.state.lock().detail = Some("exited".into());
    host.cli_sessions.lock().insert(
        "fixture-cli".into(),
        CliSession {
            tool: "fixture".into(),
            token: "fixture-cli-token".into(),
            expires: Instant::now() + Duration::from_secs(60),
        },
    );
    let mut cli = request(Method::GET, "/tools/fixture/api/v1/read");
    cli.headers_mut()
        .insert("x-vosu-tool-token", "fixture-cli-token".parse().unwrap());
    assert_eq!(
        host.overlay(cli, Bytes::new()).await.status(),
        StatusCode::OK
    );
    assert_eq!(entry.state.lock().status, "running");
    host.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn voluntary_exit_before_readiness_is_not_a_crash() {
    let root = tempfile::tempdir().unwrap();
    let fixture = copied_fixture(root.path());
    let manifest = fs::read_to_string(fixture.join("tool.toml")).unwrap();
    fs::write(
        fixture.join("tool.toml"),
        manifest.replace("\"service.py\"", "\"service.py\", \"--exit-before-ready\""),
    )
    .unwrap();
    let host = host(root.path(), vec![fixture]).await;
    assert_eq!(host.start("fixture", true).await.unwrap_err(), "exited");
    let entry = host.entry("fixture").unwrap();
    assert_eq!(entry.state.lock().status, "stopped");
    assert_eq!(entry.state.lock().restarts, 0);
    assert!(!entry.state.lock().desired);
    host.shutdown().await;
}

#[tokio::test]
async fn catalog_defaults_and_customized_sources_stay_under_user_control() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), Vec::new()).await;
    let expected = json!([
        {"source":"18-lab-dev/vosu-tools","enabled":true},
        {"source":"18-lab-dev/vosulay","enabled":true}
    ]);
    let identities = |view: Value| {
        view["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|source| json!({"source":source["source"],"enabled":source["enabled"]}))
            .collect::<Vec<_>>()
    };
    assert_eq!(json!(identities(host.source_view().unwrap())), expected);
    let path = host.config.join("catalog-sources.json");
    for customized in [
        json!([]),
        json!([{"source":"18-lab-dev/vosu-tools","enabled":true}]),
        json!([
            {"source":"18-lab-dev/vosu-tools","enabled":false},
            {"source":"18-lab-dev/vosulay","enabled":true}
        ]),
        json!([
            {"source":"18-lab-dev/vosulay","enabled":true},
            {"source":"18-lab-dev/vosu-tools","enabled":true}
        ]),
        json!([
            {"source":"18-lab-dev/vosu-tools","enabled":true},
            {"source":"owner/custom","enabled":true}
        ]),
    ] {
        atomic_json(&path, &customized).unwrap();
        let before = fs::read(&path).unwrap();
        assert_eq!(json!(identities(host.source_view().unwrap())), customized);
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let removed = json!([{"source":"18-lab-dev/vosu-tools","enabled":true}]);
    let edited = host
        .save_sources(&serde_json::to_vec(&json!({"sources": removed})).unwrap())
        .await
        .unwrap();
    assert_eq!(json!(identities(edited)), removed);
    assert_eq!(json!(identities(host.source_view().unwrap())), removed);
    host.shutdown().await;
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
    let first = host(root.path(), Vec::new()).await;
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
    let migrated: Value =
        serde_json::from_slice(&fs::read(config.join("catalog-sources.json")).unwrap()).unwrap();
    assert_eq!(migrated[0], sources[0]);
    for (index, source) in [
        (1, "18-lab-dev/vosulay"),
        (2, "18-lab-dev/vosu-tools"),
        (3, "shiruviaa/vosu-overlays"),
    ] {
        assert_eq!(migrated[index]["source"], source);
        assert_eq!(migrated[index]["enabled"], sources[index]["enabled"]);
    }
    for index in 4..7 {
        assert_eq!(migrated[index], sources[index]);
    }
    let overlay: Value =
        serde_json::from_slice(&fs::read(config.join("overlay-catalog-state.json")).unwrap())
            .unwrap();
    assert_eq!(overlay["overlay"]["source"], "shiruviaa/vosu-overlays");
    first.shutdown().await;
    let paths = [
        "catalog-sources.json",
        "state.json",
        "overlay-catalog-state.json",
    ];
    let before = paths.map(|name| fs::read(config.join(name)).unwrap());
    let second = host(root.path(), Vec::new()).await;
    for (name, bytes) in paths.iter().zip(before) {
        assert_eq!(fs::read(config.join(name)).unwrap(), bytes);
    }
    second.shutdown().await;
}

#[tokio::test]
async fn former_default_lists_migrate_without_readding_explicitly_removed_sources() {
    for (include_vosulay, edited) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config");
        fs::create_dir_all(&config).unwrap();
        // Former organization identities are persisted migration fixtures.
        let mut sources = vec![json!({"source":"shiruviaa/vosu-tools","enabled":true})];
        if include_vosulay {
            sources.push(json!({"source":"shiruviaa/vosulay","enabled":true}));
        }
        sources.push(json!({"source":"shiruviaa/vosu-overlays","enabled":true}));
        atomic_json(&config.join("catalog-sources.json"), &sources).unwrap();
        if edited {
            atomic_json(&config.join("catalog-sources-v2.json"), &true).unwrap();
        }
        let current = host(root.path(), Vec::new()).await;
        let migrated: Value =
            serde_json::from_slice(&fs::read(config.join("catalog-sources.json")).unwrap())
                .unwrap();
        let mut expected = vec![json!({"source":"18-lab-dev/vosu-tools","enabled":true})];
        if !edited || include_vosulay {
            expected.push(json!({"source":"18-lab-dev/vosulay","enabled":true}));
        }
        if edited && !include_vosulay {
            expected.push(json!({"source":"shiruviaa/vosu-overlays","enabled":true}));
        }
        assert_eq!(migrated, json!(expected));
        current.shutdown().await;
    }
}

#[tokio::test]
async fn cycles_do_not_poison_dependents() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path(), Vec::new()).await;
    put(&host.data, "cycle", "[[requires]]\nid='cycle'\nversion='*'");
    put(
        &host.data,
        "consumer",
        "[[requires]]\nid='cycle'\nversion='*'",
    );
    host.rescan().await;
    host.saved
        .lock()
        .entry("consumer".into())
        .or_default()
        .trusted = true;
    assert_eq!(host.status(&host.entry("cycle").unwrap()).0, "invalid");
    assert_eq!(host.status(&host.entry("consumer").unwrap()).0, "blocked");
    host.shutdown().await;
}

#[cfg(unix)]
fn copied_fixture(root: &Path) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tool-host");
    let target = root.join("fixture");
    fs::create_dir_all(target.join("web")).unwrap();
    for name in ["tool.toml", "service.py", "web/index.html"] {
        fs::copy(source.join(name), target.join(name)).unwrap();
    }
    target
}

#[cfg(unix)]
#[tokio::test]
async fn development_script_change_restarts_with_continuous_logs() {
    let root = tempfile::tempdir().unwrap();
    let fixture = copied_fixture(root.path());
    let host = host(root.path(), vec![fixture.clone()]).await;
    host.start("fixture", true).await.unwrap();
    let first = host.entry("fixture").unwrap();
    let pid = first.state.lock().pid.unwrap();
    let script = fs::read_to_string(fixture.join("service.py")).unwrap();
    fs::write(
        fixture.join("service.py"),
        format!("{script}\n# Development rebuild.\n"),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let entry = host.entry("fixture").unwrap();
            let ready = entry.state.lock().pid.is_some_and(|next| next != pid)
                && entry.state.lock().status == "running";
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let entry = host.entry("fixture").unwrap();
    let lines = entry.logs.lock().clone();
    assert!(
        lines
            .iter()
            .filter(|line| line.text == "fixture started")
            .count()
            >= 2
    );
    assert!(
        lines
            .iter()
            .zip(lines.iter().skip(1))
            .all(|(a, b)| a.seq < b.seq)
    );
    host.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn five_pre_readiness_crashes_fail_until_manual_start() {
    let root = tempfile::tempdir().unwrap();
    let fixture = copied_fixture(root.path());
    let manifest = fs::read_to_string(fixture.join("tool.toml")).unwrap();
    fs::write(
        fixture.join("tool.toml"),
        manifest.replace("\"service.py\"", "\"service.py\", \"--crash-before-ready\""),
    )
    .unwrap();
    let host = host(root.path(), vec![fixture]).await;
    let before = Instant::now();
    assert!(host.start("fixture", false).await.is_err());
    let entry = host.entry("fixture").unwrap();
    assert_eq!(entry.state.lock().status, "failed");
    assert_eq!(entry.state.lock().restarts, 5);
    assert!(before.elapsed() >= Duration::from_secs(15));
    assert!(
        host.start("fixture", false)
            .await
            .unwrap_err()
            .contains("failed")
    );
    host.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn websocket_proxy_preserves_frames_subprotocol_and_drains() {
    use axum::{Extension, Router, extract::State};
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let root = tempfile::tempdir().unwrap();
    let fixture = copied_fixture(root.path());
    let host = host(root.path(), vec![fixture]).await;
    let tracker = Arc::new(crate::server::websocket::UpgradeCallbackTracker::new());
    let router = Router::new()
        .fallback(
            |State(host): State<Arc<ToolHost>>, request: Request| async move {
                host.overlay(request, Bytes::new()).await
            },
        )
        .layer(Extension(tracker.clone()))
        .with_state(host.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut request = format!("ws://{address}/tools/fixture/api/v1/stream")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("sec-websocket-protocol", "fixture".parse().unwrap());
    let (mut socket, handshake) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(handshake.headers()["sec-websocket-protocol"], "fixture");
    let frame = socket.next().await.unwrap().unwrap().into_text().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&frame).unwrap()["caller"],
        "overlay"
    );
    let _ = socket.close(None).await;
    tracker.cancel();
    tokio::time::timeout(Duration::from_secs(3), tracker.wait_drained())
        .await
        .unwrap();
    host.shutdown().await;
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn tool_settings_and_approval_persist_outside_overlay_settings() {
    let root = tempfile::tempdir().unwrap();
    let tool = put(&root.path().join("data"), "user-tool", "");
    fs::write(tool.join("settings.json"), r#"[{"uniqueID":"count","type":"number","value":"12"},{"uniqueID":"enabled","type":"checkbox","value":"true"}]"#).unwrap();
    let first = host(root.path(), Vec::new()).await;
    assert_eq!(
        first.status(&first.entry("user-tool").unwrap()).0,
        "untrusted"
    );
    for action in ["trust", "disable"] {
        assert_eq!(
            first
                .dashboard(
                    request(Method::POST, &format!("/api/tools/user-tool/{action}")),
                    Bytes::new()
                )
                .await
                .status(),
            StatusCode::OK
        );
    }
    let settings = value(
        first
            .dashboard(
                request(Method::GET, "/api/tools/user-tool/settings"),
                Bytes::new(),
            )
            .await,
    )
    .await;
    assert_eq!(settings["values"]["count"], 12.0);
    assert_eq!(settings["values"]["enabled"], true);
    let saved = first
        .dashboard(
            request(Method::POST, "/api/tools/user-tool/settings"),
            Bytes::from_static(br#"[{"uniqueID":"count","value":"25"}]"#),
        )
        .await;
    assert_eq!(saved.status(), StatusCode::OK);
    assert!(root.path().join("config/settings/user-tool.json").is_file());
    assert!(!tool.join("settings.schema.toml").exists());
    first.shutdown().await;
    drop(first);
    let second = host(root.path(), Vec::new()).await;
    let entry = second.entry("user-tool").unwrap();
    assert!(second.trusted(&entry));
    assert_eq!(second.status(&entry).0, "disabled");
    assert_eq!(second.settings(&entry).unwrap()["count"], 25.0);
    second.shutdown().await;
}

#[tokio::test]
async fn password_settings_never_reach_the_dashboard() {
    let root = tempfile::tempdir().unwrap();
    let tool = put(&root.path().join("dev"), "secretive", "");
    fs::write(
        tool.join("settings.json"),
        r#"[{"uniqueID":"secret","type":"password","value":"default-secret"},{"uniqueID":"name","type":"text","value":"x"}]"#,
    )
    .unwrap();
    let host = host(root.path(), vec![tool]).await;
    let read = || async {
        value(
            host.dashboard(
                request(Method::GET, "/api/tools/secretive/settings"),
                Bytes::new(),
            )
            .await,
        )
        .await
    };
    let save = |body: &'static [u8]| {
        host.dashboard(
            request(Method::POST, "/api/tools/secretive/settings"),
            Bytes::from_static(body),
        )
    };
    let effective = || host.settings(&host.entry("secretive").unwrap()).unwrap()["secret"].clone();
    let settings = read().await;
    assert_eq!(settings["values"]["secret"], "");
    assert_eq!(settings["settings"][0]["value"], "");
    assert_eq!(settings["secrets"]["secret"], true);
    assert!(!settings.to_string().contains("default-secret"));
    assert_eq!(
        save(br#"[{"uniqueID":"secret","value":"hunter2"}]"#)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(effective(), "hunter2");
    // Blank or omitted keeps the secret; null clears it.
    save(br#"[{"uniqueID":"secret","value":""},{"uniqueID":"name","value":"y"}]"#).await;
    save(br#"[{"uniqueID":"name","value":"z"}]"#).await;
    assert_eq!(effective(), "hunter2");
    assert!(!read().await.to_string().contains("hunter2"));
    // The overlay-origin read route omits password values entirely.
    let readable = value(
        host.overlay(
            request(Method::GET, "/tools/secretive/_settings"),
            Bytes::new(),
        )
        .await,
    )
    .await;
    assert_eq!(readable, json!({"values": {"name": "z"}}));
    save(br#"[{"uniqueID":"secret","value":null}]"#).await;
    assert_eq!(effective(), "");
    assert_eq!(read().await["secrets"]["secret"], false);
    host.shutdown().await;
}

#[tokio::test]
async fn cli_broker_auth_sessions_and_authority() {
    use crate::server::{broker::BrokerToken, routes::PeerAddress};
    use axum::extract::ConnectInfo;
    let root = tempfile::tempdir().unwrap();
    let a = put(
        root.path(),
        "a",
        "[cli]\ncommand=['bin/a']\nsummary='A CLI'\n[[requires]]\nid='b'\nversion='*'\n",
    );
    let b = put(root.path(), "b", "[cli]\ncommand=['bin/b']\n");
    let c = put(
        root.path(),
        "c",
        "[service]\ncommand=['bin/c']\n[api]\nread=['/v1/read']\nwrite=['/v1/write']\n",
    );
    let host = host(root.path(), vec![a, b, c]).await;
    let stranger = put(
        &root.path().join("data"),
        "untrusted",
        "[cli]\ncommand=['bin/untrusted']\n",
    );
    host.rescan().await;
    assert!(stranger.exists());
    let broker_path = root.path().join("broker.json");
    let broker = Arc::new(BrokerToken::new(broker_path.clone()).unwrap());
    broker.publish("127.0.0.1:25150".parse().unwrap()).unwrap();
    let published: Value = serde_json::from_slice(&fs::read(broker_path).unwrap()).unwrap();
    let credential = published["token"].as_str().unwrap();
    let authenticated = |method, path: &str, token: &str, peer: &str| {
        let mut req = request(method, path);
        req.extensions_mut().insert(Some(broker.clone()));
        req.extensions_mut()
            .insert(ConnectInfo(PeerAddress(peer.parse().unwrap())));
        req.headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        req
    };
    assert_eq!(
        host.overlay(request(Method::GET, "/tools/_host/v1/cli"), Bytes::new())
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for (token, peer) in [("wrong", "127.0.0.1:1000"), (credential, "192.0.2.1:1000")] {
        assert_eq!(
            host.overlay(
                authenticated(Method::GET, "/tools/_host/v1/cli", token, peer),
                Bytes::new()
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let auth = |method, path: &str| authenticated(method, path, credential, "127.0.0.1:1000");
    for (id, expected) in [
        ("absent", StatusCode::NOT_FOUND),
        ("untrusted", StatusCode::FORBIDDEN),
        ("c", StatusCode::CONFLICT),
    ] {
        assert_eq!(
            host.overlay(
                auth(Method::POST, &format!("/tools/_host/v1/cli/{id}")),
                Bytes::new()
            )
            .await
            .status(),
            expected
        );
    }
    for (disabled, expected) in [("a", "disabled"), ("b", "blocked")] {
        host.saved.lock().insert(
            disabled.into(),
            Saved {
                enabled: Some(false),
                trusted: true,
                official: false,
                catalog_source: None,
                autostart: None,
            },
        );
        let denied = host
            .overlay(auth(Method::POST, "/tools/_host/v1/cli/a"), Bytes::new())
            .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(value(denied).await["status"], expected);
        host.saved.lock().remove(disabled);
    }
    host.entry("a").unwrap().state.lock().status = "invalid".into();
    assert_eq!(
        host.overlay(auth(Method::POST, "/tools/_host/v1/cli/a"), Bytes::new())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    host.entry("a").unwrap().state.lock().status = "running".into();
    let listing = value(
        host.overlay(auth(Method::GET, "/tools/_host/v1/cli"), Bytes::new())
            .await,
    )
    .await;
    assert_eq!(listing["tools"][0]["cli"]["usage"], "vosux a");
    let launch = value(
        host.overlay(auth(Method::POST, "/tools/_host/v1/cli/a"), Bytes::new())
            .await,
    )
    .await;
    assert_eq!(launch["env"]["VOSU_TOOL_INTERFACE"], "cli");
    assert!(Path::new(launch["argv"][0].as_str().unwrap()).is_absolute());
    let token = launch["env"]["VOSU_TOOL_TOKEN"].as_str().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("x-vosu-tool-token", token.parse().unwrap());
    // Undeclared tools: reads as an optional dependency, never writes.
    for (id, read, write) in [("a", true, true), ("b", true, true), ("c", true, false)] {
        let entry = host.entry(id).unwrap();
        assert_eq!(host.caller(&entry, &headers, false).is_some(), read);
        assert_eq!(host.caller(&entry, &headers, true).is_some(), write);
    }
    let mut req = request(Method::POST, "/tools/c/api/v1/write");
    *req.headers_mut() = headers.clone();
    assert_eq!(
        host.overlay(req, Bytes::new()).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        host.caller(&host.entry("a").unwrap(), &headers, true)
            .as_deref(),
        Some("cli:a")
    );
    let other = value(
        host.overlay(auth(Method::POST, "/tools/_host/v1/cli/b"), Bytes::new())
            .await,
    )
    .await;
    headers.insert(
        "x-vosu-tool-token",
        other["env"]["VOSU_TOOL_TOKEN"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap(),
    );
    assert!(
        host.caller(&host.entry("a").unwrap(), &headers, true)
            .is_none()
    );
    assert_eq!(
        host.overlay(
            auth(
                Method::DELETE,
                &format!(
                    "/tools/_host/v1/cli/session/{}",
                    launch["session"].as_str().unwrap()
                )
            ),
            Bytes::new()
        )
        .await
        .status(),
        StatusCode::OK
    );
    headers.insert("x-vosu-tool-token", token.parse().unwrap());
    assert!(
        host.caller(&host.entry("a").unwrap(), &headers, true)
            .is_none()
    );
    assert_eq!(
        host.overlay(
            {
                let mut req = request(Method::GET, "/tools/_host/v1/self");
                req.headers_mut()
                    .insert("x-vosu-tool-token", token.parse().unwrap());
                req
            },
            Bytes::new()
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    host.cli_sessions
        .lock()
        .values_mut()
        .for_each(|s| s.expires = Instant::now() - Duration::from_secs(1));
    assert!(
        host.cli_owner(other["env"]["VOSU_TOOL_TOKEN"].as_str().unwrap())
            .is_none()
    );
    for _ in 0..64 {
        assert_eq!(
            host.overlay(auth(Method::POST, "/tools/_host/v1/cli/a"), Bytes::new())
                .await
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        host.overlay(auth(Method::POST, "/tools/_host/v1/cli/a"), Bytes::new())
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    host.shutdown().await;
    assert!(host.cli_sessions.lock().is_empty());
}

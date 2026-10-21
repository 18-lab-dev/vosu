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
    host_with_bundle(root, dev, None).await
}
async fn host_with_bundle(
    root: &Path,
    dev: Vec<PathBuf>,
    bundle: Option<bundled::Bundle>,
) -> Arc<ToolHost> {
    let host = ToolHost::new(
        root.join("config"),
        root.join("data"),
        root.join("cache"),
        ExtraRoots {
            dev,
            bundled: bundle,
        },
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

#[tokio::test]
async fn user_bundle_duplicates_are_invalid_and_cycles_do_not_poison_dependents() {
    let root = tempfile::tempdir().unwrap();
    let collision = manifest("collision", "");
    let bundle = bundled::materialize_files(
        &root.path().join("cache/bundled"),
        "test",
        &[
            ("collision/tool.toml", collision.as_bytes()),
            ("collision/web/index.html", b"tool-ui"),
        ],
    )
    .unwrap();
    let host = host_with_bundle(root.path(), Vec::new(), Some(bundle)).await;
    put(&host.data, "collision", "");
    put(&host.data, "cycle", "[[requires]]\nid='cycle'\nversion='*'");
    put(
        &host.data,
        "consumer",
        "[[requires]]\nid='cycle'\nversion='*'",
    );
    host.rescan().await;
    let copies = host
        .entries
        .lock()
        .values()
        .filter(|entry| entry.id == "collision")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(copies.len(), 2);
    assert_eq!(host.entry("collision").unwrap().source, "bundled");
    let duplicate = copies.iter().find(|entry| entry.source == "user").unwrap();
    assert_eq!(host.status(duplicate).0, "invalid");
    assert!(
        host.status(duplicate)
            .1
            .unwrap()
            .contains("duplicates bundled")
    );
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

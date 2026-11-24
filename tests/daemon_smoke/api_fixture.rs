//! Debug development-plugin composition, using real discovery and HTTP bytes.
use super::*;

#[tokio::test]
async fn debug_fixture_is_discovered_identified_and_serves_published_bytes() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    // The daemon's compile-time path names the real profile directory, which a
    // nextest archive run mounts at the same path (the test binary itself runs
    // from an extracted copy).
    let profile = Path::new(env!("CARGO_BIN_EXE_vosu"))
        .parent()
        .unwrap()
        .to_owned();
    let fixture = profile.join("examples/libapi_v1_plugin.so");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let build = tokio::time::timeout(
        Duration::from_secs(600),
        tokio::process::Command::new(cargo)
            .current_dir(checkout)
            .args([
                "build",
                "--locked",
                "-p",
                "vosu-plugin-testkit",
                "--features",
                "test-harness",
                "--example",
                "api_v1_plugin",
            ])
            .arg("--target-dir")
            .arg(profile.parent().unwrap())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .expect("fixture build deadline");
    match build {
        Ok(status) => assert!(status.success()),
        // A nextest archive run (the prepush container) has no toolchain and a
        // read-only target dir; its workspace `--examples` build produced the
        // fixture in that profile directory.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => assert!(
            fixture.is_file(),
            "no cargo to build the fixture plugin and no prebuilt {}",
            fixture.display()
        ),
        Err(error) => panic!("fixture build failed to start: {error}"),
    }
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugin");
    fs::create_dir_all(plugin.join("lib")).unwrap();
    for name in [
        "manifest.toml",
        "namespace.json",
        "args.json",
        "result.json",
        "fixture.osu",
    ] {
        fs::copy(
            checkout.join("tests/fixtures/api-v1-plugin").join(name),
            plugin.join(name),
        )
        .unwrap();
    }
    fs::copy(&fixture, plugin.join("lib/libapi_v1_plugin.so")).unwrap();
    let target_path = root.path().join("api_v1_target");
    fs::copy("/bin/sh", &target_path).unwrap();
    let target = Command::new(&target_path)
        .args(["-c", "read line"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let target_pid = target.id();
    let _target = ChildGuard(Some(target));
    let config = smoke_channel().root(root.path().join("config/vosu"));
    fs::create_dir_all(&config).unwrap();
    let port = available_port();
    fs::write(
        config.join("vosu.toml"),
        toml_config(port, &root.path().join("static")),
    )
    .unwrap();
    let log = fs::File::create_new(root.path().join("daemon.log")).unwrap();
    let daemon = Command::new(env!("CARGO_BIN_EXE_vosu"))
        .args(["--update=false", "--plugin"])
        .arg(&plugin)
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_DATA_HOME", root.path().join("data"))
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
        .env("VOSU_PROCESS_EVENTS", "poll")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let _daemon = ChildGuard(Some(daemon));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let identified = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(response) = client.get(format!("{base}/api/v1/sessions")).send().await
                && response.status().is_success()
            {
                let body: serde_json::Value = response.json().await.unwrap();
                if let Some(session) = body["sessions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|session| session["pid"] == target_pid)
                {
                    break session.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "fixture session deadline: {}",
            fs::read_to_string(root.path().join("daemon.log")).unwrap()
        )
    });
    assert_eq!(identified["plugin"]["id"], "test.api.fixture");
    assert_eq!(identified["game"], "fixture");
    assert_eq!(identified["variant"], "synthetic");
    let id = identified["session"].as_str().unwrap();
    // Identification exposes the session before its separate root declaration
    // and PublishBatch commit. Wait for publication, not merely roster presence.
    let state = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let state: serde_json::Value = client
                .get(format!("{base}/api/v1/sessions/{id}/state"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if ["file", "stream"]
                .iter()
                .all(|kind| state["test.api.fixture"]["resources"][*kind]["url"].is_string())
            {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("identified fixture must publish both resource URLs within 30s");
    for (kind, expected) in [
        (
            "file",
            include_bytes!("../fixtures/api-v1-plugin/fixture.osu").as_slice(),
        ),
        ("stream", vosu_plugin_testkit::fixture::STREAM_BYTES),
    ] {
        let url = state["test.api.fixture"]["resources"][kind]["url"]
            .as_str()
            .unwrap();
        let response = client.get(format!("{base}{url}")).send().await.unwrap();
        assert_eq!(response.status(), 200, "published {kind}");
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            expected,
            "published {kind} bytes"
        );
    }

    // API v1 and all four mounted adapter feeds share the production daemon
    // concurrently. The focused non-osu! session never renders as upstream
    // adapter data, and adapter clients leaving never disturb API v1.
    let url = state["test.api.fixture"]["resources"]["file"]["url"]
        .as_str()
        .unwrap();
    let (mut api, mut api_buffer) =
        websocket_connect(port, &format!("/api/v1/sessions/{id}/subscribe"), None).await;
    let mut feeds = Vec::new();
    for path in ["/ws", "/tokens", "/websocket/v2", "/websocket/v2/precise"] {
        feeds.push(websocket_connect(port, path, None).await);
    }
    api.write_all(&masked_frame(
        1,
        br#"["/test.api.fixture/resources/file/url"]"#,
    ))
    .await
    .unwrap();
    let (opcode, payload) = read_server_frame(&mut api, &mut api_buffer).await;
    assert_eq!(opcode, 1);
    let initial: serde_json::Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(initial[0], 0, "{initial}");
    assert_eq!(initial[2], serde_json::json!([[0, url]]));
    for path in ["/json", "/json/sc", "/json/v2", "/json/v2/precise"] {
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), 500, "{path}");
        assert_eq!(
            response.text().await.unwrap(),
            r#"{"error":"osu is not ready/running"}"#,
            "{path}"
        );
    }
    drop(feeds);
    let after: serde_json::Value = client
        .get(format!("{base}/api/v1/sessions/{id}/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["test.api.fixture"]["resources"]["file"]["url"], url);
    api.write_all(&masked_frame(9, b"api-v1-alive"))
        .await
        .unwrap();
    assert_eq!(
        read_server_frame(&mut api, &mut api_buffer).await,
        (10, b"api-v1-alive".to_vec())
    );
}

//! The test-harness adapter capture endpoint on the real daemon executable:
//! mounted only by a `test-harness` build that received a launch capability,
//! and absent from every featureless build, which is what every release build
//! is (`build.rs` rejects `test-harness` there).
use super::*;

// Literal copies, so featureless builds can prove their absence; harness
// builds check them against the endpoint's own constants.
const PATH: &str = "/__vosu_test/v1/adapter-capture";
const HEADER: &str = "x-vosu-test-capture";
const ENV: &str = "VOSU_TEST_ADAPTER_CAPTURE";
const CAPABILITY: &str = "9d4c1b0a8f7e6d5c4b3a29180f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c";
const UNKNOWN: &str = "/__vosu_test/v1/unknown-sibling?schema=v2";

struct Daemon {
    root: TempDir,
    base: String,
    client: reqwest::Client,
    _child: ChildGuard,
}
fn command(root: &Path, capability: Option<&str>) -> (Command, u16) {
    let config = smoke_channel().root(root.join("config/vosu"));
    fs::create_dir_all(&config).unwrap();
    let port = available_port();
    fs::write(
        config.join("vosu.toml"),
        toml_config(port, &root.join("static")),
    )
    .unwrap();
    let log = fs::File::create_new(root.join("daemon.log")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_vosu"));
    command
        .arg("--update=false")
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env("VOSU_PROCESS_EVENTS", "poll")
        .env_remove(ENV)
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    if let Some(capability) = capability {
        command.env(ENV, capability);
    }
    (command, port)
}
impl Daemon {
    async fn launch(capability: Option<&str>) -> Self {
        let root = TempDir::new().unwrap();
        let (mut command, port) = command(root.path(), capability);
        let child = ChildGuard(Some(command.spawn().unwrap()));
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let base = format!("http://127.0.0.1:{port}");
        let _ = wait_get(&client, &format!("{base}/json")).await;
        Self {
            root,
            base,
            client,
            _child: child,
        }
    }
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        capabilities: &[&str],
    ) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        for capability in capabilities {
            request = request.header(HEADER, *capability);
        }
        let response = bounded!(request.send()).unwrap();
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        (
            status,
            headers,
            bounded!(response.bytes()).unwrap().to_vec(),
        )
    }
}

#[tokio::test]
async fn adapter_capture_route_exists_only_with_the_harness_feature_and_a_capability() {
    for capability in [None, Some(CAPABILITY)] {
        let daemon = Daemon::launch(capability).await;
        let get = reqwest::Method::GET;
        let served = daemon
            .call(get.clone(), &format!("{PATH}?schema=v2"), &[CAPABILITY])
            .await;
        let unknown = daemon.call(get, UNKNOWN, &[CAPABILITY]).await;
        assert_eq!(unknown.0, 404);
        if cfg!(feature = "test-harness") && capability.is_some() {
            assert_eq!(served.0, 200);
        } else {
            assert_eq!(
                (served.0, served.2),
                (unknown.0, unknown.2),
                "{capability:?}"
            );
        }
        let log = fs::read(daemon.root.path().join("daemon.log")).unwrap();
        assert!(memchr::memmem::find(&log, CAPABILITY.as_bytes()).is_none());
    }
    if !cfg!(feature = "test-harness") {
        // Not even dead code ships: the featureless executable contains none
        // of the endpoint's path, header or environment names.
        let executable = fs::read(env!("CARGO_BIN_EXE_vosu")).unwrap();
        for marker in [PATH, HEADER, ENV] {
            assert!(
                memchr::memmem::find(&executable, marker.as_bytes()).is_none(),
                "featureless executable contains {marker}"
            );
        }
    }
}

#[cfg(feature = "test-harness")]
#[tokio::test]
async fn adapter_capture_endpoint_policy_on_the_real_daemon() {
    use vosu::server::adapter_capture::endpoint;
    assert_eq!(PATH, endpoint::PATH);
    assert_eq!(HEADER, endpoint::CAPABILITY_HEADER.as_str());
    assert_eq!(ENV, endpoint::CAPABILITY_ENV);
    let daemon = Daemon::launch(Some(CAPABILITY)).await;
    let get = reqwest::Method::GET;
    for (schema, route) in [
        ("v1", "/json"),
        ("sc", "/json/sc"),
        ("v2", "/json/v2"),
        ("v2-precise", "/json/v2/precise"),
    ] {
        let (status, headers, body) = daemon
            .call(
                get.clone(),
                &format!("{PATH}?schema={schema}"),
                &[CAPABILITY],
            )
            .await;
        assert_eq!(status, 200, "{schema}");
        assert_eq!(headers["content-type"], "application/json");
        assert_eq!(headers["cache-control"], "no-store");
        assert!(!headers.contains_key("access-control-allow-origin"));
        let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(envelope["format"], "vosu-adapter-capture-v1");
        assert_eq!(envelope["schema"], schema);
        assert_eq!(envelope["session"], serde_json::Value::Null);
        // No client: the dump carries the production not-ready response.
        let (route_status, _, route_body) = daemon.call(get.clone(), route, &[]).await;
        assert_eq!(envelope["http"]["kind"], "not_ready");
        assert_eq!(envelope["http"]["status"], route_status);
        assert_eq!(
            envelope["http"]["body_utf8"].as_str().unwrap().as_bytes(),
            &route_body[..]
        );
    }
    let unknown = daemon.call(get.clone(), UNKNOWN, &[CAPABILITY]).await;
    let wrong = CAPABILITY.replace('9', "8");
    for capabilities in [
        &[][..],
        &[wrong.as_str()][..],
        &[CAPABILITY, CAPABILITY][..],
    ] {
        let reply = daemon
            .call(get.clone(), &format!("{PATH}?schema=v2"), capabilities)
            .await;
        assert_eq!(
            (reply.0, &reply.2),
            (unknown.0, &unknown.2),
            "{capabilities:?}"
        );
    }
    for method in [reqwest::Method::POST, reqwest::Method::HEAD] {
        let (status, headers, _) = daemon
            .call(method.clone(), &format!("{PATH}?schema=v2"), &[CAPABILITY])
            .await;
        assert_eq!(status, 405, "{method}");
        assert_eq!(headers["allow"], "GET");
    }
    for query in ["?schema=v2&schema=v2", "?schema=v3", "?schema=v2&x=1", ""] {
        let (status, _, body) = daemon
            .call(get.clone(), &format!("{PATH}{query}"), &[CAPABILITY])
            .await;
        assert_eq!(
            (status, &body[..]),
            (400, endpoint::BAD_QUERY_BODY),
            "{query}"
        );
    }
    // The capability never reaches a log, configuration or runtime file.
    let mut pending = vec![daemon.root.path().to_owned()];
    let mut files = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                files += 1;
                let bytes = fs::read(entry.path()).unwrap();
                assert!(
                    memchr::memmem::find(&bytes, CAPABILITY.as_bytes()).is_none(),
                    "{} contains the capability",
                    entry.path().display()
                );
            }
        }
    }
    assert!(files > 1, "the daemon wrote no files");
}

#[cfg(feature = "test-harness")]
#[tokio::test]
async fn malformed_capability_fails_startup_without_echoing_it() {
    let secret = "not-a-capability-but-secret";
    let root = TempDir::new().unwrap();
    let (mut command, _) = command(root.path(), Some(secret));
    let mut child = ChildGuard(Some(command.spawn().unwrap()));
    let status =
        tokio::task::spawn_blocking(move || vosu_proptest::wait::child(child.0.as_mut().unwrap()))
            .await
            .unwrap()
            .unwrap();
    assert!(!status.success());
    let log = fs::read_to_string(root.path().join("daemon.log")).unwrap();
    assert!(
        log.contains(&format!(
            "{ENV} must be 64 lowercase hexadecimal characters"
        )),
        "{log}"
    );
    assert!(!log.contains(secret), "{log}");
}

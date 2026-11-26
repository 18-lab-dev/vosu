//! The real catalog HTTP flow into the native process launcher. The harness
//! supplies only catalog/package bytes; approval and execution are production.
use super::*;
use axum::{Router, body::Bytes, extract::State, http::StatusCode, routing::get};
use std::{
    collections::HashMap,
    io::Write as _,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use vosu_sys::platform::BuildChannel;

const ACTION: &str = "test.p0.testkit.echo";
type Files = Arc<Mutex<HashMap<String, Bytes>>>;

struct CatalogFixture {
    root: tempfile::TempDir,
    fixture: Fixture,
    files: Files,
    socket: std::net::SocketAddr,
    channel: BuildChannel,
    _server: tokio::runtime::Runtime,
}

impl CatalogFixture {
    fn new() -> Self {
        let root = super::super::support::scratch();
        let fixture = Fixture::new(&root.path().join("receipts"));
        let files = Files::default();
        let server = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let listener =
            server.block_on(async { tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap() });
        let socket = listener.local_addr().unwrap();
        let router = Router::new()
            .route("/{name}", get(serve_file))
            .with_state(Arc::clone(&files));
        server.spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let channel = BuildChannel::from_version(vosu::BUNDLE.version).unwrap();
        Self {
            root,
            fixture,
            files,
            socket,
            channel,
            _server: server,
        }
    }

    fn publish(&self, version: &str, user_namespaces: bool) -> String {
        let staged = Artifact::Testkit.stage(&self.root.path().join(version), &[]);
        self.fixture.control(&staged, "none", None, None);
        let target = vosu_core::runtime::catalog_target();
        let library = format!(
            "lib/{}",
            staged.library.file_name().unwrap().to_str().unwrap()
        );
        let host = format!("={}", vosu::BUNDLE.version);
        let provides = json!([{"id": format!("action:{ACTION}"), "version":"1.0.0"}]);
        let actions = json!([{"name":"echo","version":"1.0.0","title":"Echo",
            "args_schema":"args.json","result_schema":"result.json","session":"none","cli":true}]);
        let runtime = if user_namespaces {
            json!({"mode":"process","linux":{"user_namespaces":true}})
        } else {
            json!("process")
        };
        let manifest = toml::to_string(&json!({
            "manifest_version":1,"kind":"plugin","id":ID,"name":"Catalog testkit","version":version,
            "host":host,"abi_version":1,"runtime":runtime,"targets":{target:{"library": library}},
            "provides":provides,"actions":actions,
        }))
        .unwrap();
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [
            ("manifest.toml", manifest.into_bytes()),
            (library.as_str(), std::fs::read(&staged.library).unwrap()),
            (
                "lib/testkit-fixture.control",
                std::fs::read(staged.dir.join("lib/testkit-fixture.control")).unwrap(),
            ),
            ("args.json", b"true".to_vec()),
            ("result.json", b"true".to_vec()),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&bytes).unwrap();
        }
        let archive = archive.finish().unwrap().into_inner();
        use sha2::{Digest as _, Sha256};
        let sha =
            vosu_core::plugin::approval::Sha256Digest(Sha256::digest(&archive).into()).to_string();
        let mut plugin = json!({"id":ID,"name":"Catalog testkit","version":version,"host":host,
            "runtime":"process","provides":provides,"actions":actions,
            "packages":{target:{"url":format!("{}/plugin.zip", self.origin()),"sha256":sha,"size":archive.len()}}});
        if user_namespaces {
            plugin["linux_user_namespaces"] = json!(true);
        }
        let catalog = json!({"catalog_version":2,"name":"Native fixture","published":"2026-11-26T00:00:00Z","plugins":[plugin]});
        let mut files = self.files.lock().unwrap();
        files.insert("catalog-v2.json".into(), Bytes::from(catalog.to_string()));
        files.insert("plugin.zip".into(), Bytes::from(archive));
        sha
    }

    fn origin(&self) -> String {
        format!("https://catalog.invalid:{}", self.socket.port())
    }

    fn target(&self) -> Value {
        json!({"kind":"plugin","id":ID,"source":format!("{}/catalog-v2.json", self.origin())})
    }

    fn daemon(&self) -> Daemon {
        Daemon::start_with(
            self.root.path(),
            Path::new(env!("CARGO_BIN_EXE_vosu")),
            &[],
            |command| {
                command.env("VOSU_CATALOG_TEST_SOCKET", self.socket.to_string());
                command.env("LOCALAPPDATA", self.root.path().join("home/local"));
            },
        )
    }

    fn install_plan(&self, daemon: &Daemon) -> Value {
        request(daemon, Method::POST, "/api/catalog/refresh", None, 200);
        request(
            daemon,
            Method::POST,
            "/api/catalog/install",
            Some(&self.target()),
            200,
        )
    }

    fn enable(&self, daemon: &Daemon) {
        request(
            daemon,
            Method::POST,
            "/api/catalog/sources",
            Some(&json!({
                "sources":[{"source":self.target()["source"],"enabled":true}],
            })),
            200,
        );
    }

    fn installed_root(&self) -> PathBuf {
        let home = self.root.path().join("home");
        let data = if cfg!(windows) {
            home.join("bin")
        } else {
            home.join("data/vosu")
        };
        self.channel.root(data).join("catalog")
    }

    fn invoke(&self, daemon: &Daemon) {
        let home = self.root.path().join("home");
        let root = if cfg!(windows) {
            home.join("local/vosu")
        } else {
            home.join("runtime/vosu")
        };
        let arguments = json!({"from":"catalog integration test"});
        let job = daemon.invoke(
            ACTION,
            &arguments,
            &self.channel.root(root).join("broker.json"),
        );
        assert_eq!(job["state"], "succeeded", "{job}");
        assert_eq!(job["result"], arguments);
    }

    fn first_host(&self, daemon: &Daemon) -> u32 {
        let receipts = self.fixture.receipts("create");
        assert_eq!(receipts.len(), 1, "{receipts:?}\n{}", daemon.log());
        let receipt = &receipts[0];
        assert_ne!(
            receipt.pid,
            daemon.pid(),
            "native code ran inside the daemon"
        );
        assert_eq!(receipt.get("plugin"), ID);
        receipt.pid
    }
}

async fn serve_file(
    State(files): State<Files>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Bytes, StatusCode> {
    files
        .lock()
        .unwrap()
        .get(&name)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)
}

fn request(
    daemon: &Daemon,
    method: Method,
    path: &str,
    body: Option<&Value>,
    expected: u16,
) -> Value {
    let (status, text) = daemon.request(method, path, body, Presented::Dashboard);
    assert_eq!(status, expected, "{path} {text}\n{}", daemon.log());
    serde_json::from_str(&text).unwrap()
}

fn wait_job(daemon: &Daemon, job: &Value) {
    let path = format!("/api/catalog/jobs/{}", job["job_id"].as_str().unwrap());
    assert!(
        wait_until(BOUND, || {
            let state = request(daemon, Method::GET, &path, None, 200);
            assert_ne!(state["status"], "failed", "{state}\n{}", daemon.log());
            state["status"] == "completed"
        }),
        "{path} did not complete\n{}",
        daemon.log()
    );
}

fn confirm(daemon: &Daemon, plan: &Value) {
    let path = format!(
        "/api/catalog/plans/{}/confirm",
        plan["plan_id"].as_str().unwrap()
    );
    request(
        daemon,
        Method::POST,
        &path,
        Some(&json!({"approve":[]})),
        400,
    );
    let job = request(
        daemon,
        Method::POST,
        &path,
        Some(&json!({"approve":[ID]})),
        200,
    );
    wait_job(daemon, &job);
}

fn load_status(daemon: &Daemon) -> Value {
    let state = request(daemon, Method::GET, "/api/catalog/status", None, 200);
    state["load_reports"][0]["status"].clone()
}

#[test]
fn approval_catalog_process_runs_actions_restarts_reapproves_and_uninstalls() {
    let fixture = CatalogFixture::new();
    let sha = fixture.publish("1.0.0", false);
    let daemon = fixture.daemon();
    fixture.enable(&daemon);
    let plan = fixture.install_plan(&daemon);
    assert_eq!(plan["adds"][0]["needs_approval"], true, "{plan}");
    assert!(fixture.fixture.receipts("constructor").is_empty());
    confirm(&daemon, &plan);
    assert_eq!(load_status(&daemon), "loaded");
    let view = daemon.plugin(ID);
    assert_eq!(view["state"], "running", "{view}");
    assert_eq!(
        view["provenance"],
        json!({"kind":"installed","source":fixture.target()["source"],"version":"1.0.0","package_sha256":sha})
    );
    assert!(
        !daemon.approvals().exists(),
        "catalog wrote a second approval store"
    );
    fixture.invoke(&daemon);
    let first = fixture.first_host(&daemon);
    let tracked = Tracked::open(first, None);
    daemon.kill();
    assert!(tracked.wait_exit(BOUND).is_some());

    let daemon = fixture.daemon();
    assert_eq!(load_status(&daemon), "loaded");
    let restarted = fixture
        .fixture
        .receipts("create")
        .into_iter()
        .find(|receipt| receipt.pid != first)
        .unwrap();
    assert_ne!(restarted.pid, daemon.pid());
    assert_eq!(daemon.plugin(ID)["state"], "running");
    fixture.invoke(&daemon);

    // An update replaces a running process package, and the old generation's
    // stop token must not apply to its replacement.
    let old_generation = daemon.plugin(ID)["generation"].clone();
    fixture.publish("1.1.0", false);
    let update = fixture.install_plan(&daemon);
    assert_eq!(update["adds"][0]["version"], "1.1.0", "{update}");
    assert_eq!(update["adds"][0]["needs_approval"], true);
    let watched = Tracked::open(restarted.pid, None);
    confirm(&daemon, &update);
    assert!(watched.wait_exit(BOUND).is_some());
    assert!(
        fixture
            .fixture
            .receipts("destroy")
            .iter()
            .any(|receipt| receipt.pid == restarted.pid)
    );
    request(
        &daemon,
        Method::POST,
        &format!("/api/plugins/{ID}/stop"),
        Some(&json!({"generation":old_generation})),
        409,
    );

    // On-disk manifest bytes changed without a catalog transaction. A manual
    // start must never execute them under the old plan approval.
    let installed = fixture.installed_root().join("plugins").join(ID);
    let manifest = installed.join("manifest.toml");
    let old_prompt = json!({"manifest_sha256":sha256(&manifest),"library_sha256":sha256(&installed.join("lib").join(Artifact::Testkit.library().file_name().unwrap()))});
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, format!("{text}\n# locally changed\n")).unwrap();
    let view = daemon.act(
        Method::POST,
        ID,
        "restart",
        Some(&json!({"generation":daemon.plugin(ID)["generation"]})),
    );
    assert_eq!(view["state"], "approval_required", "{view}");
    assert_eq!(fixture.fixture.receipts("constructor").len(), 3);
    assert_eq!(load_status(&daemon), "approval_required");
    assert_ne!(
        view["approval"]["manifest_sha256"],
        old_prompt["manifest_sha256"]
    );
    assert_eq!(
        view["approval"]["library_sha256"],
        old_prompt["library_sha256"]
    );
    request(
        &daemon,
        Method::POST,
        &format!("/api/plugins/{ID}/approval"),
        Some(&old_prompt),
        409,
    );
    let approved = daemon.act(Method::POST, ID, "approval", Some(&view["approval"]));
    assert_eq!(approved["state"], "running", "{approved}");
    assert_eq!(load_status(&daemon), "loaded");
    fixture.invoke(&daemon);

    let mut target = fixture.target();
    target["remove_data"] = json!(false);
    let job = request(
        &daemon,
        Method::POST,
        "/api/catalog/uninstall",
        Some(&target),
        200,
    );
    wait_job(&daemon, &job);
    assert!(!installed.exists());
    assert!(!daemon.plugins().iter().any(|row| row["id"] == ID));
    assert_eq!(fixture.fixture.receipts("destroy").len(), 3);
}

#[cfg(target_os = "linux")]
#[test]
fn approval_catalog_process_user_namespaces_uses_relaxed_filter() {
    let fixture = CatalogFixture::new();
    fixture.publish("1.0.0", true);
    let daemon = fixture.daemon();
    fixture.enable(&daemon);
    let source = fixture.target()["source"]
        .as_str()
        .unwrap()
        .replace(':', "%3A")
        .replace('/', "%2F");
    request(
        &daemon,
        Method::POST,
        &format!("/api/catalog/sources/{source}/trust"),
        Some(&json!({"trusted":true})),
        200,
    );
    let plan = fixture.install_plan(&daemon);
    assert_eq!(plan["adds"][0]["trust"], "trusted", "{plan}");
    assert_eq!(plan["adds"][0]["needs_approval"], true);
    assert_eq!(plan["adds"][0]["linux_user_namespaces"], true);
    confirm(&daemon, &plan);
    assert_eq!(load_status(&daemon), "loaded");
    let view = daemon.plugin(ID);
    assert_eq!(view["state"], "running", "{view}");
    assert_eq!(view["linux_user_namespaces"], true);
    let receipt = fixture.fixture.receipts("security").pop().unwrap();
    assert_eq!(receipt.get("userns"), "succeeded", "{receipt:?}");
    assert_eq!(receipt.get("setns_errno"), libc::EPERM.to_string());
    assert_eq!(receipt.get("nonewprivs"), "1");
    assert_eq!(receipt.get("seccomp"), "2");
    let revoked = daemon.act(Method::DELETE, ID, "approval", None);
    assert_eq!(revoked["state"], "approval_required", "{revoked}");
    assert_eq!(load_status(&daemon), "approval_required");
    assert_eq!(fixture.fixture.receipts("destroy")[0].pid, receipt.pid);
}

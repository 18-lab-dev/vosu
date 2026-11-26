//! B0 (AF.12): job artifacts declared by a real `vosu plugin-host` child
//! (`docs/v2/plugin-abi.md` §6.13; `docs/v2/api-v1.md` Job artifacts), with
//! no game session anywhere. The testkit's `make` action
//! (`testkit_process_plugin/artifacts.rs`) declares artifacts owned by its
//! own invocation; the production API v1 router serves them only to the
//! launch session that created the job, as a multi-chunk stream lease or a
//! file below a plugin-wide root. Every lease receives `STREAM_CLOSE`
//! before the plugin's `ARTIFACT_RELEASE`, also while a download is still
//! open, and a killed child's artifacts read as released.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::{Method, StatusCode, header};
use serde_json::{Value, json};
use vosu_core::actions::{ActionDescriptor, JobOwner, JobRegistry, Principal, ToolAuthority};
use vosu_core::server::api_v1::actions::ActionAuthority;
use vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES;

use super::support::api::{Api, bounded};
use super::support::host::wait_until;
use super::support::services::{BOUND, Services, Spec, generated, kill};

const CHUNK: u64 = VOSU_STREAM_CHUNK_MAX_BYTES as u64;
/// Three chunks: every full read crosses two chunk boundaries.
const SMALL: u64 = 2 * CHUNK + 4321;
/// Larger than any loopback socket buffering, so a body that is not read
/// keeps its lease open with most of the stream unrequested.
const LARGE: u64 = 12 * CHUNK + 4321;
/// The file below the plugin-wide root: two chunks.
const FILE: u64 = CHUNK + 777;

fn bytes(range: std::ops::Range<u64>) -> Vec<u8> {
    range.map(generated).collect()
}

/// Launch sessions by token: each tab its own exact owner.
#[derive(Default)]
struct Tabs(Mutex<HashMap<String, JobOwner>>);

impl Tabs {
    fn open(&self, token: &str) -> JobOwner {
        let owner = JobOwner::for_tool("b0.native".into());
        self.0.lock().unwrap().insert(token.into(), owner.clone());
        owner
    }
    fn revoke(&self, token: &str) {
        self.0.lock().unwrap().remove(token);
    }
}
impl ToolAuthority for Tabs {
    fn authorizes(&self, owner: &JobOwner, _: &ActionDescriptor) -> bool {
        self.0.lock().unwrap().values().any(|o| o.same(owner))
    }
}
impl ActionAuthority for Tabs {
    fn authenticate_action_session(&self, token: &str) -> Option<Principal> {
        self.0
            .lock()
            .unwrap()
            .get(token)
            .map(|owner| Principal::Tool(owner.clone()))
    }
    fn authorize_action_session(&self, token: &str, _: &str) -> Option<Principal> {
        self.authenticate_action_session(token)
    }
}

/// One services instance that never attaches a session, its plugin-wide
/// root holding `replay.osr`, and the API with action routes. Tab `a`
/// owns every job; tab `b` is another launch of the same tool.
struct Run {
    // Field order is drop order: the API before the host it serves.
    api: Api,
    jobs: JobRegistry,
    tabs: Arc<Tabs>,
    owner: JobOwner,
    run: Services,
}

fn start(name: &'static str) -> Run {
    let mut run = Services::empty();
    let root = run
        .root
        .path()
        .join(format!("b0-private-root-sentinel-{name}"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("replay.osr"), bytes(0..FILE)).unwrap();
    run.add(&Spec {
        name,
        root: Some(root),
        ..Spec::default()
    });
    let tabs = Arc::new(Tabs::default());
    let owner = tabs.open("tab-a");
    tabs.open("tab-b");
    let jobs = JobRegistry::new();
    let api = Api::with_actions(
        run.host(),
        Duration::from_secs(5),
        jobs.clone(),
        tabs.clone(),
    );
    Run {
        api,
        jobs,
        tabs,
        owner,
        run,
    }
}

struct Fetched {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
    complete: bool,
}

impl Run {
    /// Runs `make` for tab `a` and returns the succeeded job.
    fn make(&self, declare: Value) -> Value {
        let path = format!("/actions/{}.make?wait=10000", self.run.plugin(0).plugin_id);
        let (status, job) = self.api.runtime.block_on(bounded(async {
            let response = self
                .api
                .client
                .post(self.api.url(&path))
                .header("x-vosu-tool-session", "tab-a")
                .json(&json!({ "arguments": { "declare": declare } }))
                .send()
                .await
                .unwrap();
            (response.status(), response.json::<Value>().await.unwrap())
        }));
        assert_eq!(status, StatusCode::OK, "{job}");
        assert_eq!(job["state"], "succeeded", "{job}");
        job
    }

    fn request(&self, method: Method, url: &str, token: Option<&str>) -> reqwest::RequestBuilder {
        let request = self.api.client.request(method, self.api.url(url));
        match token {
            Some(token) => request.header("x-vosu-tool-session", token),
            None => request,
        }
    }

    /// One whole request: status, headers and the body (or the bytes that
    /// arrived before it failed, and `false`).
    fn fetch(
        &self,
        method: Method,
        url: &str,
        token: Option<&str>,
        range: Option<&str>,
    ) -> Fetched {
        let mut request = self.request(method, url, token);
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        self.api.runtime.block_on(bounded(async {
            let response = request.send().await.unwrap();
            read(response).await
        }))
    }

    fn error(&self, url: &str, token: Option<&str>) -> (StatusCode, String) {
        let fetched = self.fetch(Method::GET, url, token, None);
        let body: Value = serde_json::from_slice(&fetched.body).unwrap();
        (
            fetched.status,
            body["error"]["code"].as_str().unwrap().to_owned(),
        )
    }

    fn receipts(&self, probe: &str) -> Vec<super::support::fixture::Receipt> {
        self.run.plugin(0).fixture.receipts(probe)
    }

    /// The stream leases the plugin opened.
    fn opened_streams(&self) -> Vec<String> {
        self.receipts("open")
            .iter()
            .map(|open| open.get("stream").to_owned())
            .filter(|stream| stream != "0")
            .collect()
    }

    /// Revokes tab `a`, as core does for a disabled or uninstalled tool:
    /// its token stops authenticating and its launch session's jobs are
    /// invalidated.
    fn revoke_owner(&self) {
        self.tabs.revoke("tab-a");
        let _enter = self.run.host.enter();
        self.jobs.cancel_owner(&self.owner);
    }
}

async fn read(mut response: reqwest::Response) -> Fetched {
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    let complete = loop {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) => break true,
            Err(_) => break false,
        }
    };
    Fetched {
        status,
        headers,
        body,
        complete,
    }
}

fn url(job: &Value, index: usize) -> String {
    job["artifacts"][index]["url"].as_str().unwrap().to_owned()
}

/// A multi-chunk stream and a plugin-wide-root file with no session, whole,
/// ranged and by `HEAD`; another tab and no credential refused; then, with
/// a third download still open, the launch session's invalidation: every
/// lease is closed before its artifact's `ARTIFACT_RELEASE`, in the child's
/// own order, and the job reads as released.
#[test]
fn artifacts_multi_chunk_download_without_a_session_closes_before_release() {
    let r = start("b0dl");
    let job = r.make(json!([
        { "name": "render.bin", "size": SMALL, "source": "stream" },
        { "name": "replay.osr", "size": FILE, "source": "file", "path": "replay.osr" },
        { "name": "held.bin", "size": LARGE, "source": "stream" },
    ]));
    // No session was ever attached: the invocation and its artifacts are
    // sessionless, owned by the tool's job.
    assert!(r.receipts("attach").is_empty());
    let make = &r.receipts("make")[0];
    assert_eq!(
        (make.get("caller_kind"), make.get("root")),
        ("Tool", "0"),
        "{make:?}"
    );
    let ids: Vec<&str> = job["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| artifact["id"].as_str().unwrap())
        .collect();
    assert_eq!(job["result"]["declared"], json!(ids), "{job}");
    assert_eq!(job["artifacts"][0]["size"], SMALL);
    assert_eq!(job["artifacts"][1]["type"], "application/octet-stream");
    assert!(!job.to_string().contains("b0-private-root-sentinel"));

    // The stream: whole, ranged across a chunk boundary and by HEAD, which
    // never opens the provider.
    let stream = url(&job, 0);
    let whole = r.fetch(Method::GET, &stream, Some("tab-a"), None);
    assert_eq!(whole.status, StatusCode::OK);
    assert!(whole.complete);
    assert!(whole.body == bytes(0..SMALL), "stream bytes");
    assert_eq!(whole.headers[header::CACHE_CONTROL], "no-store");
    assert!(
        whole.headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    let chunks = r.receipts("artifact_read");
    assert!(chunks.len() >= 3, "a multi-chunk stream: {chunks:?}");
    assert!(
        chunks
            .iter()
            .all(|read| read.get("len").parse::<u64>().unwrap() <= CHUNK)
    );
    let range = format!("bytes={}-{}", CHUNK - 3, CHUNK + 2);
    let ranged = r.fetch(Method::GET, &stream, Some("tab-a"), Some(&range));
    assert_eq!(ranged.status, StatusCode::PARTIAL_CONTENT);
    assert!(ranged.complete);
    assert_eq!(ranged.body, bytes(CHUNK - 3..CHUNK + 3));
    let opens = r.receipts("open").len();
    let head = r.fetch(Method::HEAD, &stream, Some("tab-a"), None);
    assert_eq!(head.status, StatusCode::OK);
    assert_eq!(
        head.headers[header::CONTENT_LENGTH],
        SMALL.to_string().as_str()
    );
    assert_eq!(r.receipts("open").len(), opens, "HEAD opened the provider");

    // The file below the plugin-wide root.
    let file = r.fetch(Method::GET, &url(&job, 1), Some("tab-a"), None);
    assert_eq!(file.status, StatusCode::OK);
    assert!(file.complete);
    assert!(file.body == bytes(0..FILE), "file bytes");
    assert!(!String::from_utf8_lossy(&file.body).contains("b0-private-root-sentinel"));

    // Another tab, and no credential at all.
    assert_eq!(
        r.error(&stream, Some("tab-b")),
        (StatusCode::NOT_FOUND, "unknown_job".to_owned())
    );
    assert_eq!(
        r.error(&stream, None),
        (StatusCode::UNAUTHORIZED, "unauthorized".to_owned())
    );

    // A third download left open on its first chunk.
    let held = url(&job, 2);
    let mut open = r.api.runtime.block_on(bounded(async {
        let mut response = r
            .request(Method::GET, &held, Some("tab-a"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.chunk().await.unwrap().is_some());
        response
    }));
    assert!(r.receipts("release").is_empty());
    r.revoke_owner();
    let complete = r.api.runtime.block_on(bounded(async {
        loop {
            match open.chunk().await {
                Ok(Some(_)) => {}
                Ok(None) => break true,
                Err(_) => break false,
            }
        }
    }));
    assert!(!complete, "the open download ended short, never padded");
    assert!(
        wait_until(BOUND, || r.receipts("release").len() == 3),
        "releases: {:?}",
        r.receipts("release")
    );
    let opened = r.opened_streams();
    let closed: Vec<String> = r
        .receipts("artifact_close")
        .iter()
        .map(|close| close.get("stream").to_owned())
        .collect();
    assert_eq!(
        closed.iter().collect::<HashSet<_>>(),
        opened.iter().collect::<HashSet<_>>(),
        "every stream lease closed"
    );
    assert_eq!(closed.len(), opened.len(), "each lease closed exactly once");
    // In the child's own order: each artifact's leases closed before its
    // release, and no read after a close.
    let mut streams: HashMap<String, Vec<String>> = HashMap::new();
    let mut ended = HashSet::new();
    let mut released = HashSet::new();
    for receipt in r.run.plugin(0).fixture.journal(r.run.child(0)) {
        match receipt.probe.as_str() {
            "open" if receipt.get("stream") != "0" => streams
                .entry(receipt.get("artifact").to_owned())
                .or_default()
                .push(receipt.get("stream").to_owned()),
            "artifact_close" => {
                ended.insert(receipt.get("stream").to_owned());
            }
            "artifact_read" => assert!(
                !ended.contains(receipt.get("stream")),
                "a read after its lease closed: {receipt:?}"
            ),
            "release" => {
                let artifact = receipt.get("artifact").to_owned();
                for stream in streams.get(&artifact).into_iter().flatten() {
                    assert!(
                        ended.contains(stream),
                        "{artifact} released before lease {stream} closed"
                    );
                }
                released.insert(artifact);
            }
            _ => {}
        }
    }
    assert_eq!(released, ids.iter().map(|id| (*id).to_owned()).collect());
    assert_eq!(
        r.error(&stream, Some("tab-a")),
        (StatusCode::UNAUTHORIZED, "unauthorized".to_owned()),
        "the revoked launch session reads nothing"
    );
}

/// A killed child's artifacts read as released while the job is still
/// listed, and nothing is sent to the lost instance or its successor.
#[test]
fn artifacts_child_kill_reads_as_released() {
    let r = start("b0kill");
    let job = r.make(json!([{ "name": "render.bin", "size": SMALL, "source": "stream" }]));
    let stream = url(&job, 0);
    let first = r.fetch(Method::GET, &stream, Some("tab-a"), None);
    assert_eq!(first.status, StatusCode::OK);
    assert!(first.complete && first.body == bytes(0..SMALL));
    let child = r.run.child(0);
    kill(child);
    assert!(
        wait_until(BOUND, || r
            .fetch(Method::HEAD, &stream, Some("tab-a"), None)
            .status
            == StatusCode::GONE),
        "the killed child's artifact never read as released"
    );
    assert_eq!(
        r.error(&stream, Some("tab-a")),
        (StatusCode::GONE, "released_artifact".to_owned())
    );
    let path = format!("/jobs/{}", job["id"].as_str().unwrap());
    let listed = r.api.runtime.block_on(bounded(async {
        r.request(Method::GET, &path, Some("tab-a"))
            .send()
            .await
            .unwrap()
            .status()
    }));
    assert_eq!(listed, StatusCode::OK, "the job is still listed");
    assert!(
        r.receipts("release").is_empty(),
        "a lost instance gets no release"
    );
}

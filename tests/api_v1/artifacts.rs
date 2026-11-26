//! B0 (AF.12): job-owned artifacts through the production API v1 router and
//! a real in-process ABI provider, with no game session anywhere: exact
//! ownership, single ranges, headers, expiry, revocation, release order and
//! bounds, and each of the unit's named counterexamples.
use super::{
    artifact_fixture::{self as fixture, generated},
    support::{Harness, bounded},
};
use axum::{
    body::Body,
    extract::Request,
    http::{Method, StatusCode, header},
    response::Response,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use vosu::server::api_v1::{
    actions::{ActionAuthority, ActionRoutes},
    resources::ResourceHttp,
    router::ApiV1Router,
    subscriptions::ApiV1SubscriptionHub,
};
use vosu_core::actions::{
    ActionDescriptor, InvocationPolicy, JobOwner, JobRegistry, Principal, ToolAuthority,
};
use vosu_plugin_api::{Message, VOSU_STREAM_CHUNK_MAX_BYTES};

const CHUNK: u64 = VOSU_STREAM_CHUNK_MAX_BYTES as u64;
const TWO_GIB: u64 = 2 * 1024 * 1024 * 1024;

/// Launch sessions by token: each tab its own exact owner.
#[derive(Default)]
struct Tabs(Mutex<HashMap<String, JobOwner>>);

impl Tabs {
    fn open(&self, token: &str) -> JobOwner {
        let owner = JobOwner::for_tool("b0.tool".into());
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

struct Api {
    router: ApiV1Router,
    hub: ApiV1SubscriptionHub,
    jobs: JobRegistry,
    tabs: Arc<Tabs>,
}
impl Drop for Api {
    fn drop(&mut self) {
        self.hub.shutdown();
    }
}

fn api(h: &Harness, jobs: JobRegistry) -> Api {
    let source = h.host.api_state_source();
    let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
    let tabs = Arc::new(Tabs::default());
    tabs.open("tab-a");
    tabs.open("tab-b");
    let router = ApiV1Router::new(source, hub.clone())
        .with_resources(Arc::new(ResourceHttp::new(&h.host, Duration::from_secs(2))))
        .with_actions(ActionRoutes::new(
            h.host.clone(),
            h.host
                .invocation_executor_with_policy(InvocationPolicy::default()),
            jobs.clone(),
            tabs.clone(),
        ));
    Api {
        router,
        hub,
        jobs,
        tabs,
    }
}

fn request(method: Method, path: &str, token: Option<&str>, body: Option<Value>) -> Request {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:24050");
    if let Some(token) = token {
        builder = builder.header("x-vosu-tool-session", token);
    }
    builder
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap()
}

async fn json_body(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The whole body, or the bytes before it failed and `false`.
async fn bytes(response: Response) -> (Vec<u8>, bool) {
    let mut stream = response.into_body().into_data_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(_) => return (bytes, false),
        }
    }
    (bytes, true)
}

fn expected(range: std::ops::Range<u64>) -> Vec<u8> {
    range.map(generated).collect()
}

impl Api {
    /// Runs `plugin.make` with `args` for `token` and returns the terminal job.
    async fn make(&self, plugin: &str, token: &str, args: Value) -> Value {
        let response = self
            .router
            .handle(request(
                Method::POST,
                &format!("/api/v1/actions/{plugin}.make?wait=10000"),
                Some(token),
                Some(json!({ "arguments": args })),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let job = json_body(response).await;
        assert_eq!(job["state"], "succeeded", "{job}");
        job
    }

    async fn start(&self, plugin: &str, token: &str, args: Value) -> Value {
        let response = self
            .router
            .handle(request(
                Method::POST,
                &format!("/api/v1/actions/{plugin}.make?wait=0"),
                Some(token),
                Some(json!({ "arguments": args })),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        json_body(response).await
    }

    async fn get(&self, path: &str, token: Option<&str>) -> Response {
        self.router
            .handle(request(Method::GET, path, token, None))
            .await
    }

    async fn ranged(&self, path: &str, token: &str, range: &str) -> Response {
        let mut request = request(Method::GET, path, Some(token), None);
        request
            .headers_mut()
            .insert(header::RANGE, range.parse().unwrap());
        self.router.handle(request).await
    }
}

fn spec(name: &str, key: Value, size: u64) -> Value {
    json!({ "name": name, "size": size, "key": key })
}

fn url(job: &Value, index: usize) -> String {
    job["artifacts"][index]["url"].as_str().unwrap().to_owned()
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap()
}

fn assert_protected_headers(response: &Response) {
    let headers = response.headers();
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::CONTENT_SECURITY_POLICY], "sandbox");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
}

/// Ownership, listing, headers, HEAD, single ranges, multi-chunk stream and
/// root-file downloads, and every credential that must not read them.
#[test]
fn artifact_ownership_ranges_and_headers_without_a_session() {
    let h = Harness::new();
    std::fs::write(h.root.path().join("replay.osr"), expected(0..CHUNK + 777)).unwrap();
    let events = fixture::load(&h, 980101, "b0.own", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let size = 2 * CHUNK + 4321;
        let job = api
            .make(
                "b0.own",
                "tab-a",
                json!({"declare": [
                    spec("render.mp4", json!({"source": "stream", "size": size}), size),
                    spec("replay.osr", json!({"source": "file", "path": "replay.osr"}), CHUNK + 777),
                ]}),
            )
            .await;
        assert_eq!(job["session"], Value::Null);
        // Listed by core from the declarations; the result never carries it.
        let listed = job["artifacts"].as_array().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0]["name"], "render.mp4");
        assert_eq!(listed[0]["type"], "video/mp4");
        assert_eq!(listed[0]["size"], size);
        assert_eq!(listed[1]["type"], "application/octet-stream");
        let id = listed[0]["id"].as_str().unwrap();
        assert_eq!(id.len(), 32);
        assert_eq!(
            listed[0]["url"],
            format!("/api/v1/jobs/{}/artifacts/{id}", job["id"].as_str().unwrap())
        );
        assert_eq!(
            job["result"]["declared"][0]["artifact"], listed[0]["id"],
            "the plugin got the same opaque id"
        );
        let stream_url = url(&job, 0);
        let file_url = url(&job, 1);

        // HEAD: exact headers, no body and no plugin open.
        let opens = || {
            fixture::drain(&events)
                .into_iter()
                .filter(|m| matches!(m, Message::ArtifactOpen(_)))
                .count()
        };
        assert_eq!(opens(), 0);
        let head = api
            .router
            .handle(request(Method::HEAD, &stream_url, Some("tab-a"), None))
            .await;
        assert_eq!(head.status(), StatusCode::OK);
        assert_protected_headers(&head);
        assert_eq!(head.headers()[header::CONTENT_LENGTH], size.to_string());
        assert_eq!(head.headers()[header::CONTENT_TYPE], "video/mp4");
        assert_eq!(head.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(
            head.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"render.mp4\"; filename*=UTF-8''render.mp4"
        );
        assert!(bytes(head).await.0.is_empty());
        assert_eq!(opens(), 0);

        // Multi-chunk stream: every byte, in chunks no larger than one frame.
        let full = api.get(&stream_url, Some("tab-a")).await;
        assert_eq!(full.status(), StatusCode::OK);
        assert_protected_headers(&full);
        assert_eq!(bytes(full).await, (expected(0..size), true));
        let reads: Vec<_> = fixture::drain(&events)
            .into_iter()
            .filter_map(|m| match m {
                Message::StreamRead(read) => Some((read.offset, read.len)),
                _ => None,
            })
            .collect();
        assert_eq!(
            reads,
            [(0, CHUNK as u32), (CHUNK, CHUNK as u32), (2 * CHUNK, 4321)]
        );

        // One single range, crossing a chunk boundary.
        let ranged = api
            .ranged(&stream_url, "tab-a", &format!("bytes={}-{}", CHUNK - 5, CHUNK + 9))
            .await;
        assert_eq!(ranged.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            ranged.headers()[header::CONTENT_RANGE],
            format!("bytes {}-{}/{size}", CHUNK - 5, CHUNK + 9)
        );
        assert_protected_headers(&ranged);
        assert_eq!(bytes(ranged).await, (expected(CHUNK - 5..CHUNK + 10), true));
        for invalid in [format!("bytes={size}-"), "bytes=0-1,4-5".into()] {
            let response = api.ranged(&stream_url, "tab-a", &invalid).await;
            assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(error_code(&json_body(response).await), "invalid_range");
        }

        // A file in the plugin-wide root, opened fresh per download.
        let file = api.get(&file_url, Some("tab-a")).await;
        assert_eq!(file.status(), StatusCode::OK);
        assert_eq!(bytes(file).await, (expected(0..CHUNK + 777), true));
        let tail = api.ranged(&file_url, "tab-a", "bytes=-10").await;
        assert_eq!(tail.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            bytes(tail).await.0,
            expected(CHUNK + 767..CHUNK + 777)
        );

        // Counterexample: another tab of the same tool reads nothing.
        let other = api.get(&stream_url, Some("tab-b")).await;
        assert_eq!(other.status(), StatusCode::NOT_FOUND);
        assert_protected_headers(&other);
        assert_eq!(error_code(&json_body(other).await), "unknown_job");
        // No credential, an overlay, or an API key: 401.
        assert_eq!(
            api.get(&stream_url, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let mut keyed = request(Method::GET, &stream_url, None, None);
        keyed
            .headers_mut()
            .insert(header::AUTHORIZATION, "Bearer api-key".parse().unwrap());
        assert_eq!(api.router.handle(keyed).await.status(), StatusCode::UNAUTHORIZED);
        // A cross-origin page holding the token still cannot use it.
        let mut cross = request(Method::GET, &stream_url, Some("tab-a"), None);
        cross
            .headers_mut()
            .insert(header::ORIGIN, "http://evil.example".parse().unwrap());
        assert_eq!(api.router.handle(cross).await.status(), StatusCode::FORBIDDEN);
        // Unknown ids, and an artifact id is never a resource id.
        let unknown = format!("/api/v1/jobs/{}/artifacts/{}", job["id"].as_str().unwrap(), "0".repeat(32));
        let response = api.get(&unknown, Some("tab-a")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(error_code(&json_body(response).await), "unknown_artifact");
        let as_resource = api.get(&format!("/api/v1/resources/{id}"), None).await;
        assert_eq!(as_resource.status(), StatusCode::NOT_FOUND);
    }));
}

/// Counterexample: ordinary public resources must not start requiring a
/// credential beside the protected artifact route. A session-published
/// file is read with no token, and an unrelated or stale token changes
/// nothing on it.
#[test]
fn public_resources_stay_public_beside_artifacts() {
    let h = super::resources::setup("b0.public", 980191);
    h.runtime.block_on(bounded(async {
        let state = super::resources::urls(&h, "b0.public").await;
        let api = api(&h, JobRegistry::new());
        let file = state["resources"]["file"]["url"].as_str().unwrap();
        for token in [None, Some("stale-token"), Some("tab-a")] {
            let response = api.get(file, token).await;
            assert_eq!(response.status(), StatusCode::OK, "{token:?}");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=31536000, immutable"
            );
            assert_eq!(bytes(response).await.0, super::resources::FILE);
        }
    }));
}

/// Counterexample: an action that returns a public-looking URL mints
/// nothing. Only declarations list artifacts, and neither fabricated URL
/// reads anything.
#[test]
fn fabricated_public_url_is_not_an_artifact() {
    let h = Harness::new();
    let _events = fixture::load(&h, 980111, "b0.fake", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let first = api.make("b0.fake", "tab-a", json!({})).await;
        let job_id = first["id"].as_str().unwrap();
        let fabricated = [
            format!("/api/v1/jobs/{job_id}/artifacts/{}", "a".repeat(32)),
            "/api/v1/resources/fabricated-replay".to_owned(),
        ];
        let job = api
            .make("b0.fake", "tab-a", json!({"echo": fabricated}))
            .await;
        assert_eq!(job["result"]["echo"][0], fabricated[0].as_str());
        assert!(job.get("artifacts").is_none(), "{job}");
        assert!(first.get("artifacts").is_none());
        let response = api.get(&fabricated[0], Some("tab-a")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(error_code(&json_body(response).await), "unknown_artifact");
        assert_eq!(
            api.get(&fabricated[0], None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let response = api.get(&fabricated[1], None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }));
}

/// Counterexample: a stale invocation, or one naming another caller's
/// invocation, owns nothing; a relayed `ACTION_CALL` cannot own an
/// artifact; names and the per-job count are bounded.
#[test]
fn stale_or_foreign_invocations_and_relays_cannot_declare() {
    use vosu_core::plugin::PluginHostHandler;
    use vosu_plugin_api::{ActionCall, ActionResult, ActionStatus, CborValue};
    let h = Harness::new();
    let a_events = fixture::load(&h, 980121, "b0.alpha", h.root.path().into(), "");
    let _b_events = fixture::load(&h, 980122, "b0.beta", h.root.path().into(), "");
    let c_events = fixture::load(
        &h,
        980123,
        "b0.gamma",
        h.root.path().into(),
        "[[requires]]\nid='action:b0.alpha.make'\nversion='^1.0'\n",
    );
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let held = api.start("b0.alpha", "tab-a", json!({"hold": true})).await;
        let Message::ActionInvoke(invoke) =
            fixture::next(&a_events, |m| matches!(m, Message::ActionInvoke(_)))
        else {
            unreachable!()
        };
        let one = |name: &str| spec(name, json!({"source": "stream", "size": 8}), 8);
        // Another provider naming alpha's live invocation.
        let foreign = api
            .make(
                "b0.beta",
                "tab-b",
                json!({"invocation": invoke.invocation, "declare": [one("x.bin")]}),
            )
            .await;
        assert_eq!(
            foreign["result"]["declared"],
            json!([{"error": "invocation_finished"}])
        );
        assert!(foreign.get("artifacts").is_none());
        // Alpha's own job finishes without any artifact of beta's.
        assert_eq!(
            h.host.handle(
                vosu_core::identity::PluginInstanceId::new(980121),
                Message::ActionResult(ActionResult {
                    invocation: invoke.invocation,
                    status: ActionStatus::Ok,
                    value: Some(CborValue::Map(Vec::new())),
                    code: None,
                    message: None,
                }),
                0,
            ),
            vosu_plugin_api::vosu_status::VOSU_OK
        );
        let finished = json_body(
            api.get(
                &format!("/api/v1/jobs/{}", held["id"].as_str().unwrap()),
                Some("tab-a"),
            )
            .await,
        )
        .await;
        assert_eq!(finished["state"], "succeeded");
        assert!(finished.get("artifacts").is_none(), "{finished}");
        // The same provider replaying its own finished invocation.
        let stale = api
            .make(
                "b0.alpha",
                "tab-a",
                json!({"invocation": invoke.invocation, "declare": [one("y.bin")]}),
            )
            .await;
        assert_eq!(
            stale["result"]["declared"],
            json!([{"error": "invocation_finished"}])
        );
        // Name rule and the per-job bound.
        let bounded_job = api
            .make(
                "b0.alpha",
                "tab-a",
                json!({"declare": [
                    one("../escape.bin"), one("a.bin"), one("b.bin"), one("c.bin"),
                    one("d.bin"), one("e.bin"),
                ]}),
            )
            .await;
        let declared = bounded_job["result"]["declared"].as_array().unwrap();
        assert_eq!(declared[0], json!({"error": "invalid_name"}));
        assert!(declared[1..5].iter().all(|d| d["artifact"].is_string()));
        assert_eq!(declared[5], json!({"error": "too_many_artifacts"}));
        assert_eq!(bounded_job["artifacts"].as_array().unwrap().len(), 4);
        // A relayed ACTION_CALL has no job: caller_not_supported.
        let gamma = vosu_core::identity::PluginInstanceId::new(980123);
        assert_eq!(
            h.host.handle(
                gamma,
                Message::ActionCall(ActionCall {
                    invocation: 77,
                    action: "b0.alpha.make".into(),
                    session: None,
                    args: CborValue::serialized(&json!({"declare": [one("relay.bin")]})).unwrap(),
                }),
                0,
            ),
            vosu_plugin_api::vosu_status::VOSU_OK
        );
        let Message::ActionResult(relayed) = fixture::next(
            &c_events,
            |m| matches!(m, Message::ActionResult(r) if r.invocation == 77),
        ) else {
            unreachable!()
        };
        let relayed: Value = relayed.value.unwrap().deserialized().unwrap();
        assert_eq!(
            relayed["declared"],
            json!([{"error": "caller_not_supported"}])
        );
    }));
}

/// A started download: 200, its first chunk already primed by core.
async fn started(api: &Api, path: &str, token: &str) -> Response {
    let response = api.get(path, Some(token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    response
}

/// Waits for `ArtifactRelease`, asserting every `StreamClose` came first.
fn assert_closed_then_released(events: &std::sync::mpsc::Receiver<Message>, closes: usize) {
    let mut closed = 0;
    loop {
        match fixture::next(events, |m| {
            matches!(m, Message::StreamClose(_) | Message::ArtifactRelease(_))
        }) {
            Message::StreamClose(_) => closed += 1,
            Message::ArtifactRelease(_) => {
                assert_eq!(closed, closes, "every lease closes before the release");
                return;
            }
            _ => unreachable!(),
        }
    }
}

/// Counterexample: job expiry must not leave an open download's lease (or
/// file handle) behind. The one-shot retention deadline fires with no
/// other request, wakes the held read, closes its lease, then releases.
#[test]
fn job_expiry_closes_open_downloads_then_releases() {
    let h = Harness::new();
    let events = fixture::load(&h, 980131, "b0.expiry", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::with_retention_for_tests(Duration::from_millis(1500)));
        let size = 3 * CHUNK;
        let job = api
            .make(
                "b0.expiry",
                "tab-a",
                json!({"declare": [spec("long.mp4", json!({"source": "stream", "size": size, "hold": true}), size)]}),
            )
            .await;
        let response = started(&api, &url(&job, 0), "tab-a").await;
        let mut body = response.into_body().into_data_stream();
        let first = body.next().await.unwrap().unwrap();
        assert_eq!(first.len() as u64, CHUNK);
        // The held read is in flight before anything else happens; nothing
        // else touches the registry: only the retention deadline.
        let pending = tokio::spawn(async move { body.next().await.unwrap().is_err() });
        fixture::next(&events, |m| matches!(m, Message::StreamRead(r) if r.offset == CHUNK));
        assert!(pending.await.unwrap());
        assert_closed_then_released(&events, 1);
        let response = api.get(&url(&job, 0), Some("tab-a")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(error_code(&json_body(response).await), "unknown_job");
        let job_route = api
            .get(&format!("/api/v1/jobs/{}", job["id"].as_str().unwrap()), Some("tab-a"))
            .await;
        assert_eq!(job_route.status(), StatusCode::NOT_FOUND);
    }));
}

/// Counterexample: a partial stream must not survive a revoked tool.
/// Invalidating the launch session (core's `cancel_owner`) wakes a held
/// read at once; revoking only the token ends an unheld download at its
/// next chunk.
#[test]
fn revoked_tool_ends_partial_streams() {
    let h = Harness::new();
    let events = fixture::load(&h, 980141, "b0.revoke", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let owner = api.tabs.open("tab-r");
        let size = 3 * CHUNK;
        let job = api
            .make(
                "b0.revoke",
                "tab-r",
                json!({"declare": [
                    spec("held.mp4", json!({"source": "stream", "size": size, "hold": true}), size),
                    spec("open.mp4", json!({"source": "stream", "size": size}), size),
                ]}),
            )
            .await;
        let open = api.get(&url(&job, 1), Some("tab-r")).await;
        let mut open = open.into_body().into_data_stream();
        assert_eq!(open.next().await.unwrap().unwrap().len() as u64, CHUNK);
        let held = started(&api, &url(&job, 0), "tab-r").await;
        let mut held = held.into_body().into_data_stream();
        assert_eq!(held.next().await.unwrap().unwrap().len() as u64, CHUNK);
        let pending = tokio::spawn(async move { held.next().await.unwrap().is_err() });
        fixture::next(
            &events,
            |m| matches!(m, Message::StreamRead(r) if r.offset == CHUNK && r.stream == 2),
        );
        // Revoking the token alone: the next chunk's recheck fails.
        api.tabs.revoke("tab-r");
        assert!(open.next().await.unwrap().is_err());
        // Invalidating the session releases its artifacts: the held read
        // wakes, its lease closes, then the plugin may delete the backing.
        api.jobs.cancel_owner(&owner);
        assert!(pending.await.unwrap());
        let mut closes = 0;
        let mut releases = 0;
        while releases < 2 {
            match fixture::next(&events, |m| {
                matches!(m, Message::StreamClose(_) | Message::ArtifactRelease(_))
            }) {
                Message::StreamClose(_) => closes += 1,
                Message::ArtifactRelease(_) => {
                    releases += 1;
                    // Each artifact's own lease closed before its release.
                    assert!(closes >= releases, "a release overtook its close");
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(
            api.get(&url(&job, 0), Some("tab-r")).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }));
}

/// Counterexample: a 2 GiB output is never copied into RAM. HEAD opens
/// nothing; a tail range reads exactly its bytes; a full download reads
/// only as far as the client consumed, one frame at a time.
#[test]
fn two_gib_artifact_streams_without_buffering() {
    let h = Harness::new();
    let events = fixture::load(&h, 980151, "b0.large", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let job = api
            .make(
                "b0.large",
                "tab-a",
                json!({"declare": [spec("render.webm", json!({"source": "stream", "size": TWO_GIB}), TWO_GIB)]}),
            )
            .await;
        let path = url(&job, 0);
        assert_eq!(job["artifacts"][0]["size"], TWO_GIB);
        let head = api
            .router
            .handle(request(Method::HEAD, &path, Some("tab-a"), None))
            .await;
        assert_eq!(head.headers()[header::CONTENT_LENGTH], TWO_GIB.to_string());
        let tail = api.ranged(&path, "tab-a", "bytes=-100").await;
        assert_eq!(tail.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(bytes(tail).await, (expected(TWO_GIB - 100..TWO_GIB), true));
        let reads = |events: &std::sync::mpsc::Receiver<Message>| -> Vec<(u64, u32)> {
            fixture::drain(events)
                .into_iter()
                .filter_map(|m| match m {
                    Message::StreamRead(read) => Some((read.offset, read.len)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(reads(&events), [(TWO_GIB - 100, 100)]);
        let full = api.get(&path, Some("tab-a")).await;
        assert_eq!(full.headers()[header::CONTENT_LENGTH], TWO_GIB.to_string());
        let mut body = full.into_body().into_data_stream();
        for index in 0..3 {
            let chunk = body.next().await.unwrap().unwrap();
            assert_eq!(chunk.len() as u64, CHUNK);
            assert_eq!(chunk[..], expected(index * CHUNK..(index + 1) * CHUNK)[..]);
        }
        drop(body);
        fixture::next(&events, |m| matches!(m, Message::StreamClose(_)));
        let consumed = reads(&events);
        assert!(
            consumed.len() <= 4 && consumed.iter().all(|(_, len)| u64::from(*len) <= CHUNK),
            "core read ahead of its client: {consumed:?}"
        );
    }));
}

/// Bounds and provider faults: concurrent downloads per artifact, a size
/// that differs from the declaration, a stream that ends early, and an
/// unknown source.
#[test]
fn download_bounds_and_provider_faults() {
    let h = Harness::new();
    let events = fixture::load(&h, 980161, "b0.faults", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let size = 2 * CHUNK + 10;
        let job = api
            .make(
                "b0.faults",
                "tab-a",
                json!({"declare": [
                    spec("held.bin", json!({"source": "stream", "size": size, "hold": true}), size),
                    spec("wrong.bin", json!({"source": "stream", "size": size, "advertise": size + 1}), size),
                    spec("short.bin", json!({"source": "short", "size": size}), size),
                    spec("gone.bin", json!({"source": "missing"}), size),
                ]}),
            )
            .await;
        let mut open = Vec::new();
        for _ in 0..4 {
            let response = api.get(&url(&job, 0), Some("tab-a")).await;
            assert_eq!(response.status(), StatusCode::OK);
            open.push(response);
        }
        let busy = api.get(&url(&job, 0), Some("tab-a")).await;
        assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(error_code(&json_body(busy).await), "too_many_downloads");
        drop(open);
        let freed = api.ranged(&url(&job, 0), "tab-a", "bytes=0-9").await;
        assert_eq!(freed.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(bytes(freed).await.0, expected(0..10));

        let wrong = api.get(&url(&job, 1), Some("tab-a")).await;
        assert_eq!(wrong.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(error_code(&json_body(wrong).await), "provider_failed");
        let short = api.get(&url(&job, 2), Some("tab-a")).await;
        assert_eq!(short.status(), StatusCode::OK);
        let (read, complete) = bytes(short).await;
        assert!(!complete, "an early end aborts the body");
        assert!((read.len() as u64) < size);
        assert_eq!(read[..], expected(0..read.len() as u64)[..]);
        let gone = api.get(&url(&job, 3), Some("tab-a")).await;
        assert_eq!(gone.status(), StatusCode::BAD_GATEWAY);
        let _ = fixture::drain(&events);
    }));
}

/// Plugin loss makes a listed artifact 410 without notifying the lost
/// instance; a failed job releases what it declared at once.
#[test]
fn provider_loss_and_failed_jobs_release() {
    let h = Harness::new();
    let events = fixture::load(&h, 980171, "b0.loss", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let job = api
            .make(
                "b0.loss",
                "tab-a",
                json!({"declare": [spec("x.bin", json!({"source": "stream", "size": 8}), 8)]}),
            )
            .await;
        h.host.destroy_all(Duration::from_secs(2));
        let response = api.get(&url(&job, 0), Some("tab-a")).await;
        assert_eq!(response.status(), StatusCode::GONE);
        assert_protected_headers(&response);
        assert_eq!(error_code(&json_body(response).await), "released_artifact");
        let listed = json_body(
            api.get(
                &format!("/api/v1/jobs/{}", job["id"].as_str().unwrap()),
                Some("tab-a"),
            )
            .await,
        )
        .await;
        assert_eq!(listed["artifacts"][0]["id"], job["artifacts"][0]["id"]);
        assert!(
            !fixture::drain(&events)
                .iter()
                .any(|m| matches!(m, Message::ArtifactRelease(_))),
            "nothing is sent to a lost instance"
        );
    }));
}

/// A job that fails after declaring lists nothing and releases at once.
#[test]
fn failed_job_releases_its_declarations() {
    use vosu_core::plugin::PluginHostHandler;
    use vosu_plugin_api::{ActionResult, ActionStatus, ArtifactDeclare, CborValue};
    let h = Harness::new();
    let events = fixture::load(&h, 980181, "b0.fail", h.root.path().into(), "");
    h.runtime.block_on(bounded(async {
        let api = api(&h, JobRegistry::new());
        let job = api.start("b0.fail", "tab-a", json!({"hold": true})).await;
        let Message::ActionInvoke(invoke) =
            fixture::next(&events, |m| matches!(m, Message::ActionInvoke(_)))
        else {
            unreachable!()
        };
        let instance = vosu_core::identity::PluginInstanceId::new(980181);
        assert_eq!(
            h.host.handle(
                instance,
                Message::ArtifactDeclare(ArtifactDeclare {
                    invocation: invoke.invocation,
                    key: CborValue::Text("k".into()),
                    name: "partial.bin".into(),
                    content_id: CborValue::Null,
                    size: 1,
                }),
                41,
            ),
            vosu_plugin_api::vosu_status::VOSU_OK
        );
        fixture::next(&events, |m| matches!(m, Message::ArtifactDeclared(_)));
        h.host.handle(
            instance,
            Message::ActionResult(ActionResult {
                invocation: invoke.invocation,
                status: ActionStatus::Error,
                value: None,
                code: Some("render_failed".into()),
                message: Some("failed".into()),
            }),
            0,
        );
        let Message::ArtifactRelease(release) =
            fixture::next(&events, |m| matches!(m, Message::ArtifactRelease(_)))
        else {
            unreachable!()
        };
        assert_eq!(release.key, CborValue::Text("k".into()));
        let failed = json_body(
            api.get(
                &format!("/api/v1/jobs/{}", job["id"].as_str().unwrap()),
                Some("tab-a"),
            )
            .await,
        )
        .await;
        assert_eq!(failed["state"], "failed");
        assert!(failed.get("artifacts").is_none());
    }));
}

use std::{
    future::Future,
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, State},
    http::{Method, Request, StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use vosu::{
    calculation::{
        ModsInput as CalculationModsInput, calculate_accuracy, calculate_grade, calculate_mods,
        empty_statistics, fix_decimals_to, to_legacy_hits,
    },
    counters::{
        CounterDownloader, CounterError, CounterOpener, CounterService, DownloadRequest,
        SettingInput,
    },
    model::{
        enums::{GameState, GradeEnum, Rulesets},
        states::Mod,
    },
    net::streaming::{ZipLimits, extract_zip_atomic, is_global_unicast},
    report::escape_html,
    server::{
        auth::{AuthorizationPolicy, RequestHeaders},
        catalog::decode_response,
        commands::{
            CommandAction, CommandClient, CommandProcessor, CounterProvider, Relay, RelaySink,
            SettingsProvider,
        },
        files::{
            ByteRange, MissingKind, RangeError, ResourceOutcome, content_type, parse_range,
            resolve_encoded_resource, trailing_slash_location,
        },
        pages::{PageLookup, PageService},
        routes::{
            self, CachedJson, CalculatePpProvider, CalculateResult, Endpoint, PeerAddress, PpQuery,
            ProviderFuture, RouteDelegate, RouteProviders, StateJsonProvider, StateRoute,
        },
        runtime::{
            BoundListener, HostResolver, ListenTarget, ListenerBinder, ListenerRuntime,
            RuntimeFuture,
        },
        websocket::{
            Filter, Inbound, MAX_MESSAGE_BYTES, MESSAGE_TOO_BIG, Outbound, Schema, StateProvider,
            StateSnapshot, WebSocketHub,
        },
    },
};

struct StateFixture;
impl StateJsonProvider for StateFixture {
    fn cached_json(&self, route: StateRoute) -> ProviderFuture<'_, CachedJson> {
        Box::pin(async move {
            if route == StateRoute::V1 {
                CachedJson::Ready(Bytes::from_static(br#"{"ok":true}"#))
            } else {
                CachedJson::NotReady
            }
        })
    }
}
struct CalculateFixture;
impl CalculatePpProvider for CalculateFixture {
    fn calculate_pp(&self, query: PpQuery) -> ProviderFuture<'_, CalculateResult> {
        Box::pin(async move {
            let body =
                json!({"lazer": query.lazer, "mode": query.mode, "passed": query.passed_objects});
            CalculateResult::Ready(Bytes::from(serde_json::to_vec(&body).unwrap()))
        })
    }
}
struct DelegateFixture;
impl RouteDelegate for DelegateFixture {
    fn handle(
        &self,
        endpoint: Endpoint,
        _request: Request<Body>,
        body: Bytes,
    ) -> ProviderFuture<'_, Response> {
        Box::pin(async move {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from(format!("{endpoint:?}:{}", body.len())))
                .unwrap()
        })
    }
}

fn providers() -> RouteProviders {
    RouteProviders {
        authorization: Arc::new(AuthorizationPolicy::new(
            "127.0.0.1,localhost,absolute",
            "127.0.0.1",
        )),
        state: Arc::new(StateFixture),
        calculation: Arc::new(CalculateFixture),
        delegate: Arc::new(DelegateFixture),
    }
}

async fn dispatch(method: Method, uri: &str, origin: Option<&str>, body: Body) -> Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    let mut request = request.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(PeerAddress(SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        )))));
    routes::dispatch(State(providers()), request).await
}

#[tokio::test]
async fn exact_method_routing_envelopes_cors_and_body_limit_are_observable() {
    let ready = dispatch(Method::GET, "/json", None, Body::empty()).await;
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(ready.headers()[header::CONTENT_TYPE], "application/json");
    assert_eq!(ready.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert_eq!(
        to_bytes(ready.into_body(), 1024).await.unwrap(),
        r#"{"ok":true}"#
    );

    let not_ready = dispatch(Method::GET, "/json/v2", None, Body::empty()).await;
    assert_eq!(not_ready.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        to_bytes(not_ready.into_body(), 1024).await.unwrap(),
        r#"{"error":"osu is not ready/running"}"#
    );

    for method in [Method::HEAD, Method::OPTIONS, Method::POST] {
        let response = dispatch(method.clone(), "/json", None, Body::empty()).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{method} must not be synthesized"
        );
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    }

    let calculation = dispatch(
        Method::GET,
        "/api/calculate/pp?lazer=true&mode=3&passedObjects=12&mods=%5B%7B%22acronym%22%3A%22DT%22%7D%5D",
        None,
        Body::empty(),
    ).await;
    assert_eq!(calculation.status(), 200);
    assert_eq!(
        to_bytes(calculation.into_body(), 1024).await.unwrap(),
        r#"{"lazer":true,"mode":3,"passed":12}"#
    );

    let bad_query = dispatch(
        Method::GET,
        "/api/calculate/pp?combo=-1",
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(bad_query.status(), StatusCode::BAD_REQUEST);

    let oversized = dispatch(
        Method::POST,
        "/api/settingsSave",
        None,
        Body::from(vec![0; routes::MAX_POST_BODY + 1]),
    )
    .await;
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        oversized.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "*"
    );
}

#[test]
fn authorization_matrix_requires_both_peer_and_authoritative_header() {
    let policy = AuthorizationPolicy::new("127.0.0.1,localhost,absolute,10.2.*.*", "localhost");
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mapped = IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped());
    let wildcard = IpAddr::V4(Ipv4Addr::new(10, 2, 8, 9));
    let hostile = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));

    for peer in [loopback, mapped, wildcard] {
        assert!(policy.authorize(peer, RequestHeaders::default()).allowed());
    }
    assert!(
        !policy
            .authorize(hostile, RequestHeaders::default())
            .allowed()
    );
    assert!(
        policy
            .authorize(
                loopback,
                RequestHeaders {
                    origin: Some("http://LOCALHOST:24050"),
                    referer: Some("http://evil.test")
                }
            )
            .allowed()
    );
    assert!(
        !policy
            .authorize(
                loopback,
                RequestHeaders {
                    origin: Some("http://evil.test"),
                    referer: Some("http://localhost")
                }
            )
            .allowed(),
        "Origin wins over Referer"
    );
    assert!(
        policy
            .authorize(
                loopback,
                RequestHeaders {
                    origin: None,
                    referer: Some("http://localhost/settings")
                }
            )
            .allowed()
    );
    assert!(
        policy
            .authorize(
                loopback,
                RequestHeaders {
                    origin: Some("absolute"),
                    referer: None
                }
            )
            .allowed()
    );
    assert!(
        !policy
            .authorize(
                hostile,
                RequestHeaders {
                    origin: Some("http://localhost"),
                    referer: None
                }
            )
            .allowed()
    );
}

#[tokio::test]
async fn rejected_origin_is_403_not_found_and_never_reaches_upgrade() {
    let response = dispatch(
        Method::GET,
        "/websocket/v2",
        Some("http://evil.invalid"),
        Body::empty(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "Not Found"
    );
}

#[test]
fn file_range_mime_and_path_security_matrix_is_strict() {
    assert_eq!(parse_range(None, 10).unwrap(), ByteRange::Full);
    assert_eq!(
        parse_range(Some("bytes=2-5"), 10).unwrap(),
        ByteRange::Partial {
            start: 2,
            end_inclusive: 5
        }
    );
    assert_eq!(
        parse_range(Some("bytes=-3"), 10).unwrap(),
        ByteRange::Partial {
            start: 7,
            end_inclusive: 9
        }
    );
    assert_eq!(
        parse_range(Some("bytes=7-"), 10).unwrap(),
        ByteRange::Partial {
            start: 7,
            end_inclusive: 9
        }
    );
    assert_eq!(
        parse_range(Some("bytes=0-10"), 10),
        Err(RangeError::Unsatisfiable)
    );
    assert_eq!(
        parse_range(Some("bytes=0-1,4-5"), 10),
        Err(RangeError::Multiple)
    );
    assert_eq!(
        parse_range(Some("items=0-1"), 10),
        Err(RangeError::Malformed)
    );
    assert_eq!(content_type(Path::new("x.css")), "text/css; charset=utf-8");
    assert_eq!(
        content_type(Path::new("x.woff2")),
        "font/woff2; charset=utf-8"
    );
    assert_eq!(
        content_type(Path::new("x.mp3")),
        "audio/mpeg; charset=utf-8"
    );
    assert_eq!(trailing_slash_location("/counter?x=1"), "/counter/?x=1");

    let root = TempDir::new().unwrap();
    std::fs::write(root.path().join("safe.txt"), "safe").unwrap();
    assert!(matches!(
        resolve_encoded_resource(
            Some(root.path()),
            "safe.txt",
            "/safe.txt",
            MissingKind::NotFound
        )
        .unwrap(),
        ResourceOutcome::File(_)
    ));
    for hostile in [
        "..%2Foutside",
        "%2Fetc%2Fpasswd",
        "%252e%252e%252fsecret",
        "safe%00.txt",
    ] {
        assert!(matches!(
            resolve_encoded_resource(Some(root.path()), hostile, "/x", MissingKind::NotFound)
                .unwrap(),
            ResourceOutcome::Missing(MissingKind::NotFound)
        ));
    }
}

#[test]
fn embedded_pages_and_assets_preserve_status_mime_and_no_cache_contract() {
    let pages = PageService;
    let dashboard = pages.lookup_get("/", false, None);
    assert!(matches!(dashboard, PageLookup::Dynamic(_)));
    let css = pages.asset("homepage.min.css");
    assert_eq!(css.status, 200);
    assert_eq!(css.content_type, "text/css; charset=utf-8");
    assert!(!css.body.is_empty());
    assert_eq!(pages.asset("../LICENSE").status, 404);
}

#[derive(Default)]
struct NoopOpener;
impl CounterOpener for NoopOpener {
    fn open(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Default)]
struct NoopDownloader;
impl CounterDownloader for NoopDownloader {
    fn download_and_install(
        &self,
        _request: DownloadRequest,
        destination: PathBuf,
        _cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), CounterError>> + Send + 'static>> {
        Box::pin(async move {
            std::fs::create_dir_all(destination)?;
            Ok(())
        })
    }
}

#[test]
fn counter_metadata_settings_migration_and_traversal_are_end_to_end() {
    let root = TempDir::new().unwrap();
    let static_root = root.path().join("static");
    let folder = static_root.join("counter");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("index.html"), "ok").unwrap();
    std::fs::write(folder.join("metadata.txt"), "nAmE: Fancy ## ignored\nAUTHOR: Alice\nVersion:\nResolution: 640 x 480\nAuthorLinks: https://a, https://b\n").unwrap();
    std::fs::write(folder.join("settings.json"), r#"[{"uniqueID":"title","type":"text","title":"Title","value":"default"},{"uniqueID":"enabled","type":"checkbox","title":"Enabled","value":false}]"#).unwrap();
    std::fs::write(folder.join("settings.values.json"), r#"{"title":"legacy"}"#).unwrap();
    let service = CounterService::new(
        static_root,
        root.path().join("settings"),
        Arc::new(NoopOpener),
        Arc::new(NoopDownloader),
    );

    let counters = service.discover().unwrap();
    assert_eq!(counters.len(), 1);
    assert_eq!(counters[0].name, "Fancy");
    assert_eq!(counters[0].resolution, vec![json!(640), json!(480)]);
    assert_eq!(counters[0].authorlinks, vec!["https://a", " https://b"]);
    let (_, values) = service.get_settings("counter").unwrap();
    assert_eq!(values["title"], "legacy");
    assert!(folder.join("settings.old-values.json").exists());
    service
        .settings()
        .save_user_values(
            "counter",
            &[SettingInput {
                unique_id: "en-ab_led".into(),
                value: json!("true"),
            }],
        )
        .unwrap();
    assert_eq!(
        service.get_counter_values("counter").unwrap()["enabled"],
        true
    );
    assert!(service.open("../outside").is_err());
    assert!(service.delete("counter/child").is_err());
}

#[test]
fn catalog_zip_destination_and_report_escaping_reject_hostile_inputs() {
    let overlays = decode_response(
        200,
        br#"[{"name":"A","author":"B","version":"1","downloadUrl":"https://example.test/a.zip"}]"#,
    )
    .unwrap();
    assert_eq!(overlays.len(), 1);
    assert!(decode_response(503, b"[]").is_err());
    assert!(decode_response(200, b"not-json").is_err());

    for address in [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
        IpAddr::V6("fc00::1".parse().unwrap()),
        IpAddr::V6(Ipv4Addr::LOCALHOST.to_ipv6_mapped()),
    ] {
        assert!(
            !is_global_unicast(address),
            "{address} must not be a public destination"
        );
    }
    assert!(is_global_unicast("8.8.8.8".parse().unwrap()));
    assert_eq!(
        escape_html("<script>&\"'"),
        "&lt;script&gt;&amp;&quot;&#39;"
    );

    let root = TempDir::new().unwrap();
    let archive = root.path().join("bad.zip");
    let file = std::fs::File::create(&archive).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    writer
        .start_file("../escape.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(b"escape").unwrap();
    writer.finish().unwrap();
    let destination = root.path().join("installed");
    assert!(
        extract_zip_atomic(
            &archive,
            &destination,
            ZipLimits::default(),
            &CancellationToken::new()
        )
        .is_err()
    );
    assert!(!root.path().join("escape.txt").exists());
    assert!(!destination.exists());
}

#[test]
fn enum_mod_accuracy_grade_and_hit_boundaries_are_wire_stable() {
    assert_eq!(serde_json::to_value(GameState::Play).unwrap(), 2);
    assert_eq!(
        serde_json::from_value::<Rulesets>(json!(3)).unwrap(),
        Rulesets::Mania
    );
    assert!(serde_json::from_value::<GradeEnum>(json!(99)).is_err());

    let mods = calculate_mods(CalculationModsInput::Name("HDDT".into()), true).unwrap();
    assert_eq!(mods.name, "HDDT");
    assert!(mods.rate > 1.0);
    let hd = Mod {
        acronym: "HD".into(),
        settings: None,
    };
    let mut hits = empty_statistics();
    assert_eq!(calculate_accuracy(false, &[], 0, &hits), 0.0);
    hits.great = 100;
    assert_eq!(calculate_accuracy(false, &[], 0, &hits), 100.0);
    assert_eq!(calculate_grade(false, &[hd], 0, 100.0, &hits), "XH");
    hits.miss = 1;
    assert_ne!(calculate_grade(false, &[], 0, 99.0, &hits), "X");
    let legacy = to_legacy_hits(2, &hits);
    assert_eq!(legacy.n0, 1);
    assert_eq!(
        fix_decimals_to(-0.0, 2).unwrap().to_bits(),
        0.0f64.to_bits()
    );
    assert!(fix_decimals_to(1.0, 101).is_err());
}

struct StaticCounters;
impl CounterProvider for StaticCounters {
    fn local_counters(&self) -> Result<Value, String> {
        Ok(json!([{"name":"one"}]))
    }
}
struct StaticSettings;
impl SettingsProvider for StaticSettings {
    fn get(&self, overlay: &str) -> Result<Value, String> {
        Ok(json!({"overlay": overlay}))
    }
    fn save(&self, _overlay: &str, _body: Value) -> Result<(), String> {
        Ok(())
    }
}
struct NullRelays;
impl RelaySink for NullRelays {
    fn relay(&self, _event: Relay) {}
}

#[tokio::test]
async fn command_frames_decode_target_filter_and_shutdown_without_debounce_leak() {
    let processor = CommandProcessor::new(
        Arc::new(StaticCounters),
        Arc::new(StaticSettings),
        Arc::new(NullRelays),
    );
    let client = CommandClient::from_query(7, Some("counter%20name")).unwrap();
    assert_eq!(client.overlay, "counter name");
    assert!(matches!(
        processor.process(&client, "malformed"),
        CommandAction::NoAck
    ));
    assert!(matches!(
        processor.process(&client, "getSettings:wrong"),
        CommandAction::Reply(_)
    ));
    let CommandAction::Reply(frame) = processor.process(&client, "getSettings:counter%20name")
    else {
        panic!("reply")
    };
    assert!(String::from_utf8_lossy(&frame).contains("counter name"));
    assert!(
        CommandProcessor::relay_frame(
            &client,
            &Relay {
                except: Some(7),
                command: "getSettings".into(),
                overlay: client.overlay.clone(),
                payload: None
            }
        )
        .is_none()
    );
    processor.shutdown().await;
}

struct ResolverFixture;
impl HostResolver for ResolverFixture {
    fn resolve<'a>(
        &'a self,
        target: &'a ListenTarget,
    ) -> RuntimeFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], target.port))]) })
    }
}
struct BoundFixture {
    address: SocketAddr,
}
impl BoundListener for BoundFixture {
    fn address(&self) -> SocketAddr {
        self.address
    }
    fn begin_drain(&self) {}
    fn drained(&self) -> RuntimeFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancel_survivors(&self) {}
}
struct BinderFixture;
impl ListenerBinder for BinderFixture {
    fn bind<'a>(
        &'a self,
        target: &'a ListenTarget,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, io::Result<Arc<dyn BoundListener>>> {
        Box::pin(async move {
            if target.host == "unresolvable.invalid" {
                Err(io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "fixture bind failure",
                ))
            } else {
                Ok(Arc::new(BoundFixture { address }) as Arc<dyn BoundListener>)
            }
        })
    }
}

#[tokio::test]
async fn failed_listener_rebind_keeps_effective_listener_and_shutdown_terminates_runtime() {
    let (runtime, mut state) = ListenerRuntime::with_policy(
        Arc::new(ResolverFixture),
        Arc::new(BinderFixture),
        vec![Duration::from_millis(20)].into(),
        Duration::from_millis(20),
    );
    let (desired, desired_rx) = watch::channel(ListenTarget::new("localhost", 24050));
    let shutdown = CancellationToken::new();
    let task = {
        let shutdown = shutdown.clone();
        tokio::spawn(runtime.run(desired_rx, shutdown))
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            state.changed().await.unwrap();
            if state.borrow().effective.is_some() {
                break;
            }
        }
    })
    .await
    .unwrap();
    let original = state.borrow().effective.clone().unwrap();

    desired.send_replace(ListenTarget::new("unresolvable.invalid", 24051));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            state.changed().await.unwrap();
            let snapshot = state.borrow();
            if snapshot
                .desired
                .as_ref()
                .is_some_and(|target| target.host == "unresolvable.invalid")
                && snapshot.last_error.is_some()
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(state.borrow().effective.as_ref().unwrap(), &original);
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}

struct SnapshotFixture {
    generation: AtomicU64,
    encodes: AtomicUsize,
}
impl StateSnapshot for SnapshotFixture {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.encodes.fetch_add(1, Ordering::AcqRel);
        Ok(Bytes::from(self.generation().to_string()))
    }
    fn encode_filtered(&self, _filters: &[Filter]) -> Result<Bytes, String> {
        Ok(Bytes::from_static(b"filtered"))
    }
}
struct ProviderFixture {
    snapshot: Arc<SnapshotFixture>,
    changed: watch::Sender<u64>,
}
impl StateProvider for ProviderFixture {
    fn poll_rate(&self, _schema: Schema) -> Duration {
        Duration::from_millis(10)
    }
    fn subscribe(&self, _schema: Schema) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    fn capture(&self, _schema: Schema) -> Option<Arc<dyn StateSnapshot>> {
        Some(self.snapshot.clone())
    }
}

#[tokio::test]
async fn websocket_cadence_backpressure_filter_control_and_cleanup_contract() {
    let snapshot = Arc::new(SnapshotFixture {
        generation: AtomicU64::new(1),
        encodes: AtomicUsize::new(0),
    });
    let (changed, _) = watch::channel(0);
    let provider = Arc::new(ProviderFixture {
        snapshot: snapshot.clone(),
        changed: changed.clone(),
    });
    let hub = WebSocketHub::new(Schema::V2, provider);
    let runner = {
        let hub = hub.clone();
        tokio::spawn(async move { hub.run().await })
    };
    let (mut connection, mut outbound) = hub.connect();

    let first = tokio::time::timeout(Duration::from_secs(1), outbound.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first, Outbound::Text(Bytes::from_static(b"1")));
    snapshot.generation.store(2, Ordering::Release);
    changed.send_replace(2);
    assert!(
        tokio::time::timeout(Duration::from_millis(75), outbound.recv())
            .await
            .is_err(),
        "pending writer drops state frames"
    );
    connection.write_completed();
    snapshot.generation.store(3, Ordering::Release);
    changed.send_replace(3);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.recv())
            .await
            .unwrap()
            .unwrap(),
        Outbound::Text(Bytes::from_static(b"3"))
    );
    connection.write_completed();

    connection.set_filters(vec![Filter::Field("play".into())]);
    snapshot.generation.store(4, Ordering::Release);
    changed.send_replace(4);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), outbound.recv())
            .await
            .unwrap()
            .unwrap(),
        Outbound::Text(Bytes::from_static(b"filtered"))
    );
    connection.write_completed();

    connection
        .receive(Inbound::Ping(Bytes::from_static(b"p")))
        .await;
    assert_eq!(
        outbound.recv().await,
        Some(Outbound::Pong(Bytes::from_static(b"p")))
    );
    connection
        .receive(Inbound::Text {
            data: Bytes::from(vec![b'x'; MAX_MESSAGE_BYTES]),
            finished: false,
        })
        .await;
    connection
        .receive(Inbound::Continuation {
            data: Bytes::from_static(b"x"),
            finished: true,
        })
        .await;
    assert_eq!(
        outbound.recv().await,
        Some(Outbound::Close {
            code: MESSAGE_TOO_BIG,
            reason: "Message too big"
        })
    );
    assert_eq!(hub.client_count(), 0);
    hub.shutdown();
    tokio::time::timeout(Duration::from_secs(1), runner)
        .await
        .unwrap()
        .unwrap();
}

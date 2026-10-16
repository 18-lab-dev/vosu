//! Fixed hot_paths-shaped snapshot, cached production HTTP response, loopback only.
//! cold_connection includes connect/write/full read/close; keep_alive excludes connect.
//! dispatch_only calls the production dispatch handler without transport or Axum layers.
use std::{
    hint::black_box,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{Request, StatusCode, header},
    response::Response,
};
use criterion::{Criterion, criterion_group, criterion_main};
use vosu::{
    api::{self, ApiRuntimeSettings, ManagerInstanceSnapshot, ManagerSnapshot, SnapshotManager},
    instance::InstanceSettings,
    model::{
        enums::ClientType,
        states::{HitErrorArray, KeypressButton},
    },
    server::{
        auth::AuthorizationPolicy,
        routes::{
            CalculatePpProvider, CalculateResult, Endpoint, PeerAddress, PpQuery, ProviderFuture,
            RouteDelegate, RouteProviders, dispatch,
        },
        runtime::{AxumListenerFactory, ListenTarget, TcpListenerFactory},
        service::SnapshotStateProvider,
    },
    state::{PreciseState, RegularState, StateBundleSnapshot},
};

fn snapshot() -> Arc<ManagerSnapshot> {
    let regular = RegularState {
        global: Default::default(),
        settings: Default::default(),
        beatmap: Default::default(),
        menu: Default::default(),
        bass_density: Default::default(),
        gameplay: Default::default(),
        result_screen: Default::default(),
        tourney: Default::default(),
        user: Default::default(),
        lazer_multi_spectating: Default::default(),
        ranked_play: Default::default(),
        room: Default::default(),
    };
    let precise = PreciseState {
        catcher: None,
        status: 0,
        play_time: 0.0,
        mode: 3,
        key_overlay: Arc::new(Vec::new()),
        keypresses: Arc::new(
            (1..=18)
                .map(|index| KeypressButton {
                    name: Arc::from(format!("K{index}")),
                    is_pressed: index % 2 == 0,
                    count: index,
                    pressed_at: (index % 2 == 0).then_some(1_000.0),
                    released_at: None,
                })
                .collect(),
        ),
        hit_errors: Arc::new(HitErrorArray::default()),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0,
    };
    let instance = ManagerInstanceSnapshot::new(
        7,
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        false,
        Arc::new(StateBundleSnapshot {
            source: ClientType::Stable,
            generation: 1,
            reset_epoch: 0,
            regular: Arc::new(regular),
            precise: Arc::new(precise),
        }),
    );
    Arc::new(ManagerSnapshot::new(
        1,
        Some(7),
        true,
        ApiRuntimeSettings::default(),
        [instance],
    ))
}

// These routes are not exercised: fail loudly instead of substituting implementations
// for any measured production boundary.
struct UnusedRoutes;
impl CalculatePpProvider for UnusedRoutes {
    fn calculate_pp(&self, _: PpQuery) -> ProviderFuture<'_, CalculateResult> {
        panic!("GET /json/v2 must not call the calculation provider")
    }
}
impl RouteDelegate for UnusedRoutes {
    fn handle(&self, _: Endpoint, _: Request<Body>, _: Bytes) -> ProviderFuture<'_, Response> {
        panic!("GET /json/v2 must not call the route delegate")
    }
}

const KEEP_ALIVE: &[u8] =
    b"GET /json/v2 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n";
const CLOSE: &[u8] = b"GET /json/v2 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE: usize = 1024 * 1024;

fn connect(address: SocketAddr) -> BufReader<TcpStream> {
    let stream = TcpStream::connect_timeout(&address, IO_TIMEOUT).expect("loopback connect");
    stream.set_nodelay(true).unwrap();
    stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
    BufReader::new(stream)
}

#[derive(Default)]
struct ResponseBuffer {
    headers: Vec<u8>,
    body: Vec<u8>,
}

impl ResponseBuffer {
    fn request(&mut self, connection: &mut BufReader<TcpStream>, request: &[u8]) {
        connection.get_mut().write_all(request).expect("write GET");
        self.headers.clear();
        loop {
            let start = self.headers.len();
            let count = connection
                .read_until(b'\n', &mut self.headers)
                .expect("read header");
            assert!(
                count != 0 && self.headers.len() <= MAX_RESPONSE,
                "bounded complete headers"
            );
            if &self.headers[start..] == b"\r\n" {
                break;
            }
        }
        let headers = std::str::from_utf8(&self.headers).expect("ASCII HTTP headers");
        let length = headers
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .expect("production cached body has Content-Length")
            .1
            .trim()
            .parse::<usize>()
            .expect("valid Content-Length");
        assert!(length <= MAX_RESPONSE, "bounded response body");
        self.body.resize(length, 0);
        connection
            .read_exact(&mut self.body)
            .expect("read complete body");
    }

    fn validate(&self, expected: &[u8]) {
        let headers = std::str::from_utf8(&self.headers).unwrap();
        assert_eq!(headers.split("\r\n").next(), Some("HTTP/1.1 200 OK"));
        let content_type = headers
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .unwrap()
            .1
            .trim();
        assert_eq!(content_type, "application/json");
        assert_eq!(self.body, expected);
    }
}

async fn dispatch_request(providers: &RouteProviders, address: SocketAddr) -> Response {
    let mut request = Request::builder()
        .uri("/json/v2")
        .header(header::HOST, "127.0.0.1")
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(PeerAddress(address));
    dispatch(State(providers.clone()), request).await
}

fn bench(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let snapshots = Arc::new(SnapshotManager::new(Arc::new(ManagerSnapshot::not_ready(
        0,
        ApiRuntimeSettings::default(),
    ))));
    let published = snapshots.publish(snapshot());
    let expected = api::v2::encode(&published).expect("ready v2 fixture");
    let providers = RouteProviders {
        authorization: Arc::new(AuthorizationPolicy::new("", "127.0.0.1")),
        state: Arc::new(SnapshotStateProvider::new(snapshots)),
        calculation: Arc::new(UnusedRoutes),
        delegate: Arc::new(UnusedRoutes),
    };
    let listener = runtime.block_on(async {
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        AxumListenerFactory::new(providers.clone())
            .start(ListenTarget::new("127.0.0.1", 0), tcp)
            .unwrap()
    });
    let address = listener.address();
    assert!(address.ip().is_loopback());

    let mut buffer = ResponseBuffer::default();
    {
        let mut cold = connect(address);
        buffer.request(&mut cold, CLOSE);
        buffer.validate(&expected);
        let mut eof = [0];
        assert_eq!(
            cold.read(&mut eof).unwrap(),
            0,
            "server closes cold connection"
        );
    }
    let mut persistent = connect(address);
    // Validate two responses on the same connection to establish correct framing/reuse.
    for _ in 0..2 {
        buffer.request(&mut persistent, KEEP_ALIVE);
        buffer.validate(&expected);
    }
    runtime.block_on(async {
        let response = dispatch_request(&providers, address).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(
            to_bytes(response.into_body(), MAX_RESPONSE).await.unwrap(),
            expected
        );
    });

    let mut group = c.benchmark_group("http_v2");
    group.bench_function("cold_connection", |b| {
        b.iter(|| {
            let mut connection = connect(address);
            buffer.request(&mut connection, CLOSE);
            black_box(&buffer.body);
            // The connection is closed by Drop before the iteration ends.
        });
    });
    group.bench_function("keep_alive", |b| {
        b.iter(|| {
            buffer.request(&mut persistent, KEEP_ALIVE);
            black_box(&buffer.body);
        });
    });
    group.bench_function("dispatch_only", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let response = dispatch_request(&providers, address).await;
                black_box(to_bytes(response.into_body(), MAX_RESPONSE).await.unwrap());
            });
        });
    });
    group.finish();

    drop(persistent);
    listener.begin_drain();
    runtime.block_on(async {
        tokio::time::timeout(IO_TIMEOUT, listener.drained())
            .await
            .expect("production listener and connections drain cleanly");
    });
    drop(listener);
    runtime.shutdown_timeout(IO_TIMEOUT);
}

criterion_group!(benches, bench);
criterion_main!(benches);

//! M7 D17: production listeners, bundled hosts, bus, and API v1 transport.
//! Only the input publisher and action provider are fixtures.
#[path = "m7/daemon.rs"]
mod daemon;
#[path = "m7/tool.rs"]
mod tool;

use criterion::Criterion;
use daemon::TestDaemon;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{
    hint::black_box,
    io,
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite};
use vosu_core::{
    plugin::{self, BundledPlugin, PluginHost, PluginInstanceId, ProcessRecord, ProcessSource},
    state::SessionKey,
};
use vosu_plugin_api::{
    HostServices, PublishBatchWriter, PublishValueRef, vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::publisher::{self, LiveHost, PublisherEvent};

pub const WAIT: Duration = Duration::from_secs(15);
const GAME: &str = "test.m7.capture";
const PID: u32 = 750_791;
const SCORES: [i64; 2] = [12_345, 23_456];
pub const SUBSCRIBE: &str = "[\"/vosu.play/score\"]";
const CAPTURE_MANIFEST: &str = r#"
kind = 'plugin'
manifest_version = 1
id = 'test.m7.capture'
name = 'M7 capture fixture'
version = '1.0.0'
host = '>=2.0.0-alpha.1, <3.0.0'
abi_version = 1
runtime = 'in-process'
concurrent_dispatch = true
covers = ['field:vosu.status.status', 'field:vosu.play.score']
[match]
process_names = ['test.m7.capture']
[[provides]]
id = 'game:testkit'
version = '*'
"#;

struct Processes(bool);
impl ProcessSource for Processes {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        Ok(if self.0 { vec![process()] } else { vec![] })
    }
    fn is_alive(&self, pid: u32) -> bool {
        self.0 && pid == PID
    }
    fn events_healthy(&self) -> bool {
        true
    }
}

fn process() -> ProcessRecord {
    ProcessRecord {
        pid: PID,
        name: GAME.into(),
        path: GAME.into(),
        command_line: GAME.into(),
        argv: Some(vec![GAME.into()]),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 7913,
    }
}

pub struct Hosts {
    pub daemon: TestDaemon,
    pub overlay: String,
    pub dashboard: String,
}
impl Hosts {
    pub async fn start(root: &Path, capture: bool, tool: bool) -> Self {
        let mut daemon = daemon::new(root).await;
        let dashboard = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        daemon.config().update(|config| {
            config.server_ip = "127.0.0.1".into();
            config.dashboard_ip = "127.0.0.1".into();
            config.server_port = 0.0;
            config.dashboard_port = f64::from(dashboard.local_addr().unwrap().port());
        });
        if tool {
            daemon.add_development_tool(tool::package(root));
            load(daemon.plugins(), tool::provider(), 100);
        }
        drop(dashboard);
        daemon.compose(Arc::new(Processes(capture))).await.unwrap();
        let (overlay, dashboard) = daemon.listener_addresses().await;
        Self {
            daemon,
            overlay: format!("http://{overlay}"),
            dashboard: format!("http://{dashboard}"),
        }
    }

    pub async fn shutdown(&self) {
        self.daemon.shutdown().await.unwrap();
    }
}

fn load(host: &Arc<PluginHost>, entry: BundledPlugin<'_>, id: u64) {
    let loaded = plugin::load_bundled(
        &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
        &host.validation_snapshot(),
        &[entry],
    )
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(id),
        &loaded[0],
        std::path::PathBuf::new(),
        loaded[0]
            .manifest()
            .concurrent_dispatch
            .then(|| plugin::WorkerPool::new(1, 64)),
    )
    .unwrap();
}

pub struct Capture {
    pub key: SessionKey,
    host: LiveHost,
    batch: PublishBatchWriter,
    bytes: Vec<u8>,
    count: usize,
    // Keeps the publisher's bounded event observation alive.
    _events: mpsc::Receiver<PublisherEvent>,
}
impl Capture {
    pub fn attach(host: &Arc<PluginHost>) -> Self {
        let events = publisher::register(GAME);
        load(
            host,
            BundledPlugin {
                manifest: CAPTURE_MANIFEST,
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[],
                vtable: publisher::vosu_plugin_vtable,
            },
            101,
        );
        host.process_appeared(process());
        let deadline = std::time::Instant::now() + WAIT;
        let key = loop {
            if let PublisherEvent::Attached { handle } = events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("the composed host attaches the fixture capture")
            {
                break SessionKey(handle);
            }
        };
        let mut source = Self {
            key,
            host: publisher::host_services(GAME),
            batch: PublishBatchWriter::new(),
            bytes: Vec::new(),
            count: 0,
            _events: events,
        };
        source.publish();
        source
    }

    pub fn publish(&mut self) -> i64 {
        let score = SCORES[self.count % SCORES.len()];
        self.batch.begin(self.key.0);
        self.batch
            .push("vosu.play", "/score", PublishValueRef::I64(score));
        if self.count == 0 {
            self.batch
                .push("vosu.status", "/status", PublishValueRef::String("play"));
        }
        self.batch.finish_into(&mut self.bytes).unwrap();
        assert_eq!(
            self.host
                .send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &self.bytes),
            vosu_status::VOSU_OK,
        );
        self.count += 1;
        score
    }

    pub fn revision(&self, host: &PluginHost) -> u64 {
        host.state().store.current(self.key).unwrap().revision
    }
}

pub type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub async fn connect(hosts: &Hosts, fields: &str) -> Socket {
    use tungstenite::client::IntoClientRequest as _;
    let mut request = format!("ws{}/api/v1/sessions/active/subscribe", &hosts.overlay[4..])
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", hosts.dashboard.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(tungstenite::Message::Text(fields.into()))
        .await
        .unwrap();
    socket
}

pub async fn frame(socket: &mut Socket) -> Value {
    let text = socket
        .next()
        .await
        .expect("subscription still open")
        .unwrap()
        .into_text()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

pub async fn update(socket: &mut Socket, source: &mut Capture) {
    let score = source.publish();
    let value = frame(socket).await;
    assert_eq!(value[0], 1, "a delta, not a reset or error");
    let changes = value[2].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0][0], 0);
    assert_eq!(changes[0][1], score);
    assert!(value[3].as_array().unwrap().is_empty());
    black_box(value);
}

pub async fn bounded<T>(operation: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, operation)
        .await
        .expect("bounded M7 fixture operation")
}

pub fn bench(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut end = c.benchmark_group("end_to_end");
    end.bench_function("m7_idle_hosts", |b| {
        let root = tempfile::tempdir().unwrap();
        let hosts = runtime.block_on(bounded(Hosts::start(root.path(), true, false)));
        let host = hosts.daemon.plugins();
        let mut source = Capture::attach(host);
        let capture_count = source.count;
        let revision = source.revision(host);
        b.iter(|| black_box(source.publish()));
        assert!(source.count > capture_count, "no capture iterations ran");
        assert_eq!(
            source.revision(host) - revision,
            (source.count - capture_count) as u64
        );
        runtime.block_on(hosts.shutdown());
    });
    end.bench_function("m7_dashboard_subscription", |b| {
        let root = tempfile::tempdir().unwrap();
        let hosts = runtime.block_on(bounded(Hosts::start(root.path(), true, false)));
        let mut source = Capture::attach(hosts.daemon.plugins());
        let mut socket = runtime.block_on(bounded(async {
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            client
                .get(format!("{}/api/dashboard/status", hosts.dashboard))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .bytes()
                .await
                .unwrap();
            let sessions: Value = client
                .get(format!("{}/api/v1/sessions", hosts.overlay))
                .header(reqwest::header::ORIGIN, &hosts.dashboard)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(sessions["sessions"][0]["pid"], PID);
            let mut socket = connect(&hosts, SUBSCRIBE).await;
            assert_eq!(
                frame(&mut socket).await[2],
                serde_json::json!([[0, SCORES[0]]])
            );
            socket
        }));
        let capture_count = source.count;
        b.iter(|| runtime.block_on(bounded(update(&mut socket, &mut source))));
        assert!(source.count > capture_count, "no subscription deltas ran");
        drop(socket);
        runtime.block_on(hosts.shutdown());
    });
    end.bench_function("m7_tool_action_progress", |b| {
        let root = tempfile::tempdir().unwrap();
        let (hosts, mut client) = runtime.block_on(bounded(async {
            let hosts = Hosts::start(root.path(), false, true).await;
            let client = tool::Client::launch(&hosts).await;
            (hosts, client)
        }));
        b.iter(|| runtime.block_on(bounded(client.action())));
        assert!(client.jobs > 0, "no jobs ran");
        drop(client);
        runtime.block_on(hosts.shutdown());
    });
}

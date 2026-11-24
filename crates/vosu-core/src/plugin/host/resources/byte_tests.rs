use super::*;
use crate::plugin::host::tests::{test_host, test_process};
use crate::plugin::host_services::PluginHostHandler;
use crate::plugin::registry::{BundledPlugin, load_bundled};
use std::collections::VecDeque;
use std::sync::{
    LazyLock,
    atomic::AtomicUsize,
    mpsc::{Receiver, SyncSender, sync_channel},
};
use vosu_plugin_api::{
    CborValue, HostServices, Init, Plugin, PluginError, ResourceResolved, StreamChunk,
    VtableHostServices,
};

pub(super) struct Fixture {
    pub resolves: AtomicUsize,
    pub next: std::sync::atomic::AtomicU64,
    pub path: Mutex<Option<String>>,
    pub chunks: Mutex<VecDeque<StreamChunk>>,
    pub trees: Mutex<VecDeque<ResourceResolved>>,
    pub attached: SyncSender<u64>,
    pub closed: SyncSender<u64>,
    pub reads: SyncSender<(u64, u64, u32)>,
}
static FIXTURES: LazyLock<Mutex<HashMap<String, Arc<Fixture>>>> = LazyLock::new(Default::default);
struct BytePlugin {
    host: VtableHostServices,
    fixture: Arc<Fixture>,
}
impl Plugin for BytePlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let fixture = FIXTURES.lock().unwrap().remove(&init.plugin_id).unwrap();
        Ok(Self { host, fixture })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        let response = match message {
            Message::SessionAttached(a) => {
                self.fixture.attached.try_send(a.handle).unwrap();
                None
            }
            Message::ResourceResolve(_) => {
                self.fixture.resolves.fetch_add(1, Ordering::SeqCst);
                let path = self.fixture.path.lock().unwrap().clone();
                Some(Message::ResourceResolved(match path {
                    Some(path) => ResourceResolved::Path {
                        root: "songs".into(),
                        path,
                    },
                    None => ResourceResolved::Stream {
                        stream: self.fixture.next.fetch_add(1, Ordering::SeqCst),
                        size: 17,
                    },
                }))
            }
            Message::TreeList(_) | Message::TreeLookup(_) => {
                self.fixture.resolves.fetch_add(1, Ordering::SeqCst);
                self.fixture
                    .trees
                    .lock()
                    .unwrap()
                    .pop_front()
                    .map(Message::ResourceResolved)
            }
            Message::StreamRead(r) => {
                self.fixture
                    .reads
                    .try_send((r.stream, r.offset, r.len))
                    .unwrap();
                self.fixture
                    .chunks
                    .lock()
                    .unwrap()
                    .pop_front()
                    .map(Message::StreamChunk)
            }
            Message::StreamClose(c) => {
                self.fixture.closed.try_send(c.stream).unwrap();
                None
            }
            _ => None,
        };
        if let Some(response) = response {
            assert!(matches!(
                self.host.send(&response, correlation),
                vosu_status::VOSU_OK
                    | vosu_status::VOSU_ERR_INVALID_ARGUMENT
                    | vosu_status::VOSU_ERR_CANCELLED
            ));
        }
        Ok(())
    }
}
vosu_plugin_api::export_plugin!(bundled BytePlugin);

/// Even blocking-worker-backed operations have a bounded test-side wait.
pub(super) struct BoundedRuntime(tokio::runtime::Runtime);
impl BoundedRuntime {
    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.0.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), future)
                .await
                .expect("byte operation exceeded its bounded test deadline")
        })
    }
}

pub(super) struct BytesTest {
    pub runtime: BoundedRuntime,
    pub host: Arc<PluginHost>,
    pub instance: PluginInstanceId,
    pub owner: ResourceOwner,
    pub fixture: Arc<Fixture>,
    pub closed: Receiver<u64>,
    pub reads: Receiver<(u64, u64, u32)>,
    _logs: tempfile::TempDir,
    pub dir: tempfile::TempDir,
}
impl BytesTest {
    pub fn new(suffix: &str, pid: u32) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let _enter = runtime.enter();
        let (logs, host) = test_host(runtime.handle().clone());
        let plugin = format!("testkit.bytes.{suffix}");
        let executable = format!("byte-{pid}.exe");
        let manifest = format!(
            "kind='plugin'\nmanifest_version=1\nid='{plugin}'\nname='bytes'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nroot_slots=['songs']\n[match]\nprocess_names=['{executable}']\n[[provides]]\nid='game:fixture'\nversion='*'\n"
        );
        let (attached, attachments) = sync_channel(1);
        let (closed, close_events) = sync_channel(128);
        let (reads, read_events) = sync_channel(128);
        let fixture = Arc::new(Fixture {
            resolves: AtomicUsize::new(0),
            next: std::sync::atomic::AtomicU64::new(101),
            path: Mutex::new(None),
            chunks: Mutex::new(VecDeque::new()),
            trees: Mutex::new(VecDeque::new()),
            attached,
            closed,
            reads,
        });
        FIXTURES
            .lock()
            .unwrap()
            .insert(plugin, Arc::clone(&fixture));
        let loaded = load_bundled(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &crate::plugin::ValidationSnapshot::default(),
            &[BundledPlugin {
                manifest: &manifest,
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[],
                vtable: vosu_plugin_vtable,
            }],
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let instance = PluginInstanceId(u64::from(pid));
        host.create_instance(instance, &loaded[0], dir.path().to_owned(), None)
            .unwrap();
        host.process_appeared(test_process(pid, &executable));
        let handle = attachments.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            host.handle(
                instance,
                Message::RootDeclare(RootDeclare {
                    handle: None,
                    name: "songs".into(),
                    path: dir.path().to_str().unwrap().into()
                }),
                0
            ),
            vosu_status::VOSU_OK
        );
        let owner = host.resource_owner(instance, handle).unwrap();
        drop(_enter);
        Self {
            runtime: BoundedRuntime(runtime),
            host,
            instance,
            owner,
            fixture,
            closed: close_events,
            reads: read_events,
            _logs: logs,
            dir,
        }
    }
    pub async fn lease(&self) -> crate::plugin::StreamLease {
        let reply = self
            .host
            .resource_requests(Duration::from_millis(100))
            .request(
                self.owner,
                ResourceRequest::Resolve(vosu_plugin_api::ResourceResolve {
                    handle: self.owner.handle,
                    kind: "file".into(),
                    key: CborValue::Integer(83.into()),
                }),
            )
            .await
            .unwrap();
        let ResourceReply::Stream { lease, .. } = reply else {
            panic!("expected stream");
        };
        lease
    }
    pub fn chunk(&self, bytes: &[u8]) {
        self.fixture
            .chunks
            .lock()
            .unwrap()
            .push_back(StreamChunk::Data {
                data: vosu_plugin_api::CborBytes(bytes.to_vec()),
            });
    }
}
impl Drop for BytesTest {
    fn drop(&mut self) {
        self.host.destroy_all(Duration::from_secs(3));
    }
}

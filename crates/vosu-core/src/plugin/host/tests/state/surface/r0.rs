use super::*;
use std::sync::{
    LazyLock, Mutex,
    mpsc::{Receiver, SyncSender, sync_channel},
};
use vosu_plugin_api::{
    Init, Message, Plugin, PluginError, ResourceRef, ResourceResolved, RootDeclare,
    SessionIdentify, VtableHostServices,
};
use vosu_surface::files::{FileError, RelativeChild};
use vosu_surface::requester::BundledRequester;

struct Fixture {
    host: Mutex<Option<VtableHostServices>>,
    attached: SyncSender<u64>,
    closed: SyncSender<u64>,
    stream: bool,
    stream_size: std::sync::atomic::AtomicU64,
    variant: &'static str,
    hold_reads: std::sync::atomic::AtomicBool,
}
static FIXTURES: LazyLock<Mutex<std::collections::HashMap<String, Arc<Fixture>>>> =
    LazyLock::new(Default::default);
struct Provider {
    host: VtableHostServices,
    fixture: Arc<Fixture>,
}
impl Plugin for Provider {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let fixture = FIXTURES.lock().unwrap().remove(&init.plugin_id).unwrap();
        *fixture.host.lock().unwrap() = Some(host);
        Ok(Self { host, fixture })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(a) => {
                assert_eq!(
                    self.host.send(
                        &Message::SessionIdentify(SessionIdentify {
                            handle: a.handle,
                            game: "testkit".into(),
                            variant: self.fixture.variant.into(),
                            name: "R0".into(),
                            version: "1".into(),
                            focus_priority: false,
                        }),
                        0
                    ),
                    vosu_status::VOSU_OK
                );
                self.fixture.attached.try_send(a.handle).unwrap();
            }
            Message::ResourceResolve(_) => {
                let response = if self.fixture.stream {
                    ResourceResolved::Stream {
                        stream: correlation,
                        size: self
                            .fixture
                            .stream_size
                            .load(std::sync::atomic::Ordering::Relaxed),
                    }
                } else {
                    ResourceResolved::Path {
                        root: "songs".into(),
                        path: "chart.osu".into(),
                    }
                };
                assert_eq!(
                    self.host
                        .send(&Message::ResourceResolved(response), correlation),
                    vosu_status::VOSU_OK
                );
            }
            Message::StreamRead(read) => {
                if self
                    .fixture
                    .hold_reads
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    return Ok(());
                }
                let bytes = b"stream!";
                let start = read.offset as usize;
                let end = bytes.len().min(start + read.len as usize);
                assert_eq!(
                    self.host.send(
                        &Message::StreamChunk(vosu_plugin_api::StreamChunk::Data {
                            data: vosu_plugin_api::CborBytes(bytes[start..end].to_vec()),
                        }),
                        correlation
                    ),
                    vosu_status::VOSU_OK
                );
            }
            Message::StreamClose(close) => {
                self.fixture.closed.try_send(close.stream).unwrap();
            }
            _ => {}
        }
        Ok(())
    }
}
vosu_plugin_api::export_plugin!(bundled Provider);

struct Case {
    rt: tokio::runtime::Runtime,
    host: Arc<crate::plugin::PluginHost>,
    requester: Arc<dyn BundledRequester>,
    selected: vosu_surface::state::IdentifiedSession,
    instance: PluginInstanceId,
    handle: u64,
    fixture: Arc<Fixture>,
    closed: Receiver<u64>,
    attachments: Receiver<u64>,
    dir: tempfile::TempDir,
    _logs: tempfile::TempDir,
}
impl Case {
    fn new(
        name: &str,
        instance: u64,
        stream: bool,
        resource_grant: bool,
        field_grant: bool,
    ) -> Self {
        Self::variant(
            name,
            instance,
            stream,
            resource_grant,
            field_grant,
            "stable",
        )
    }
    fn variant(
        name: &str,
        instance: u64,
        stream: bool,
        resource_grant: bool,
        field_grant: bool,
        variant: &'static str,
    ) -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _enter = rt.enter();
        let (logs, host) = test_host(rt.handle().clone());
        let requirements = if resource_grant {
            "[[requires]]\nid='resource:chart.file'\nversion='*'\n"
        } else {
            ""
        };
        let fields = if field_grant {
            "[[requires]]\nid='field:vosu.chart.resources.file'\nversion='*'\n"
        } else {
            ""
        };
        let manifest = session_manifest(name, &[name]).replace("[match]", &format!(
            "root_slots=['songs','files','nested']\n[[resource_kinds]]\nkind='file'\nfamilies=['chart-text']\n{fields}{requirements}[match]"
        ));
        let (attached, attachments) = sync_channel(2);
        let (closed, close_events) = sync_channel(8);
        let fixture = Arc::new(Fixture {
            host: Mutex::new(None),
            attached,
            closed,
            stream,
            variant,
            stream_size: std::sync::atomic::AtomicU64::new(7),
            hold_reads: std::sync::atomic::AtomicBool::new(false),
        });
        FIXTURES
            .lock()
            .unwrap()
            .insert(name.into(), fixture.clone());
        let loaded = registry::load_bundled(&[registry::BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_plugin_vtable,
        }])
        .unwrap();
        let instance = PluginInstanceId::new(instance);
        let dir = tempfile::tempdir().unwrap();
        host.create_instance(instance, &loaded[0], dir.path().to_owned(), None)
            .unwrap();
        host.process_appeared(test_process(instance.0 as u32, name));
        let handle = attachments.recv_timeout(RECV_TIMEOUT).unwrap();
        let requester = host.surface_requester(instance).unwrap();
        let selected = requester.capture_roster("testkit").unwrap().sessions[0].clone();
        assert_eq!(
            host.handle(
                instance,
                Message::RootDeclare(RootDeclare {
                    handle: None,
                    name: "songs".into(),
                    path: dir.path().to_str().unwrap().into(),
                }),
                0
            ),
            vosu_status::VOSU_OK
        );
        drop(_enter);
        Self {
            rt,
            host,
            requester,
            selected,
            instance,
            handle,
            fixture,
            closed: close_events,
            attachments,
            dir,
            _logs: logs,
        }
    }
    fn run<F: Future>(&self, future: F) -> F::Output {
        self.run_within(RECV_TIMEOUT, future)
    }
    fn run_within<F: Future>(&self, bound: Duration, future: F) -> F::Output {
        self.rt.block_on(async {
            tokio::time::timeout(bound, future)
                .await
                .expect("bounded R0 operation")
        })
    }
    fn reference(&self, content: &str) -> String {
        let reference = vosu_plugin_api::encode(&ResourceRef {
            kind: "file".into(),
            key: CborValue::Integer(51.into()),
            name: "chart.osu".into(),
            content_id: CborValue::Text(content.into()),
        })
        .unwrap();
        assert_eq!(
            self.fixture
                .host
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send_flat(
                    vosu_opcode::VOSU_OP_PUBLISH_BATCH,
                    0,
                    0,
                    &batch(
                        self.handle,
                        &[(
                            "vosu.chart",
                            "/resources/file",
                            PublishValueRef::ResourceRef(&reference)
                        )]
                    ),
                ),
            vosu_status::VOSU_OK
        );
        let state = self
            .host
            .state()
            .store
            .current(SessionKey(self.handle))
            .unwrap();
        let Some(StateValue::String(url)) = state.get(
            "vosu.chart",
            &Pointer::parse("/resources/file/url").unwrap(),
        ) else {
            panic!("reference URL")
        };
        url.rsplit('/').next().unwrap().to_owned()
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        self.host.destroy_all(RECV_TIMEOUT);
    }
}

#[test]
fn surface_roots_scope_epoch_owner_and_same_open_object() {
    let case = Case::new("test.r0.roots", 18901, false, true, true);
    std::fs::write(case.dir.path().join("chart.osu"), b"wide").unwrap();
    let relative = RelativeChild::parse("chart.osu").unwrap();
    let old = case
        .requester
        .declared_root(&case.selected, "songs")
        .unwrap();
    let opened = case.run(old.open(&relative)).unwrap();
    assert_eq!(case.run(opened.read(0, 64)).unwrap(), b"wide");
    let scoped = tempfile::tempdir().unwrap();
    std::fs::write(scoped.path().join("chart.osu"), b"scoped-unique").unwrap();
    assert_eq!(
        case.host.handle(
            case.instance,
            Message::RootDeclare(RootDeclare {
                handle: Some(case.handle),
                name: "songs".into(),
                path: scoped.path().to_str().unwrap().into(),
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    assert!(matches!(
        case.run(old.open(&relative)),
        Err(FileError::TargetGone)
    ));
    // Already selected bytes never retry against the new root/focused session.
    assert_eq!(case.run(opened.read(0, 64)).unwrap(), b"wide");
    let root = case
        .requester
        .declared_root(&case.selected, "songs")
        .unwrap();
    let file = case.run(root.open(&relative)).unwrap();
    assert_eq!(file.metadata.size, 13);
    assert_eq!(case.run(file.read(0, 64)).unwrap(), b"scoped-unique");
    let mut forged = case.selected.clone();
    forged.owner_instance += 1;
    assert!(case.requester.declared_root(&forged, "songs").is_err());
    forged = case.selected.clone();
    forged.generation += 1;
    assert!(case.requester.declared_root(&forged, "songs").is_err());
    forged = case.selected.clone();
    forged.owner = "foreign.owner".into();
    assert!(case.requester.declared_root(&forged, "songs").is_err());
    case.host.process_exited(case.instance.0 as u32);
    assert!(matches!(
        case.run(root.open(&relative)),
        Err(FileError::TargetGone)
    ));
}

#[test]
fn surface_directory_and_calculation_stay_on_retained_roots() {
    let case = Case::new("test.r0.directory", 18902, false, true, true);
    std::fs::write(case.dir.path().join("MiXeD.osu"), b"inside-root").unwrap();
    std::fs::create_dir(case.dir.path().join("nested")).unwrap();
    std::fs::write(case.dir.path().join("nested/unique.osu"), b"nested-root").unwrap();
    let root = case
        .requester
        .declared_root(&case.selected, "songs")
        .unwrap();
    let mut directory = case
        .run(root.directory(&RelativeChild::parse("").unwrap()))
        .unwrap();
    let page = case.run(directory.next_page()).unwrap();
    assert!(!page.truncated);
    assert!(
        page.entries
            .iter()
            .any(|entry| entry.name == "MiXeD.osu" && entry.size == 11)
    );
    let inside = case.dir.path().join("MiXeD.osu");
    let file = case
        .run(
            case.requester
                .open_calculation_path(&case.selected, inside.to_str().unwrap()),
        )
        .unwrap();
    assert_eq!(case.run(file.read(0, 32)).unwrap(), b"inside-root");
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("outside.osu"), b"forbidden").unwrap();
    assert!(matches!(
        case.run(case.requester.open_calculation_path(
            &case.selected,
            outside.path().join("outside.osu").to_str().unwrap()
        )),
        Err(FileError::InvalidPath)
    ));
    assert_eq!(
        case.host.handle(
            case.instance,
            Message::RootDeclare(RootDeclare {
                handle: Some(case.handle),
                name: "songs".into(),
                path: outside.path().to_str().unwrap().into(),
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    assert!(matches!(
        case.run(directory.next_page()),
        Err(FileError::TargetGone)
    ));
}

#[test]
fn surface_resource_requires_byte_grant_live_membership_and_closes_streams() {
    let denied = Case::new("test.r0.denied", 18903, true, false, true);
    let token = denied.reference("denied");
    assert!(matches!(
        denied.run(
            denied
                .requester
                .open_published_resource(&denied.selected, &token)
        ),
        Err(FileError::PermissionDenied)
    ));
    let case = Case::new("test.r0.resource", 18904, true, true, true);
    let token = case.reference("first");
    let resource = case
        .run(
            case.requester
                .open_published_resource(&case.selected, &token),
        )
        .unwrap();
    assert_eq!(case.run(resource.file.read(0, 3)).unwrap(), b"str");
    drop(resource);
    let first_close = case.closed.recv_timeout(RECV_TIMEOUT).unwrap();
    let resource = case
        .run(
            case.requester
                .open_published_resource(&case.selected, &token),
        )
        .unwrap();
    assert_eq!(case.run(resource.read_chart()).unwrap(), b"stream!");
    assert_ne!(first_close, case.closed.recv_timeout(RECV_TIMEOUT).unwrap());
    assert!(case.closed.try_recv().is_err());
    let _new = case.reference("replacement");
    assert!(
        case.run(
            case.requester
                .open_published_resource(&case.selected, &token)
        )
        .is_err()
    );
}

#[test]
fn surface_resource_files_use_published_type_and_same_object_bytes() {
    let case = Case::new("test.r0.file", 18905, false, true, true);
    std::fs::write(case.dir.path().join("chart.osu"), b"actual chart bytes").unwrap();
    let token = case.reference("file-identity");
    let resource = case
        .run(
            case.requester
                .open_published_resource(&case.selected, &token),
        )
        .unwrap();
    assert_eq!(resource.identity, token);
    assert_eq!(resource.media_type, "text/plain; charset=utf-8");
    assert_eq!(resource.file.metadata.size, 18);
    assert_eq!(
        case.run(resource.read_chart()).unwrap(),
        b"actual chart bytes"
    );
}

#[test]
fn surface_resource_limit_and_cancel_release_the_request_owned_stream() {
    use std::sync::atomic::Ordering;
    let case = Case::new("test.r0.limit", 18906, true, true, true);
    let token = case.reference("limit");
    case.fixture.stream_size.store(
        vosu_surface::resources::MAX_CHART_BYTES + 1,
        Ordering::Relaxed,
    );
    assert!(matches!(
        case.run(
            case.requester
                .open_published_resource(&case.selected, &token)
        ),
        Err(FileError::LimitExceeded)
    ));
    let oversized = case.closed.recv_timeout(RECV_TIMEOUT).unwrap();
    case.fixture.stream_size.store(7, Ordering::Relaxed);
    let resource = case
        .run(
            case.requester
                .open_published_resource(&case.selected, &token),
        )
        .unwrap();
    case.fixture.hold_reads.store(true, Ordering::Relaxed);
    assert!(
        case.run(async {
            tokio::time::timeout(Duration::from_millis(10), resource.file.read(0, 3)).await
        })
        .is_err()
    );
    assert_ne!(oversized, case.closed.recv_timeout(RECV_TIMEOUT).unwrap());
    drop(resource);
    assert!(case.closed.try_recv().is_err());
}

/// One full page opens each of its 16,384 children through the retained
/// directory: about 73 ms on Linux but 2.3 s on native Windows alone and
/// over 5 s under the parallel native suite. This remains a hang guard.
const PAGE_BOUND: Duration = Duration::from_secs(60);

#[test]
fn surface_directory_pages_are_bounded_and_do_not_claim_false_absence() {
    let case = Case::new("test.r0.pages", 18907, false, true, true);
    for index in 0..16_400 {
        std::fs::write(case.dir.path().join(format!("{index:05}.osu")), []).unwrap();
    }
    let root = case
        .requester
        .declared_root(&case.selected, "songs")
        .unwrap();
    let mut directory = case
        .run(root.directory(&RelativeChild::parse("").unwrap()))
        .unwrap();
    let first = case.run_within(PAGE_BOUND, directory.next_page()).unwrap();
    assert!(first.truncated);
    assert_eq!(first.entries.len(), 16_384);
    let second = case.run_within(PAGE_BOUND, directory.next_page()).unwrap();
    assert!(!second.truncated);
    assert_eq!(second.entries.len(), 16);
    let names = first
        .entries
        .into_iter()
        .chain(second.entries)
        .map(|entry| entry.name)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(names.len(), 16_400);
}

#[test]
fn surface_resource_byte_grant_does_not_grant_its_reference_field() {
    let case = Case::new("test.r0.no-field", 18908, true, true, false);
    let token = case.reference("private-selection");
    assert!(matches!(
        case.run(
            case.requester
                .open_published_resource(&case.selected, &token)
        ),
        Err(FileError::PermissionDenied)
    ));
}

#[test]
fn surface_published_resource_cannot_borrow_membership_from_another_session() {
    let case = Case::new("test.r0.membership", 18909, true, true, true);
    let token = case.reference("only-first-session");
    case.host
        .process_appeared(test_process(18910, "test.r0.membership"));
    let handle = case.attachments.recv_timeout(RECV_TIMEOUT).unwrap();
    assert_ne!(handle, case.handle);
    let roster = case.requester.capture_roster("testkit").unwrap();
    let other = roster
        .sessions
        .iter()
        .find(|entry| entry.session != case.selected.session)
        .unwrap();
    assert!(matches!(
        case.run(case.requester.open_published_resource(other, &token)),
        Err(FileError::PermissionDenied)
    ));
    assert_eq!(
        case.run(
            case.requester
                .open_published_resource(&case.selected, &token)
        )
        .unwrap()
        .file
        .metadata
        .size,
        7
    );
}

#[test]
fn surface_calculation_relative_root_tracks_variant_and_rejects_forgery() {
    for (variant, name, id) in [
        ("stable", "test.r0.calc.stable", 18911),
        ("lazer", "test.r0.calc.lazer", 18912),
    ] {
        let case = Case::variant(name, id, false, true, true, variant);
        let files = tempfile::tempdir().unwrap();
        std::fs::write(case.dir.path().join("chart.osu"), b"stable-songs").unwrap();
        std::fs::write(files.path().join("chart.osu"), b"lazer-files").unwrap();
        assert_eq!(
            case.host.handle(
                case.instance,
                Message::RootDeclare(RootDeclare {
                    handle: Some(case.handle),
                    name: "files".into(),
                    path: files.path().to_str().unwrap().into(),
                }),
                0
            ),
            vosu_status::VOSU_OK
        );
        let opened = case
            .run(
                case.requester
                    .open_calculation_path(&case.selected, "chart.osu"),
            )
            .unwrap();
        assert_eq!(
            case.run(opened.read(0, 64)).unwrap(),
            if variant == "lazer" {
                b"lazer-files".as_slice()
            } else {
                b"stable-songs".as_slice()
            }
        );
        let mut forged = case.selected.clone();
        forged.variant = if variant == "lazer" {
            "stable"
        } else {
            "lazer"
        }
        .into();
        assert!(matches!(
            case.run(case.requester.open_calculation_path(&forged, "chart.osu")),
            Err(FileError::PermissionDenied)
        ));
    }
}

#[test]
fn surface_calculation_rejects_canonical_non_osu_alias() {
    let case = Case::new("test.r0.calc.extension", 18913, false, true, true);
    std::fs::write(case.dir.path().join("actual.txt"), b"not-a-chart").unwrap();
    std::fs::write(case.dir.path().join("actual.osu"), b"chart").unwrap();
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as symlink;
    symlink("actual.txt", case.dir.path().join("alias.osu")).unwrap();
    symlink("actual.osu", case.dir.path().join("valid.osu")).unwrap();
    for spelling in [
        "alias.osu".to_owned(),
        case.dir
            .path()
            .join("alias.osu")
            .to_str()
            .unwrap()
            .to_owned(),
    ] {
        assert!(matches!(
            case.run(
                case.requester
                    .open_calculation_path(&case.selected, &spelling)
            ),
            Err(FileError::InvalidPath)
        ));
    }
    let opened = case
        .run(
            case.requester
                .open_calculation_path(&case.selected, "valid.osu"),
        )
        .unwrap();
    assert_eq!(case.run(opened.read(0, 64)).unwrap(), b"chart");
}

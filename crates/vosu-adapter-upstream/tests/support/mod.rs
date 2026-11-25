#![expect(
    dead_code,
    reason = "shared confinement fixture exposes helpers to several integration targets"
)]
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, LazyLock, Mutex,
        mpsc::{SyncSender, sync_channel},
    },
    time::Duration,
};
use vosu_core::plugin::{self, PluginHost, PluginInstanceId};
use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, PublishBatchWriter, PublishValueRef,
    RootDeclare, SessionIdentify, VtableHostServices, vosu_opcode, vosu_status,
};
use vosu_surface::requester::BundledRequester;
pub const WAIT: Duration = Duration::from_secs(10);
pub const MAP: &[u8] = b"osu file format v14\n\n[General]\nAudioFilename: audio.mp3\nMode: 0\n\n[Metadata]\nTitle:R1\nArtist:Fixture\nCreator:Fixture\nVersion:Declared\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,1,100,1,0\n\n[HitObjects]\n256,192,1000,1,0,0:0:0:0:\n128,192,1500,1,0,0:0:0:0:\n";
static PENDING: LazyLock<Mutex<HashMap<PathBuf, SyncSender<u64>>>> =
    LazyLock::new(Default::default);
struct Publisher {
    host: VtableHostServices,
    dir: PathBuf,
    ready: SyncSender<u64>,
    lazer: bool,
}
impl Plugin for Publisher {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let ready = PENDING.lock().unwrap().remove(&init.data_dir).unwrap();
        Ok(Self {
            host,
            dir: init.data_dir,
            ready,
            lazer: false,
        })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                self.lazer = attached.command_line.as_deref() == Some("lazer");
                self.send(
                    Message::SessionIdentify(SessionIdentify {
                        handle: attached.handle,
                        game: "osu".into(),
                        variant: if self.lazer { "lazer" } else { "stable" }.into(),
                        name: "R1".into(),
                        version: "1".into(),
                        focus_priority: false,
                    }),
                    0,
                );
                for slot in ["songs", "files", "skins"] {
                    self.send(
                        Message::RootDeclare(RootDeclare {
                            handle: Some(attached.handle),
                            name: slot.into(),
                            path: if slot == "skins" {
                                self.dir.join(slot).join("Skin")
                            } else {
                                self.dir.join(slot)
                            }
                            .to_str()
                            .unwrap()
                            .into(),
                        }),
                        0,
                    );
                }
                let paths = serde_json::json!({
                    "stable":{"game":"/forged/outside","songs":"/forged/outside","skins":"/forged/outside","replays":null},
                    "lazer":null,
                    "current":{"beatmapFolder":"Set","beatmapFile":"map.osu","background":"bg.jpg","audio":"audio.mp3"},
                    "skinFolder":"Skin"
                });
                let mut cbor = Vec::new();
                ciborium::into_writer(&paths, &mut cbor).unwrap();
                let reference = vosu_plugin_api::encode(&vosu_plugin_api::ResourceRef {
                    kind: "file".into(),
                    key: vosu_plugin_api::CborValue::Integer(1.into()),
                    name: "map.osu".into(),
                    content_id: vosu_plugin_api::CborValue::Text("r1-chart".into()),
                })
                .unwrap();
                let mut batch = PublishBatchWriter::new();
                batch.begin(attached.handle);
                batch.push("vosu.osu", "/internal/paths", PublishValueRef::Cbor(&cbor));
                batch.push(
                    "vosu.chart",
                    "/resources/file",
                    PublishValueRef::ResourceRef(&reference),
                );
                let mut payload = Vec::new();
                batch.finish_into(&mut payload).unwrap();
                assert_eq!(
                    self.host
                        .send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &payload),
                    vosu_status::VOSU_OK
                );
                self.ready.try_send(attached.handle).unwrap();
            }
            Message::ResourceResolve(_) => {
                self.send(
                    Message::ResourceResolved(vosu_plugin_api::ResourceResolved::Path {
                        root: if self.lazer { "files" } else { "songs" }.into(),
                        path: "Set/map.osu".into(),
                    }),
                    correlation,
                );
            }
            _ => {}
        }
        Ok(())
    }
}
impl Publisher {
    fn send(&self, message: Message, correlation: u64) {
        assert_eq!(self.host.send(&message, correlation), vosu_status::VOSU_OK);
    }
}
vosu_plugin_api::export_plugin!(bundled Publisher);
mod adapter {
    use super::*;
    pub struct Adapter;
    impl Plugin for Adapter {
        fn create(_: VtableHostServices, _: Init) -> Result<Self, PluginError> {
            Ok(Self)
        }
        fn on_message(&mut self, _: Message, _: u64) -> Result<(), PluginError> {
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled Adapter);
}
struct Storage;
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, plugin::settings::SettingsError> {
        Ok(Default::default())
    }
    fn store(
        &self,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        Ok(())
    }
}
struct Console;
impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}
pub struct Case {
    pub host: Arc<PluginHost>,
    pub requester: Arc<dyn BundledRequester>,
    pub provider: vosu_adapter_upstream::files::Provider,
    pub root: tempfile::TempDir,
    pub runtime: tokio::runtime::Runtime,
    pub handle: u64,
    pub lazer: bool,
}
impl Case {
    pub fn new(lazer: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let enter = runtime.enter();
        let root = tempfile::tempdir().unwrap();
        for slot in ["songs", "files", "skins"] {
            std::fs::create_dir(root.path().join(slot)).unwrap();
        }
        for slot in ["songs", "files"] {
            std::fs::create_dir(root.path().join(slot).join("Set")).unwrap();
            std::fs::write(root.path().join(slot).join("Set/map.osu"), MAP).unwrap();
            std::fs::write(
                root.path().join(slot).join("Set/bg.jpg"),
                if slot == "songs" {
                    b"stable-bg"
                } else {
                    b"lazer-bg!"
                },
            )
            .unwrap();
            std::fs::write(root.path().join(slot).join("Set/audio.mp3"), b"0123456789").unwrap();
        }
        std::fs::create_dir_all(root.path().join("skins/Skin/SubDir")).unwrap();
        std::fs::write(
            root.path().join("skins/Skin/SubDir/MiXeD.PNG"),
            b"skin-exact",
        )
        .unwrap();
        let logger =
            vosu_core::logging::Logger::new(root.path(), Default::default(), Arc::new(Console))
                .unwrap();
        let host = PluginHost::new(
            logger,
            Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
                Storage,
            ))),
            runtime.handle().clone(),
        );
        let (send, recv) = sync_channel(1);
        PENDING.lock().unwrap().insert(root.path().to_owned(), send);
        let manifest = "kind='plugin'\nmanifest_version=1\nid='vosu.osu'\nname='R1 Fixture'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nroot_slots=['songs','files','skins']\n[[provides]]\nid='field:vosu.osu'\nversion='1.3.0'\nschema='osu.json'\n[[provides]]\nid='game:osu'\nversion='*'\n[[resource_kinds]]\nkind='file'\nfamilies=['chart-text']\n[match]\nprocess_names=['r1-fixture']\n";
        // As composition does, the batch's bundled source is a known provider.
        let mut snapshot = host.validation_snapshot();
        snapshot.providers.push(plugin::ProviderRegistration {
            id: "vosu.osu".into(),
            official: true,
            builtin: true,
            replaces: Vec::new(),
            revision: None,
        });
        let loaded = plugin::load_bundled(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &snapshot,
            &[
                plugin::BundledPlugin {
                    manifest,
                    settings_schema: None,
                    namespace_schemas: &[vosu_osu::OSU_NAMESPACE_SCHEMA],
                    action_schemas: &[],
                    vtable: vosu_plugin_vtable,
                },
                plugin::BundledPlugin {
                    manifest: vosu_adapter_upstream::MANIFEST,
                    settings_schema: None,
                    namespace_schemas: &[],
                    action_schemas: &[],
                    vtable: adapter::vosu_plugin_vtable,
                },
            ],
        )
        .unwrap();
        host.create_instance(
            PluginInstanceId::new(1),
            &loaded[0],
            root.path().into(),
            None,
        )
        .unwrap();
        host.create_instance(
            PluginInstanceId::new(2),
            &loaded[1],
            root.path().join("adapter"),
            None,
        )
        .unwrap();
        host.process_appeared(vosu_core::plugin::ProcessRecord {
            pid: 9911,
            name: "r1-fixture".into(),
            path: root.path().join("game").to_str().unwrap().into(),
            command_line: if lazer { "lazer" } else { "stable" }.into(),
            bitness: vosu_plugin_api::Bitness::X64,
            start_time: 1,
            argv: None,
        });
        let handle = recv.recv_timeout(WAIT).unwrap();
        let requester = host.surface_requester(PluginInstanceId::new(2)).unwrap();
        let provider = vosu_adapter_upstream::files::Provider::new(requester.clone());
        drop(enter);
        Self {
            host,
            requester,
            provider,
            root,
            runtime,
            handle,
            lazer,
        }
    }
    pub fn run<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(async {
            tokio::time::timeout(WAIT, future)
                .await
                .expect("bounded R1 operation")
        })
    }
    pub fn songs(&self) -> PathBuf {
        self.root
            .path()
            .join(if self.lazer { "files" } else { "songs" })
    }
}
impl Drop for Case {
    fn drop(&mut self) {
        self.host.destroy_all(WAIT);
    }
}

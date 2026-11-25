//! R5 production osu provider -> real ABI -> public state -> HTTP matrix.
#![expect(
    clippy::disallowed_methods,
    reason = "Isolated resource files and mutation"
)]
use super::support::*;
use axum::http::{StatusCode, header};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock, Mutex},
};
use vosu_osu::resources::{PreparedResources, ResourceWorker, RootPaths, Selection, TreeSource};
use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, PublishBatchWriter, PublishValueRef,
    ResourceResolved, SessionIdentify, VtableHostServices, encode, vosu_opcode, vosu_status,
};

const CHART: &[u8] = b"osu file format v14\n// chart-975201\n";
const AUDIO: &[u8] = b"OggS-audio-nonzero-975202";
const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\nnonzero-image-975203";
#[derive(Default)]
struct Control {
    prepared: Option<PreparedResources>,
    host: Option<VtableHostServices>,
    handle: Option<u64>,
    resolves: usize,
    reads: usize,
    closes: usize,
}
static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<Mutex<Control>>>>> =
    LazyLock::new(Mutex::default);
struct OsuProvider {
    control: Arc<Mutex<Control>>,
    host: VtableHostServices,
}
fn publish(control: &Arc<Mutex<Control>>) {
    let (host, handle, declarations, references) = {
        let c = control.lock().unwrap();
        let handle = c.handle.unwrap();
        let prepared = c.prepared.as_ref().unwrap();
        (
            c.host.unwrap(),
            handle,
            prepared.declarations(handle),
            prepared.references(),
        )
    };
    for root in declarations {
        assert_eq!(
            host.send(&Message::RootDeclare(root), 0),
            vosu_status::VOSU_OK
        );
    }
    let encoded: Vec<_> = references.iter().map(|r| encode(r).unwrap()).collect();
    let mut batch = PublishBatchWriter::new();
    batch.begin(handle);
    for (reference, bytes) in references.iter().zip(&encoded) {
        let (namespace, pointer, tree) = match reference.kind.as_str() {
            "file" => ("vosu.chart", "/resources/file", false),
            "audio" => ("vosu.chart", "/resources/audio", false),
            "background" => ("vosu.chart", "/resources/background", false),
            "set" => ("vosu.chart", "/resources/set", true),
            "skin" => ("vosu.skin", "/tree", true),
            _ => panic!("unexpected kind"),
        };
        batch.push(
            namespace,
            pointer,
            if tree {
                PublishValueRef::TreeRef(bytes)
            } else {
                PublishValueRef::ResourceRef(bytes)
            },
        );
    }
    let mut bytes = Vec::new();
    batch.finish_into(&mut bytes).unwrap();
    assert_eq!(
        host.send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes),
        vosu_status::VOSU_OK
    );
}
impl Plugin for OsuProvider {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let control = REGISTRY.lock().unwrap().remove(&init.plugin_id).unwrap();
        control.lock().unwrap().host = Some(host);
        Ok(Self { control, host })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        if let Message::SessionAttached(s) = message {
            self.control.lock().unwrap().handle = Some(s.handle);
            assert_eq!(
                self.host.send(
                    &Message::SessionIdentify(SessionIdentify {
                        handle: s.handle,
                        game: "osu!".into(),
                        variant: "production-provider".into(),
                        name: "R5 osu provider".into(),
                        version: "975201".into(),
                        focus_priority: false,
                    }),
                    0
                ),
                vosu_status::VOSU_OK
            );
            publish(&self.control);
            return Ok(());
        }
        // The provider performs blocking safe-open on the serialized plugin
        // dispatch thread, never on a Tokio worker. Release before calling host.
        let reply = {
            let mut c = self.control.lock().unwrap();
            match message {
                Message::ResourceResolve(r) => {
                    c.resolves += 1;
                    c.prepared
                        .as_mut()
                        .unwrap()
                        .resolve(&r.kind, &r.key)
                        .map(Message::ResourceResolved)
                }
                Message::TreeList(r) => c
                    .prepared
                    .as_ref()
                    .unwrap()
                    .list(&r.kind, &r.key)
                    .map(Message::ResourceResolved),
                Message::TreeLookup(r) => c
                    .prepared
                    .as_ref()
                    .unwrap()
                    .lookup(&r.kind, &r.key, &r.path)
                    .map(Message::ResourceResolved),
                Message::StreamRead(r) => {
                    c.reads += 1;
                    c.prepared
                        .as_ref()
                        .unwrap()
                        .read(r)
                        .map(Message::StreamChunk)
                }
                Message::StreamClose(r) => {
                    c.closes += 1;
                    c.prepared.as_mut().unwrap().close(r.stream);
                    None
                }
                _ => None,
            }
        };
        if let Some(reply) = reply {
            assert_eq!(self.host.send(&reply, correlation), vosu_status::VOSU_OK);
        }
        Ok(())
    }
}
fn vtable() -> vosu_plugin_api::vosu_plugin_vtable {
    vosu_plugin_api::__vtable_for_serialized::<OsuProvider>()
}
async fn prepare(selection: Selection) -> PreparedResources {
    let worker = ResourceWorker::start();
    let prepared = bounded(worker.prepare(selection).unwrap()).await.unwrap();
    bounded(worker.shutdown()).await;
    prepared
}
fn selection(root: &std::path::Path, lazer: bool) -> Selection {
    let names = if lazer {
        ["a/hashchart", "b/hashaudio", "c/hashimage"]
    } else {
        ["map.osu", "audio.ogg", "background.png"]
    };
    let entries = BTreeMap::from([
        ("map.osu".into(), names[0].into()),
        ("audio.ogg".into(), names[1].into()),
        ("background.png".into(), names[2].into()),
    ]);
    Selection {
        roots: RootPaths {
            songs: Some(root.into()),
            skin: Some(root.into()),
        },
        lazer,
        chart: Some(names[0].into()),
        audio: Some(names[1].into()),
        background: Some(names[2].into()),
        chart_name: Some("map.osu".into()),
        audio_name: Some("audio.ogg".into()),
        background_name: Some("background.png".into()),
        set: Some(if lazer {
            TreeSource::Store(entries.clone())
        } else {
            TreeSource::Folder(String::new())
        }),
        skin: Some(if lazer {
            TreeSource::Store(entries)
        } else {
            TreeSource::Folder(String::new())
        }),
    }
}
async fn ready(_h: &Harness, server: &Server) -> Value {
    bounded(async {
        loop {
            let (status, value) = server.get("/sessions/active/state").await;
            if status == StatusCode::OK && value["vosu.skin"]["tree"]["url"].is_string() {
                return value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}
fn url(server: &Server, reference: &Value) -> String {
    format!(
        "{}{}",
        server.base.trim_end_matches("/api/v1"),
        reference["url"].as_str().unwrap()
    )
}
async fn closed(control: &Arc<Mutex<Control>>, n: usize) {
    bounded(async {
        loop {
            if control.lock().unwrap().closes == n {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

#[derive(Default)]
struct Captured(Mutex<String>);
impl vosu_core::logging::ConsoleSink for Captured {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, line: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().push_str(line);
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn resources_real_osu_stable_and_lazer_direct_redirect_trees_mutation_and_privacy() {
    for (id, pid, lazer) in [
        ("test.r5.osu.stable", 975201, false),
        ("test.r5.osu.lazer", 975202, true),
    ] {
        let captured = Arc::new(Captured::default());
        let (mut h, logger) = Harness::with_console(captured.clone());
        logger.info("R5-log-capture-positive-control-975201");
        let source = tempfile::Builder::new()
            .prefix("R5-private-install-975201-")
            .tempdir()
            .unwrap();
        for (path, bytes) in [
            ("map.osu", CHART),
            ("audio.ogg", AUDIO),
            ("background.png", IMAGE),
            ("a/hashchart", CHART),
            ("b/hashaudio", AUDIO),
            ("c/hashimage", IMAGE),
        ] {
            let path = source.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let selected = selection(source.path(), lazer);
        let prepared = h.runtime.block_on(bounded(prepare(selected.clone())));
        let control = Arc::new(Mutex::new(Control {
            prepared: Some(prepared),
            ..Control::default()
        }));
        assert!(
            REGISTRY
                .lock()
                .unwrap()
                .insert(id.into(), control.clone())
                .is_none()
        );
        let mut text = format!(
            "{}\nroot_slots=['songs','files','skins']\n[match]\nprocess_names=['{id}']\n[[provides]]\nid='game:fixture'\nversion='*'\n",
            manifest(id)
        );
        for (kind, tree, family) in [
            ("file", false, "chart-text"),
            ("audio", false, "audio"),
            ("background", false, "image"),
            ("set", true, "chart-text"),
            ("skin", true, "image"),
        ] {
            text.push_str(&format!(
                "[[resource_kinds]]\nkind='{kind}'\ntree={tree}\nfamilies=['{family}']\n"
            ));
        }
        h.load(&text, &[], &[], vtable);
        h.host.process_appeared(record(pid, id));
        h.runtime.block_on(bounded(async {
            let server = Server::new(&h, false).await;
            let before = ready(&h, &server).await;
            let sentinel = source.path().to_str().unwrap();
            assert!(!before.to_string().contains(sentinel));
            assert!(!before.to_string().contains("hashchart"));
            assert_eq!(control.lock().unwrap().resolves, 0);
            let no_redirect = reqwest::Client::builder().no_proxy().timeout(WAIT).redirect(reqwest::redirect::Policy::none()).build().unwrap();
            for (index, (kind, expected)) in [("file", CHART), ("audio", AUDIO), ("background", IMAGE)].into_iter().enumerate() {
                let reference = &before["vosu.chart"]["resources"][kind];
                let direct = url(&server, reference);
                let redirect = format!("{}/sessions/active/chart/{kind}", server.base);
                let response = no_redirect.get(&redirect).send().await.unwrap();
                assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
                assert_eq!(response.headers()[header::LOCATION], reference["url"].as_str().unwrap());
                assert_eq!(control.lock().unwrap().resolves, index * 2);
                for endpoint in [&direct, &redirect] {
                    let response = server.client.get(endpoint).send().await.unwrap();
                    assert_eq!(response.status(), StatusCode::OK);
                    assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
                }
                closed(&control, (index + 1) * 2).await;
                assert_eq!(control.lock().unwrap().resolves, (index + 1) * 2);
                assert_eq!(control.lock().unwrap().reads, (index + 1) * 2);
            }
            let tree = url(&server, &before["vosu.chart"]["resources"]["set"]);
            let index: Value = server.client.get(&tree).send().await.unwrap().json().await.unwrap();
            assert!(index["entries"].as_array().unwrap().iter().any(|v| v["path"] == "map.osu"));
            for (path, expected) in [("map.osu", CHART), ("audio.ogg", AUDIO), ("background.png", IMAGE)] {
                let response = server.client.get(format!("{tree}{path}")).send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
            }
            let skin = url(&server, &before["vosu.skin"]["tree"]);
            for endpoint in [format!("{skin}BACKGROUND.PNG"), format!("{}/sessions/active/skin/BACKGROUND.PNG", server.base)] {
                let response = server.client.get(endpoint).send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
                assert_eq!(response.bytes().await.unwrap().as_ref(), IMAGE);
            }
            let changed = b"osu file format v14\n// mutation-generation-975204-much-longer\n";
            let path = source.path().join(if lazer { "a/hashchart" } else { "map.osu" });
            std::fs::write(path, changed).unwrap();
            let original = &before["vosu.chart"]["resources"]["file"];
            let old_key = control.lock().unwrap().prepared.as_ref().unwrap().references().into_iter().find(|r| r.kind == "file").unwrap();
            assert!(matches!(control.lock().unwrap().prepared.as_mut().unwrap().resolve("file", &old_key.key), Some(ResourceResolved::Error { error }) if error.code == "gone"));
            let response = server.client.get(url(&server, original)).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::GONE);
            let error = response.text().await.unwrap();
            assert!(error.contains("unpublished_resource"));
            assert!(!error.contains(sentinel));
            let prepared = prepare(selected).await;
            control.lock().unwrap().prepared = Some(prepared);
            publish(&control);
            let after = ready(&h, &server).await;
            assert_ne!(after["vosu.chart"]["resources"]["file"]["url"], original["url"]);
            for endpoint in [url(&server, &after["vosu.chart"]["resources"]["file"]), format!("{}/sessions/active/chart/file", server.base)] {
                let response = server.client.get(endpoint).send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.bytes().await.unwrap().as_ref(), changed);
            }
            closed(&control, 8).await;
            assert_eq!(control.lock().unwrap().resolves, 9);
            assert_eq!(control.lock().unwrap().reads, 8);
            assert_eq!(server.client.get(url(&server, original)).send().await.unwrap().status(), StatusCode::GONE);
            bounded(logger.flush()).await.unwrap();
            let logs = captured.0.lock().unwrap().clone();
            assert!(logs.contains("R5-log-capture-positive-control-975201"));
            assert!(!logs.contains(sentinel));
            assert!(!logs.contains("hashchart"));
            let persisted = std::fs::read_to_string(h.root.path().join("latest.log")).unwrap();
            assert!(persisted.contains("R5-log-capture-positive-control-975201"));
            assert!(!persisted.contains(sentinel));
            assert!(!persisted.contains("hashchart"));
            server.shutdown().await;
        }));
    }
}

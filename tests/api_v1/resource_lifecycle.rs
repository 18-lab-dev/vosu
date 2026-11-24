//! R5: exercise membership and transport together, through an actual ABI instance.
#![expect(
    clippy::disallowed_methods,
    reason = "Disposable disk mutation fixtures"
)]
use super::support::*;
use axum::http::{StatusCode, header};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use vosu_core::api::{ApiPlan, SessionTarget};
use vosu_core::resources::{RegistryLimits, ResourceRegistry, ResourceSecret};
use vosu_plugin_api::{
    CborValue, HostServices, Init, Message, Plugin, PluginError, PublishBatchWriter,
    PublishValueRef, ResourceRef, RootDeclare, VtableHostServices, encode, vosu_opcode,
    vosu_status,
};
use vosu_plugin_testkit::fixture::{ApiFixturePlugin, STREAM_BYTES};

const FILE: &[u8] = include_bytes!("../fixtures/api-v1-plugin/fixture.osu");
#[derive(Default)]
struct Observed {
    host: Option<VtableHostServices>,
    handles: Vec<u64>,
    resolve: usize,
    read: usize,
    close: usize,
    held: Option<(Message, u64)>,
    hold: bool,
    hold_list: bool,
    hold_lookup: bool,
    large_stream: bool,
    hold_second_read: bool,
    lists: usize,
    lookups: usize,
}
fn registrations() -> &'static Mutex<HashMap<String, Arc<Mutex<Observed>>>> {
    static REGISTRY: LazyLock<Mutex<HashMap<String, Arc<Mutex<Observed>>>>> =
        LazyLock::new(Mutex::default);
    &REGISTRY
}
struct Fixture {
    inner: ApiFixturePlugin,
    observed: Arc<Mutex<Observed>>,
    host: VtableHostServices,
    decoy: String,
}
impl Plugin for Fixture {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let observed = registrations()
            .lock()
            .unwrap()
            .remove(&init.plugin_id)
            .unwrap();
        observed.lock().unwrap().host = Some(host);
        let decoy = init.data_dir.join("decoy").to_str().unwrap().to_owned();
        Ok(Self {
            inner: ApiFixturePlugin::create(host, init)?,
            observed,
            host,
            decoy,
        })
    }
    fn on_message(&mut self, message: Message, correlation: u64) -> Result<(), PluginError> {
        {
            let mut o = self.observed.lock().unwrap();
            match &message {
                Message::SessionAttached(s) => o.handles.push(s.handle),
                Message::ResourceResolve(_) => {
                    o.resolve += 1;
                    if o.hold {
                        assert!(o.held.replace((message, correlation)).is_none());
                        return Ok(());
                    }
                }
                Message::TreeList(_) => {
                    o.lists += 1;
                    if o.hold_list {
                        assert!(o.held.replace((message, correlation)).is_none());
                        return Ok(());
                    }
                }
                Message::TreeLookup(_) => {
                    o.lookups += 1;
                    if o.hold_lookup {
                        assert!(o.held.replace((message, correlation)).is_none());
                        return Ok(());
                    }
                }
                Message::StreamRead(_) => {
                    o.read += 1;
                    if o.hold_second_read && o.read == 2 {
                        assert!(o.held.replace((message, correlation)).is_none());
                        return Ok(());
                    }
                }
                Message::StreamClose(_) => o.close += 1,
                _ => {}
            }
        }
        let large = self.observed.lock().unwrap().large_stream;
        if large {
            let reply = match &message {
                Message::ResourceResolve(_) => Some(Message::ResourceResolved(
                    vosu_plugin_api::ResourceResolved::Stream {
                        stream: 975301,
                        size: u64::from(vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES) * 3,
                    },
                )),
                Message::StreamRead(r) => {
                    Some(Message::StreamChunk(vosu_plugin_api::StreamChunk::Data {
                        data: vosu_plugin_api::CborBytes(vec![0x7d; r.len as usize]),
                    }))
                }
                _ => None,
            };
            if let Some(reply) = reply {
                assert_eq!(self.host.send(&reply, correlation), vosu_status::VOSU_OK);
                return Ok(());
            }
        }
        let attached = matches!(message, Message::SessionAttached(_));
        self.inner.on_message(message, correlation)?;
        if attached {
            assert_eq!(
                self.host.send(
                    &Message::RootDeclare(RootDeclare {
                        handle: None,
                        name: "fixture".into(),
                        path: self.decoy.clone(),
                    }),
                    0
                ),
                vosu_status::VOSU_OK
            );
        }
        Ok(())
    }
}
fn vtable() -> vosu_plugin_api::vosu_plugin_vtable {
    vosu_plugin_api::__vtable_for_serialized::<Fixture>()
}
fn setup(
    id: &str,
    pid: u32,
    store: Option<Arc<vosu_core::state::ReferenceStore>>,
) -> (Harness, Arc<Mutex<Observed>>) {
    let mut h = Harness::new();
    if let Some(store) = store {
        let logger = {
            let _enter = h.runtime.enter();
            vosu_core::logging::Logger::new(
                h.root.path(),
                vosu_core::logging::LoggerOptions::default(),
                Arc::new(Quiet),
            )
            .unwrap()
        };
        h.host = vosu_core::plugin::PluginHost::new_with_resources(
            logger,
            Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
                Arc::new(Storage),
            )),
            h.runtime.handle().clone(),
            store,
        );
    }
    std::fs::write(h.root.path().join("fixture.osu"), FILE).unwrap();
    std::fs::create_dir(h.root.path().join("decoy")).unwrap();
    std::fs::write(
        h.root.path().join("decoy/fixture.osu"),
        b"wrong-plugin-root-8927",
    )
    .unwrap();
    let observed = Arc::new(Mutex::new(Observed::default()));
    assert!(
        registrations()
            .lock()
            .unwrap()
            .insert(id.into(), observed.clone())
            .is_none()
    );
    let manifest = format!(
        "{}\nroot_slots=['fixture']\n[match]\nprocess_names=['{id}']\n[[provides]]\nid='field:{id}'\nversion='1.0.0'\nschema='namespace.json'\n[[resource_kinds]]\nkind='fixture-file'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-stream'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-tree'\ntree=true\nfamilies=['chart-text']\n",
        manifest(id)
    );
    let schema =
        format!(r#"{{"title":"{id}","version":"1.0.0","type":"object","properties":{{}}}}"#);
    h.load(&manifest, &[&schema], &[], vtable);
    h.host.process_appeared(record(pid, id));
    (h, observed)
}
struct Quiet;
impl vosu_core::logging::ConsoleSink for Quiet {
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
struct Storage;
impl vosu_core::plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, Value>, vosu_core::plugin::settings::SettingsError> {
        Ok(Default::default())
    }
    fn store(
        &self,
        _: &str,
        _: &serde_json::Map<String, Value>,
    ) -> Result<(), vosu_core::plugin::settings::SettingsError> {
        Ok(())
    }
}
async fn state(h: &Harness, id: &str, target: SessionTarget) -> Value {
    bounded(async {
        loop {
            if let Ok(update) = h
                .host
                .api_state_source()
                .read(&target, &ApiPlan::new(&[format!("/{id}")]).unwrap())
                && let Some(Some(value)) = update.values.first()
            {
                let mut bytes = Vec::new();
                value.write_json(&mut bytes);
                let v: Value = serde_json::from_slice(&bytes).unwrap();
                if v["resources"]["tree"]["url"].is_string() {
                    return v;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}
fn endpoint(server: &Server, reference: &Value) -> String {
    format!(
        "{}{}",
        server.base.trim_end_matches("/api/v1"),
        reference["url"].as_str().unwrap()
    )
}
fn publish_ref(o: &Arc<Mutex<Observed>>, id: &str, handle: u64, pointer: &str, content: &str) {
    let host = o.lock().unwrap().host.unwrap();
    let reference = encode(&ResourceRef {
        kind: "fixture-file".into(),
        key: CborValue::Text("file".into()),
        name: "fixture.osu".into(),
        content_id: CborValue::Text(content.into()),
    })
    .unwrap();
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push(id, pointer, PublishValueRef::ResourceRef(&reference));
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    assert_eq!(
        host.send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes),
        vosu_status::VOSU_OK
    );
}
async fn count(o: &Arc<Mutex<Observed>>, close: usize) {
    bounded(async {
        loop {
            if o.lock().unwrap().close == close {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

#[test]
fn resources_shared_sessions_lazy_cache_root_precedence_and_stream_close() {
    let id = "test.r5.shared";
    let (h, o) = setup(id, 975101, None);
    h.runtime.block_on(bounded(async {
        let first = state(&h, id, SessionTarget::Active).await;
        let first_session = h.id();
        h.host.process_appeared(record(975102, id));
        bounded(async {
            loop {
                if o.lock().unwrap().handles.len() == 2
                    && h.host.api_state_source().sessions().unwrap().len() == 2
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        // Explicit session reads avoid relying on foreground ordering.
        let (_, sessions) = {
            let server = Server::new(&h, false).await;
            let result = server.get("/sessions").await;
            server.shutdown().await;
            result
        };
        let other = sessions["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["session"] != first_session)
            .unwrap()["session"]
            .as_str()
            .unwrap();
        let second = state(&h, id, SessionTarget::Id(other.into())).await;
        assert_eq!(first["resources"], second["resources"]);
        assert_eq!(
            o.lock().unwrap().resolve,
            0,
            "publication and reads must be lazy"
        );
        let server = Server::new(&h, false).await;
        let file = endpoint(&server, &first["resources"]["file"]);
        for _ in 0..2 {
            let response = server.client.get(&file).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.bytes().await.unwrap().as_ref(), FILE);
        }
        assert_eq!(
            o.lock().unwrap().resolve,
            1,
            "successful safe-open must cache"
        );
        let stream = endpoint(&server, &first["resources"]["stream"]);
        for expected in 1..=2 {
            let response = server.client.get(&stream).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.bytes().await.unwrap().as_ref(), STREAM_BYTES);
            count(&o, expected).await;
        }
        assert_eq!(
            o.lock().unwrap().resolve,
            3,
            "closed streams cannot be reused"
        );
        assert_eq!(o.lock().unwrap().read, 2);
        h.host.process_exited(975101);
        let response = server.client.get(&file).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap().as_ref(), FILE);
        h.host.process_exited(975102);
        assert_eq!(
            server.client.get(&file).send().await.unwrap().status(),
            StatusCode::GONE
        );
        server.shutdown().await;
    }));
}

#[test]
fn resources_mutation_typed_gone_http_410_and_new_publication() {
    let id = "test.r5.mutation";
    let (h, o) = setup(id, 975103, None);
    h.runtime.block_on(bounded(async {
        let before = state(&h, id, SessionTarget::Active).await;
        let server = Server::new(&h, false).await;
        let url = endpoint(&server, &before["resources"]["file"]);
        assert_eq!(
            server
                .client
                .get(&url)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            FILE
        );
        let token = before["resources"]["file"]["url"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap();
        let published = h
            .host
            .published_resource(token, vosu_core::resources::ReferenceClass::Resource)
            .unwrap();
        let resolver = vosu_core::resource_io::ResourceResolver::new(&h.host, WAIT);
        let resolved = resolver
            .resolve(published.owner, &published.identity)
            .await
            .unwrap();
        let vosu_core::resource_io::ResolvedResource::File(file) = resolved else {
            panic!("file")
        };
        let changed = b"new-chart-content-975103-not-the-old-chart";
        std::fs::write(h.root.path().join("fixture.osu"), changed).unwrap();
        assert!(matches!(
            file.read(0, changed.len() as u32).await,
            Err(vosu_core::plugin::ResourceRequestError::Gone)
        ));
        let response = server.client.get(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "unpublished_resource"
        );
        let handle = o.lock().unwrap().handles[0];
        publish_ref(&o, id, handle, "/resources/file", "new-generation-975103");
        let after = state(&h, id, SessionTarget::Active).await;
        assert_ne!(before["resources"]["file"], after["resources"]["file"]);
        let response = server
            .client
            .get(endpoint(&server, &after["resources"]["file"]))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.bytes().await.unwrap().as_ref(), changed);
        assert_eq!(
            server.client.get(&url).send().await.unwrap().status(),
            StatusCode::GONE
        );
        assert_eq!(o.lock().unwrap().resolve, 3);
        server.shutdown().await;
    }));
}

#[test]
fn resources_detach_while_resolving_rejects_late_reply() {
    let id = "test.r5.detach";
    let (h, o) = setup(id, 975104, None);
    h.runtime.block_on(bounded(async {
        let state = state(&h, id, SessionTarget::Active).await;
        o.lock().unwrap().hold = true;
        let server = Server::new(&h, false).await;
        let url = endpoint(&server, &state["resources"]["file"]);
        let request = tokio::spawn({
            let client = server.client.clone();
            let url = url.clone();
            async move { client.get(url).send().await.unwrap() }
        });
        bounded(async {
            loop {
                if o.lock().unwrap().held.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        h.host.process_exited(975104);
        let response = bounded(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        let (host, correlation) = {
            let mut o = o.lock().unwrap();
            (o.host.unwrap(), o.held.take().unwrap().1)
        };
        assert_ne!(
            host.send(
                &Message::ResourceResolved(vosu_plugin_api::ResourceResolved::Path {
                    root: "fixture".into(),
                    path: "fixture.osu".into()
                }),
                correlation
            ),
            vosu_status::VOSU_OK
        );
        assert_eq!(
            server.client.get(&url).send().await.unwrap().status(),
            StatusCode::GONE
        );
        assert_eq!(o.lock().unwrap().resolve, 1);
        assert_eq!(o.lock().unwrap().read, 0);
        assert_eq!(o.lock().unwrap().close, 0);
        server.shutdown().await;
    }));
}

fn store(path: &std::path::Path, records: usize) -> Arc<vosu_core::state::ReferenceStore> {
    Arc::new(vosu_core::state::ReferenceStore::new(
        ResourceSecret::open_or_create(path).unwrap(),
        ResourceRegistry::new(RegistryLimits {
            records,
            ..RegistryLimits::default()
        }),
    ))
}
#[test]
fn resources_restart_persistent_secret_and_rotated_secret_http() {
    let secret = tempfile::tempdir().unwrap();
    let path = secret.path().join("resource-secret");
    let id = "test.r5.restart";
    let first = {
        let (h, o) = setup(id, 975105, Some(store(&path, 16)));
        h.runtime.block_on(bounded(async {
            let state = state(&h, id, SessionTarget::Active).await;
            assert_eq!(o.lock().unwrap().resolve, 0);
            let server = Server::new(&h, false).await;
            assert_eq!(
                server
                    .client
                    .get(endpoint(&server, &state["resources"]["file"]))
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap()
                    .as_ref(),
                FILE
            );
            server.shutdown().await;
            state
        }))
    };
    for (pid, secret_path, same) in [
        (975106, path.clone(), true),
        (975107, secret.path().join("other-secret"), false),
    ] {
        let (h, o) = setup(id, pid, Some(store(&secret_path, 16)));
        h.runtime.block_on(bounded(async {
            let current = state(&h, id, SessionTarget::Active).await;
            assert_eq!(current["resources"] == first["resources"], same);
            assert_eq!(o.lock().unwrap().resolve, 0);
            let server = Server::new(&h, false).await;
            let response = server
                .client
                .get(endpoint(&server, &first["resources"]["file"]))
                .send()
                .await
                .unwrap();
            if same {
                assert_eq!(response.status(), StatusCode::OK);
                assert_eq!(response.bytes().await.unwrap().as_ref(), FILE);
                assert_eq!(o.lock().unwrap().resolve, 1);
            } else {
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
                assert_eq!(o.lock().unwrap().resolve, 0);
            }
            server.shutdown().await;
        }));
    }
}

#[test]
fn resources_eviction_of_current_reference_is_410_not_resolver_access() {
    let secret = tempfile::tempdir().unwrap();
    let id = "test.r5.eviction";
    let resources = store(&secret.path().join("secret"), 3);
    let (h, o) = setup(id, 975108, Some(resources.clone()));
    h.runtime.block_on(bounded(async {
        let initial = state(&h, id, SessionTarget::Active).await;
        let handle = o.lock().unwrap().handles[0];
        let server = Server::new(&h, false).await;
        let old = endpoint(&server, &initial["resources"]["file"]);
        assert_eq!(
            server
                .client
                .get(&old)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            FILE
        );
        publish_ref(&o, id, handle, "/resources/additional", "evict-975108");
        let stats = resources.registry.lock().unwrap().stats();
        assert_eq!(stats.records, 3);
        assert_eq!(stats.evictions, 1);
        let response = server.client.get(&old).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(
            o.lock().unwrap().resolve,
            1,
            "an evicted cached file must not be reopened"
        );
        let current = state(&h, id, SessionTarget::Active).await;
        assert_eq!(
            server
                .client
                .get(endpoint(&server, &current["resources"]["additional"]))
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            FILE
        );
        assert_eq!(o.lock().unwrap().resolve, 2);
        server.shutdown().await;
    }));
}

#[test]
fn resources_failed_open_retries_and_rejected_batch_leaves_no_membership() {
    let id = "test.r5.retry";
    let (h, o) = setup(id, 975109, None);
    h.runtime.block_on(bounded(async {
        let initial = state(&h, id, SessionTarget::Active).await;
        let server = Server::new(&h, false).await;
        let url = endpoint(&server, &initial["resources"]["file"]);
        std::fs::remove_file(h.root.path().join("fixture.osu")).unwrap();
        assert_eq!(
            server.client.get(&url).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(o.lock().unwrap().resolve, 1);
        std::fs::write(h.root.path().join("fixture.osu"), FILE).unwrap();
        for _ in 0..2 {
            let response = server.client.get(&url).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.bytes().await.unwrap().as_ref(), FILE);
        }
        assert_eq!(o.lock().unwrap().resolve, 2);
        let (host, handle) = {
            let o = o.lock().unwrap();
            (o.host.unwrap(), o.handles[0])
        };
        let reference = ResourceRef {
            kind: "fixture-file".into(),
            key: CborValue::Text("file".into()),
            name: "fixture.osu".into(),
            content_id: CborValue::Text("rejected-generation-975109".into()),
        };
        let resources = &h.host.state().store.resources;
        let identity = vosu_core::resources::ReferenceIdentity::new(
            &resources.secret,
            id,
            vosu_core::resources::ReferenceClass::Resource,
            reference.clone(),
        )
        .unwrap();
        let stats = resources.registry.lock().unwrap().stats();
        let mut batch = PublishBatchWriter::new();
        batch.begin(handle);
        let encoded = encode(&reference).unwrap();
        batch.push(
            id,
            "/resources/rejected",
            PublishValueRef::ResourceRef(&encoded),
        );
        batch.push("test.r5.ungranted", "/value", PublishValueRef::I64(975109));
        let mut bytes = Vec::new();
        batch.finish_into(&mut bytes).unwrap();
        assert_ne!(
            host.send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes),
            vosu_status::VOSU_OK
        );
        assert_eq!(resources.registry.lock().unwrap().stats(), stats);
        assert_eq!(state(&h, id, SessionTarget::Active).await, initial);
        assert!(matches!(
            h.host.published_resource(
                identity.id.as_str(),
                vosu_core::resources::ReferenceClass::Resource
            ),
            Err(vosu_core::resources::RegistryError::Gone),
        ));
        let response = server
            .client
            .get(format!(
                "{}/resources/{}",
                server.base,
                identity.id.as_str()
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(o.lock().unwrap().resolve, 2);
        server.shutdown().await;
    }));
}

#[path = "streams.rs"]
mod streams;

//! Shared production-host fixtures for benchmarks and cost invariants.
#![expect(
    dead_code,
    reason = "Each benchmark/test consumes a focused subset of the fixture"
)]
// One copy of the harness per crate: the including root declares
// `mod adapter_fixture` (tests/adapter_fixture), which owns it.
pub use crate::adapter_fixture::host as harness;
use harness::*;
use vosu_core::api::{ApiPlan, SessionTarget};
use vosu_plugin_api::PublishValueRef;

pub const ID: &str = "test.x1.telemetry";
pub struct Telemetry {
    pub h: Harness,
    pub handle: u64,
}
impl Telemetry {
    pub fn new(id: &str, pid: u32) -> Self {
        let mut h = Harness::new();
        let (handle, _) = h.publisher(id, pid);
        publish(id, handle, &[("/score", PublishValueRef::I64(913))]);
        Self { h, handle }
    }
    pub fn publish(&self, id: &str, n: i64) {
        publish(id, self.handle, &[("/score", PublishValueRef::I64(n))]);
    }
    pub fn plan(id: &str) -> ApiPlan {
        ApiPlan::new(&[format!("/{id}/score")]).unwrap()
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "Disposable cold-resource fixture, not user configuration or state"
)]
pub fn resource_host(id: &str, pid: u32) -> Harness {
    let mut h = Harness::new();
    std::fs::write(
        h.root.path().join("fixture.osu"),
        include_bytes!("../../tests/fixtures/api-v1-plugin/fixture.osu"),
    )
    .unwrap();
    let manifest = format!(
        "{}\nroot_slots=['fixture']\n[match]\nprocess_names=['{id}']\n[[provides]]\nid='field:{id}'\nversion='1.0.0'\nschema='namespace.json'\n[[resource_kinds]]\nkind='fixture-file'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-stream'\nfamilies=['chart-text']\n[[resource_kinds]]\nkind='fixture-tree'\ntree=true\nfamilies=['chart-text']\n",
        manifest(id)
    );
    let schema =
        format!(r#"{{"title":"{id}","version":"1.0.0","type":"object","properties":{{}}}}"#);
    h.load(&manifest, &[&schema], &[], || {
        vosu_plugin_api::__vtable_for_serialized::<vosu_plugin_testkit::fixture::ApiFixturePlugin>()
    });
    h.host.process_appeared(record(pid, id));
    h
}
pub fn resource_urls(h: &Harness, id: &str) -> serde_json::Value {
    h.runtime.block_on(bounded(async {
        loop {
            if let Ok(update) = h.host.api_state_source().read(
                &SessionTarget::Active,
                &ApiPlan::new(&[format!("/{id}")]).unwrap(),
            ) && let Some(Some(value)) = update.values.first()
            {
                let mut bytes = Vec::new();
                value.write_json(&mut bytes);
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if value["resources"]["tree"]["url"].is_string() {
                    return value;
                }
            }
            tokio::task::yield_now().await;
        }
    }))
}

pub struct References {
    pub store: vosu_core::state::StateStore,
    grant: vosu_core::state::PublisherGrant,
    pub a: Vec<u8>,
    pub b: Vec<u8>,
}
impl References {
    pub fn new() -> Self {
        use std::sync::{Arc, RwLock};
        use vosu_core::state::*;
        use vosu_plugin_api::*;
        let text = format!(
            "{}\ncovers=['field:vosu.chart']\n[[provides]]\nid='field:test.x1.reference'\nversion='1.0.0'\nschema='state.json'\n[[resource_kinds]]\nkind='audio'\nfamilies=['audio']\n",
            manifest("test.x1.reference")
        );
        let grant = PublisherGrant::from_manifest(
            &vosu_plugin_api::manifest::PackageManifest::parse(&text)
                .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
                .unwrap(),
        )
        .unwrap();
        let store = StateStore::new(Arc::new(RwLock::new(SchemaRegistry::with_core().unwrap())));
        store.open_session(
            SessionKey(99131),
            &grant,
            ClientFacts {
                game: "osu".into(),
                variant: "stable".into(),
                name: "X1".into(),
                version: "1".into(),
                pid: 99131,
                session: "x1".into(),
                plugin_id: "test.x1.reference".into(),
                plugin_version: "1.0.0".into(),
            },
            true,
        );
        let reference = |content: &str| {
            encode(&ResourceRef {
                kind: "audio".into(),
                key: CborValue::Text("track-17".into()),
                name: "track.mp3".into(),
                content_id: CborValue::Text(content.into()),
            })
            .unwrap()
        };
        Self {
            store,
            grant,
            a: reference("content-29"),
            b: reference("content-31"),
        }
    }
    pub fn commit(&self, changed: bool) {
        use vosu_plugin_api::{PublishFieldRef, PublishValueRef};
        self.store
            .commit_from_instance(
                &self.grant,
                vosu_core::state::SessionKey(99131),
                99132,
                [PublishFieldRef {
                    handle: 99131,
                    ns: "vosu.chart",
                    pointer: "/resources/audio",
                    value: PublishValueRef::ResourceRef(if changed { &self.b } else { &self.a }),
                }]
                .into_iter(),
            )
            .unwrap();
    }
}

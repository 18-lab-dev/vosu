//! Immutable oracle chart bytes use real ROOT_DECLARE and lazy resource resolution.
//! The original path echoes remain source facts, never root authority.
use vosu_plugin_api::{
    CborValue, HostServices, Message, PublishBatchWriter, PublishValueRef, ResourceRef,
    ResourceResolved, RootDeclare, VtableHostServices, vosu_opcode, vosu_status,
};

pub(super) struct Chart {
    root: String,
    slot: &'static str,
}
impl Chart {
    #[expect(clippy::disallowed_methods, reason = "Disposable immutable oracle chart fixture, never user configuration or state") ]
    pub(super) fn new(root: &std::path::Path, content: &str, lazer: bool) -> Self {
        std::fs::write(root.join("oracle-chart.osu"), content).unwrap();
        Self {
            root: root.to_string_lossy().into_owned(),
            slot: if lazer { "files" } else { "songs" },
        }
    }
    pub(super) fn attach(&self, host: &VtableHostServices, handle: u64) {
        assert_eq!(
            host.send(
                &Message::RootDeclare(RootDeclare {
                    handle: Some(handle),
                    name: self.slot.into(),
                    path: self.root.clone()
                }),
                0
            ),
            vosu_status::VOSU_OK
        );
        let reference = ResourceRef {
            kind: "file".into(),
            key: CborValue::Text("oracle-chart".into()),
            name: "oracle-chart.osu".into(),
            content_id: CborValue::Text("immutable-oracle-chart-1973".into()),
        };
        let encoded = vosu_plugin_api::encode(&reference).unwrap();
        let mut writer = PublishBatchWriter::new();
        writer.begin(handle);
        writer.push(
            "vosu.chart",
            "/resources/file",
            PublishValueRef::ResourceRef(&encoded),
        );
        let mut bytes = Vec::new();
        writer.finish_into(&mut bytes).unwrap();
        let message = Message::decode(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, &bytes).unwrap();
        assert_eq!(host.send(&message, 0), vosu_status::VOSU_OK);
    }
    pub(super) fn resolve(
        &self,
        host: &VtableHostServices,
        message: &Message,
        correlation: u64,
    ) -> bool {
        if let Message::ResourceResolve(request) = message
            && request.kind == "file"
            && request.key == CborValue::Text("oracle-chart".into())
        {
            assert_eq!(
                host.send(
                    &Message::ResourceResolved(ResourceResolved::Path {
                        root: self.slot.into(),
                        path: "oracle-chart.osu".into()
                    }),
                    correlation
                ),
                vosu_status::VOSU_OK
            );
            true
        } else {
            false
        }
    }
}

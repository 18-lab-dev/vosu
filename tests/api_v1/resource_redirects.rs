use super::support::*;
use axum::http::{StatusCode, header};
use vosu_core::api::{ApiPlan, SessionTarget};
use vosu_plugin_api::{
    CborValue, HostServices, PublishBatchWriter, PublishValueRef, ResourceRef, encode, vosu_opcode,
    vosu_status,
};
use vosu_plugin_testkit::publisher::{self, PublisherEvent};

#[test]
fn resources_convenience_redirect_only_published_urls_and_encode_once() {
    let id = "test.r4.redirect";
    let mut h = Harness::new();
    let manifest = format!(
        "{}\n[match]\nprocess_names=['{id}']\n[[resource_kinds]]\nkind='audio'\nfamilies=['audio']\n[[resource_kinds]]\nkind='skin'\ntree=true\nfamilies=['image']\n[[provides]]\nid='game:fixture'\nversion='*'\n",
        manifest(id)
    );
    publisher::register_manifest(id, &manifest);
    let events = publisher::register(id);
    h.load(&manifest, &[], &[], publisher::vosu_plugin_vtable);
    h.host.process_appeared(record(974015, id));
    let deadline = std::time::Instant::now() + WAIT;
    let handle = loop {
        if let PublisherEvent::Attached { handle } = events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            break handle;
        }
    };
    let reference = |kind: &str, name: &str| {
        encode(&ResourceRef {
            kind: kind.into(),
            key: CborValue::Integer(87.into()),
            name: name.into(),
            content_id: CborValue::Text("cold-identity-9931".into()),
        })
        .unwrap()
    };
    let audio = reference("audio", "track.ogg");
    let skin = reference("skin", "skin");
    let mut batch = PublishBatchWriter::new();
    batch.begin(handle);
    batch.push(
        "vosu.chart",
        "/resources/audio",
        PublishValueRef::ResourceRef(&audio),
    );
    batch.push("vosu.skin", "/tree", PublishValueRef::TreeRef(&skin));
    let mut bytes = Vec::new();
    batch.finish_into(&mut bytes).unwrap();
    assert_eq!(
        publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes),
        vosu_status::VOSU_OK
    );
    h.runtime.block_on(bounded(async {
        let plan =
            ApiPlan::new(&["/vosu.chart/resources/audio/url", "/vosu.skin/tree/url"]).unwrap();
        let state = h
            .host
            .api_state_source()
            .read(&SessionTarget::Active, &plan)
            .unwrap();
        let urls: Vec<_> = state
            .values
            .iter()
            .map(|value| {
                let Some(vosu_core::state::StateValue::String(value)) = value else {
                    panic!("published URL absent")
                };
                value.to_string()
            })
            .collect();
        let server = Server::new(&h, false).await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(WAIT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        for (path, expected) in [
            ("/sessions/active/chart/audio", urls[0].clone()),
            (
                "/sessions/active/skin/a%20b/%252e.png",
                format!("{}a%20b/%252e.png", urls[1]),
            ),
        ] {
            let response = client
                .get(format!("{}{path}", server.base))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(response.headers()[header::LOCATION], expected);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let response = client
            .get(format!("{}/sessions/active/skin/a%2Fb.png", server.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = client
            .get(format!("{}/sessions/active/chart/background", server.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        server.shutdown().await;
    }));
}

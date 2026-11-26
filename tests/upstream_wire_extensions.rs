//! A real producer/ABI/core/surface fixture preserves facts while v2 omits extensions.
#[expect(
    dead_code,
    reason = "Shared source-oracle helpers also serve the full parity inventory"
)]
#[path = "state_api_compat/oracle.rs"]
mod oracle;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use vosu::server::service::{RegisteredProvider, SurfaceRouter};
use vosu_core::plugin::PluginInstanceId;
use vosu_core::server::{
    routes::StateRoute,
    websocket::{Outbound, Schema, WebSocketHub},
};
use vosu_osu_model::{
    launch::InstanceSettings,
    memory::{ClientRenderState, PlaybackClock},
};
use vosu_plugin_api::Selector;
use vosu_surface::{
    listener::ListenerRole,
    routes::{RouteDescriptor, RoutePath},
    state::SessionTarget,
};

fn no_extensions(bytes: &[u8]) {
    let value: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(value["play"]["score"], 0);
    assert!(value["beatmap"]["time"]["live"].is_number());
    for pointer in [
        "/beatmap/time/rate",
        "/beatmap/time/running",
        "/play/source",
    ] {
        assert!(value.pointer(pointer).is_none(), "{pointer}");
    }
}
#[test]
fn v2_http_and_websocket_frames_omit_extensions_but_bus_facts_are_unchanged() {
    let input: oracle::Input = serde_json::from_slice(
        &std::fs::read(oracle::fixture_root().join("stable-defaults/input.json")).unwrap(),
    )
    .unwrap();
    let mut replay = oracle::replay(&input);
    let mut source = (*replay.instances[0].state).clone();
    let regular = Arc::make_mut(&mut source.regular);
    regular.global.render = Some(ClientRenderState {
        spectating: true,
        clock: Some(PlaybackClock {
            rate: 1.37,
            running: false,
        }),
    });
    Arc::make_mut(&mut source.precise).status = 2;
    let publisher = replay.fixture.attach(
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        false,
        true,
    );
    publisher
        .publish(Arc::new(source), vosu_osu_state::Delivery::All)
        .unwrap();
    let requester = replay
        .fixture
        .h
        .host
        .surface_requester(PluginInstanceId::new(2))
        .unwrap();
    let facts = requester
        .capture(
            SessionTarget::Focused,
            &[
                Selector {
                    ns: "vosu.osu".into(),
                    pointer: "/beatmap/time".into(),
                },
                Selector {
                    ns: "vosu.osu".into(),
                    pointer: "/play/source".into(),
                },
            ],
        )
        .unwrap();
    let value = |ns: &str, pointer: &str| -> Value {
        facts
            .values
            .iter()
            .find(|e| e.ns == ns && e.pointer == pointer)
            .unwrap()
            .value
            .as_ref()
            .unwrap()
            .deserialized()
            .unwrap()
    };
    assert_eq!(value("vosu.osu", "/beatmap/time")["rate"], json!(1.37));
    assert_eq!(value("vosu.osu", "/beatmap/time")["running"], false);
    assert_eq!(value("vosu.osu", "/play/source"), "spectator");
    // Common rate is not an upstream adapter grant. Inspect it as the host,
    // rather than broadening that adapter's authority just for this test.
    let selected = facts.session.as_ref().unwrap();
    let diagnostic = replay
        .fixture
        .h
        .host
        .diagnostic_sessions()
        .into_iter()
        .find(|session| session.session == selected.session)
        .unwrap();
    let state = replay
        .fixture
        .h
        .host
        .state()
        .store
        .current(vosu_core::state::SessionKey(diagnostic.handle))
        .unwrap();
    let mut bytes = Vec::new();
    state
        .get(
            "vosu.play",
            &vosu_core::state::Pointer::parse("/rate").unwrap(),
        )
        .unwrap()
        .write_json(&mut bytes);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap(),
        json!(1.37)
    );
    let provider = replay.fixture.providers[2].clone();
    let _routes = requester
        .register_routes(vec![(
            RouteDescriptor {
                listener: ListenerRole::Overlay,
                method: "GET".into(),
                path: RoutePath::Exact("/json/v2".into()),
                websocket: false,
            },
            Arc::new(RegisteredProvider::State {
                provider: provider.clone(),
                route: StateRoute::V2,
            }),
        )])
        .unwrap();
    let router = SurfaceRouter::new(replay.fixture.h.host.clone(), requester);
    let request = Request::builder()
        .uri("/json/v2")
        .body(Body::empty())
        .unwrap();
    replay.fixture.h.runtime.block_on(async {
        let response = router.http(&request).await;
        assert_eq!(response.status(), 200);
        let http = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        no_extensions(&http);
        let hub = WebSocketHub::new(Schema::V2, provider);
        let (connection, mut receiver) = hub.connect();
        let run = tokio::spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        let frame = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let Outbound::Text(bytes) = frame else {
            panic!("expected a real v2 state frame");
        };
        no_extensions(&bytes);
        assert_eq!(bytes, http);
        drop(connection);
        hub.shutdown();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap();
    });
}

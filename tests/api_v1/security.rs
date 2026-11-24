use super::support::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use vosu_core::state::{
    Audience, Pointer, SessionKey, project_document, project_namespace, project_select,
};
use vosu_plugin_api::{PublishValueRef, Selector};
use vosu_plugin_testkit::subscriber::{self, SubscribeCommand, SubscriberEvent};

#[test]
fn authorized_plugin_sees_private_paths_but_public_reads_caches_and_frames_never_do() {
    let mut h = Harness::new();
    let ns = "vosu.osu";
    let (handle, _) = h.publisher(ns, 894_030);
    publish(
        ns,
        handle,
        &[
            (
                "/internal/paths/stable/install",
                PublishValueRef::String(SENTINEL),
            ),
            ("/public", PublishValueRef::String("public-8914")),
        ],
    );
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        for path in [
            "/sessions",
            "/sessions/active",
            "/sessions/active/state",
            "/sessions/active/state/vosu.osu",
            "/sessions/active/state?pointer=%2Fvosu.osu",
            "/sessions/active/state?pointer=%2Fvosu.osu%2Finternal",
            "/sessions/active/state/vosu.osu?pointer=%2Finternal%2Fpaths%2Fstable%2Finstall",
        ] {
            let (status, value) = server.get(path).await;
            assert_eq!(status, 200, "{path}");
            clean(&value);
            if path.contains("internal") {
                assert_eq!(value, json!({"value": null}));
            }
        }
        assert!(
            h.host
                .state()
                .subscriptions
                .lock()
                .unwrap()
                .demand(SessionKey(handle), Some("play"))
                .selectors
                .is_empty(),
            "all HTTP reads are demand-free"
        );
        for (epoch, pointers) in [
            vec![""],
            vec!["/vosu.osu"],
            vec!["/vosu.osu/internal/paths/stable/install"],
        ]
        .into_iter()
        .enumerate()
        {
            let mut socket = server.ws("/sessions/active/subscribe").await;
            let initial = select(&mut socket, &pointers).await;
            clean(&initial);
            if pointers[0].contains("internal") {
                assert_eq!(initial[2], json!([]));
                publish(
                    ns,
                    handle,
                    &[(
                        "/internal/paths/lazer/install",
                        PublishValueRef::String(SENTINEL),
                    )],
                );
                socket
                    .send(Message::Ping(bytes::Bytes::from_static(b"privacy-fence")))
                    .await
                    .unwrap();
                assert!(matches!(
                    bounded(socket.next()).await.unwrap().unwrap(),
                    Message::Pong(_)
                ));
            } else {
                assert!(initial.to_string().contains("public-8914"));
                publish(
                    ns,
                    handle,
                    &[
                        ("/epoch", PublishValueRef::U64(epoch as u64 + 1)),
                        (
                            "/internal/paths/lazer/install",
                            PublishValueRef::String(SENTINEL),
                        ),
                    ],
                );
                let update = frame(&mut socket).await;
                assert_eq!(update[0], 1);
                assert!(update.to_string().contains("public-8914"));
                clean(&update);
            }
            socket.close(None).await.unwrap();
        }
        let mut events = server.ws("/sessions/subscribe").await;
        let initial = select(&mut events, &[""]).await;
        clean(&initial);
        assert!(initial.to_string().contains("894030"));
        drop(events);
        server.shutdown().await;
    }));
    // The privacy assertion has a positive control through a real authorized
    // subscriber ABI, not merely a private Rust storage object.
    let reader = "test.s4.private.reader";
    let (events, commands) = subscriber::register(reader);
    h.load(
        &format!(
            "{}\n[[requires]]\nid='field:vosu.osu.internal.paths.stable'\nversion='*'\n",
            manifest(reader)
        ),
        &[],
        &[],
        subscriber::vosu_plugin_vtable,
    );
    commands
        .try_send(SubscribeCommand {
            session: None,
            selectors: vec![Selector {
                ns: "vosu.sessions".into(),
                pointer: String::new(),
            }],
        })
        .unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        match events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            SubscriberEvent::Attached(attached) => {
                assert_eq!(attached.handle, handle);
                break;
            }
            SubscriberEvent::SubscribeStatus(status) => {
                assert_eq!(status, vosu_plugin_api::vosu_status::VOSU_OK)
            }
            _ => {}
        }
    }
    commands
        .try_send(SubscribeCommand {
            session: Some(handle),
            selectors: vec![Selector {
                ns: ns.into(),
                pointer: "/internal/paths/stable/install".into(),
            }],
        })
        .unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        if let SubscriberEvent::Update { values, .. } = events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            assert_eq!(
                serde_json::to_value(values[0].value.as_ref().unwrap()).unwrap(),
                SENTINEL
            );
            break;
        }
    }
    let state = h.host.state().store.current(SessionKey(handle)).unwrap();
    let registry = h.host.state().registry.read().unwrap();
    assert!(format!("{:?}", state.namespace(ns).unwrap()).contains(SENTINEL));
    clean(project_document(&registry, &state, Audience::Api));
    clean(project_namespace(
        &registry,
        ns,
        state.namespace(ns).unwrap(),
        Audience::Api,
    ));
    for path in ["", "/internal", "/internal/paths/stable/install"] {
        let value = project_select(
            &registry,
            &state,
            ns,
            &Pointer::parse(path).unwrap(),
            Audience::Api,
        );
        clean(&value);
        if !path.is_empty() {
            assert!(value.is_none());
        }
    }
    for _ in 0..2 {
        let cached = state.api_json(&registry);
        assert!(!String::from_utf8_lossy(cached).contains(SENTINEL));
        assert!(String::from_utf8_lossy(cached).contains("public-8914"));
    }
}

#[test]
fn dashboard_origin_peer_policy_and_upgrade_allowlist_fail_closed() {
    let mut h = Harness::new();
    h.publisher("test.s4.security", 894_031);
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        for path in [
            "/sessions",
            "/sessions/active/state",
            "/sessions/active/subscribe",
            "/sessions/subscribe",
        ] {
            let response = server
                .client
                .get(format!("{}{path}", server.base))
                .header("Origin", "https://evil.invalid")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 403);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"]["code"],
                "forbidden"
            );
            let response = server
                .client
                .get(format!("{}{path}", server.base))
                .header("Origin", "https://evil.invalid")
                .header("Authorization", "Bearer not-a-read-grant")
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                403,
                "credentials cannot widen read authorization"
            );
        }
        for path in [
            "/sessions",
            "/sessions/active",
            "/sessions/active/state",
            "/sessions/active/subscribe/precise",
            "/sessions/subscribe/",
        ] {
            let result = bounded(tokio_tungstenite::connect_async(format!(
                "{}{path}",
                server.base.replacen("http:", "ws:", 1)
            )))
            .await;
            let tokio_tungstenite::tungstenite::Error::Http(response) = result.unwrap_err() else {
                panic!("expected HTTP rejection");
            };
            assert_eq!(response.status(), 404, "{path}");
        }
        let dashboard = Server::new(&h, true).await;
        for path in [
            "/sessions",
            "/sessions/active",
            "/sessions/active/state",
            "/sessions/active/state/test.s4.security",
            "/sessions/subscribe",
            "/sessions/active/subscribe",
        ] {
            let (status, body) = dashboard.get(path).await;
            assert_eq!(status, 404, "dashboard must never serve {path}");
            assert_eq!(body["error"]["code"], "invalid_request");
        }
        dashboard.shutdown().await;
        server.shutdown().await;
    }));
}

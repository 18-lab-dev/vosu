use super::*;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use vosu_proptest::bounded;

#[tokio::test]
async fn api_v1_http_and_session_events_use_real_transport() {
    bounded!(async {
        let root = tempfile::tempdir().unwrap();
        let config = Arc::new(
            vosu_core::config::ConfigStore::initialize(root.path().join("vosu.toml"))
                .await
                .unwrap(),
        );
        let logger = vosu_core::logging::Logger::new(
            root.path(),
            vosu_core::logging::LoggerOptions::default(),
            Arc::new(vosu_core::logging::StdoutConsole),
        )
        .unwrap();
        let settings = Arc::new(vosu_core::plugin::settings::ConfigSettingsBridgeB2::new(
            config.clone(),
        ));
        let host = vosu_core::plugin::PluginHost::new(
            logger.clone(),
            Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
                settings,
            )),
            tokio::runtime::Handle::current(),
        );
        let source = host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
        let api = ApiV1Router::new(source, hub.clone());
        let router =
            axum::Router::new()
                .fallback(
                    |axum::extract::State(api): axum::extract::State<ApiV1Router>,
                     request: Request| async move { api.handle(request).await },
                )
                .with_state(api)
                .layer(axum::Extension(Arc::new(UpgradeCallbackTracker::new())));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let stopped = stop.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let base = format!("http://{address}/api/v1");
        let response = client.get(format!("{base}/sessions")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            serde_json::json!({"sessions": []})
        );
        for method in [Method::POST, Method::HEAD, Method::DELETE] {
            let response = client
                .request(method.clone(), format!("{base}/sessions"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method}"
            );
            assert_eq!(response.headers()[header::ALLOW], "GET");
            if method != Method::HEAD {
                assert_eq!(
                    response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
                    "method_not_allowed"
                );
            }
        }
        let response = client
            .get(format!("{base}/sessions/active/state?pointer=bad"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
            "invalid_pointer"
        );
        let response = client
            .get(format!("{base}/sessions/active"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
            "unknown_session"
        );
        let active = tokio_tungstenite::connect_async(format!(
            "ws://{address}/api/v1/sessions/active/subscribe"
        ))
        .await
        .unwrap_err();
        let tokio_tungstenite::tungstenite::Error::Http(response) = active else {
            panic!("expected HTTP rejection");
        };
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://{address}/api/v1/sessions/subscribe"))
                .await
                .unwrap();
        use tokio_tungstenite::tungstenite::Message;
        socket.send(Message::Text("[\"\"]".into())).await.unwrap();
        let frame = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(frame.to_text().unwrap()).unwrap(),
            serde_json::json!([0, 1, [[0, {}]]])
        );
        socket
            .send(Message::Ping(bytes::Bytes::from_static(b"api-ping")))
            .await
            .unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap(),
            Message::Pong(bytes::Bytes::from_static(b"api-ping"))
        );
        socket.send(Message::Text("[\"\"]".into())).await.unwrap();
        let frame = loop {
            let frame = socket.next().await.unwrap().unwrap();
            // Axum's transport can also flush its automatic control Pong.
            if frame.is_pong() {
                continue;
            }
            break frame;
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(frame.to_text().unwrap()).unwrap()[1],
            5
        );
        let Message::Close(Some(close)) = socket.next().await.unwrap().unwrap() else {
            panic!("expected policy close");
        };
        assert_eq!(u16::from(close.code), 1008);
        hub.shutdown();
        drop(socket);
        stop.cancel();
        server.await.unwrap();
        host.destroy_all(Duration::ZERO);
        config.shutdown().await.unwrap();
        logger.shutdown().await.unwrap();
    });
}

use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn slow_plugin_destruction_preserves_legacy_websocket_going_away() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = tempfile::tempdir().unwrap();
        let (mut owner, _) = owner(root.path()).await;
        let overlay = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dashboard = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let overlay_port = overlay.local_addr().unwrap().port();
        let dashboard_port = dashboard.local_addr().unwrap().port();
        owner.config.update(|config| {
            config.server_ip = "127.0.0.1".into();
            config.dashboard_ip = "127.0.0.1".into();
            config.server_port = f64::from(overlay_port);
            config.dashboard_port = f64::from(dashboard_port);
        });
        drop((overlay, dashboard));
        owner.compose().await.unwrap();
        let (mut socket, response) = tokio_tungstenite::connect_async(format!(
            "ws://127.0.0.1:{overlay_port}/websocket/commands"
        ))
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SWITCHING_PROTOCOLS
        );
        while owner.hubs.lock().await.as_ref().unwrap().v1.client_count() == 0 {
            tokio::task::yield_now().await;
        }
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let plugins = Arc::clone(&owner.plugins);
        let shutdown = tokio::spawn(async move {
            owner
                .shutdown_with_plugin_destroy(async move {
                    let _ = entered_tx.send(());
                    let _ = tokio::time::timeout(Duration::from_secs(10), release_rx).await;
                    tokio::task::spawn_blocking(move || {
                        plugins.destroy_all(dev_plugins::PLUGIN_DESTROY_GRACE)
                    })
                    .await
                    .map_err(io::Error::other)
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        // Exercise a destruction phase longer than the listener's five-second
        // drain budget. A prematurely started drain produces an abnormal EOF.
        assert!(
            tokio::time::timeout(Duration::from_secs(6), socket.next())
                .await
                .is_err(),
            "listener drain must not start while plugin destruction is held"
        );
        release_tx.send(()).unwrap();
        let Message::Close(Some(close)) = socket.next().await.unwrap().unwrap() else {
            panic!("expected the legacy hub's Going Away frame");
        };
        assert_eq!(u16::from(close.code), 1001);
        let _ = socket.flush().await;
        drop(socket);
        shutdown.await.unwrap();
    })
    .await
    .expect("shutdown regression test exceeded its bound");
}

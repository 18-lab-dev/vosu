use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn slow_plugin_destruction_preserves_legacy_websocket_going_away() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let root = tempfile::tempdir().unwrap();
        let mut owner = TestDaemon::new(root.path(), &BUNDLE).await;
        let (overlay_port, dashboard_port) = isolated_ports(&mut owner).await;
        let probes = owner.join_probes().await;
        assert_eq!(
            probes
                .iter()
                .filter(|(kind, _)| *kind == "legacy hub")
                .count(),
            4
        );
        assert!(probes.iter().any(|(kind, _)| *kind == "settings bridge"));
        let subscriptions = owner.subscriptions();
        let (mut api, response) = tokio_tungstenite::connect_async(format!(
            "ws://127.0.0.1:{overlay_port}/api/v1/sessions/subscribe"
        ))
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SWITCHING_PROTOCOLS
        );
        api.send(Message::Text(r#"[""]"#.into())).await.unwrap();
        assert!(matches!(api.next().await, Some(Ok(Message::Text(_)))));
        assert_eq!(subscriptions.active_tasks(), 1);
        let (mut socket, response) = tokio_tungstenite::connect_async(format!(
            "ws://127.0.0.1:{overlay_port}/websocket/commands"
        ))
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SWITCHING_PROTOCOLS
        );
        let hub = owner.commands_hub().await.unwrap();
        while hub.client_count() == 0 {
            tokio::task::yield_now().await;
        }
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let plugins = Arc::clone(owner.plugins());
        let mut shutdown = tokio::spawn(async move {
            owner
                .shutdown_with_plugin_destroy(async move {
                    let _ = entered_tx.send(());
                    // The listener and close-frame probes run before the
                    // assertion's six-second wait. Do not let a second timed
                    // wait release this gate while those probes are in flight.
                    // On failure the sender drops, still allowing cleanup.
                    let _ = release_rx.await;
                    tokio::task::spawn_blocking(move || plugins.destroy_all(PLUGIN_DESTROY_GRACE))
                        .await
                        .map_err(io::Error::other)
                })
                .await
                .unwrap();
        });
        entered_rx.await.unwrap();
        assert_eq!(hub.client_count(), 0);
        assert_eq!(
            subscriptions.active_tasks(),
            0,
            "subscription worker joined before plugin destruction"
        );
        for (kind, probe) in probes {
            assert!(
                probe.is_finished(),
                "{kind} was not joined before plugin destruction"
            );
        }
        for port in [overlay_port, dashboard_port] {
            assert!(
                tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .is_err(),
                "listener still open: {port}"
            );
        }
        let Message::Close(Some(close)) = api.next().await.unwrap().unwrap() else {
            panic!("expected the API subscription's Going Away frame");
        };
        assert_eq!(u16::from(close.code), 1001);
        // Registered feeds are closed and listener callbacks joined before
        // plugin destruction, so no provider callback can outlive its owner.
        let Message::Close(Some(close)) = socket.next().await.unwrap().unwrap() else {
            panic!("expected the legacy hub's Going Away frame");
        };
        assert_eq!(u16::from(close.code), 1001);
        let _ = socket.flush().await;
        drop(socket);
        // A destroy phase longer than the listener drain budget remains joined
        // without delaying or replacing the already-delivered Going Away frame.
        assert!(
            tokio::time::timeout(Duration::from_secs(6), &mut shutdown)
                .await
                .is_err(),
            "shutdown must join held plugin destruction after draining listeners"
        );
        release_tx.send(()).unwrap();
        shutdown.await.unwrap();
    })
    .await
    .expect("shutdown regression test exceeded its bound");
}

async fn isolated_ports(owner: &mut TestDaemon) -> (u16, u16) {
    let overlay = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dashboard = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let overlay_port = overlay.local_addr().unwrap().port();
    let dashboard_port = dashboard.local_addr().unwrap().port();
    owner.config().update(|config| {
        config.server_ip = "127.0.0.1".into();
        config.dashboard_ip = "127.0.0.1".into();
        config.server_port = f64::from(overlay_port);
        config.dashboard_port = f64::from(dashboard_port);
    });
    drop((overlay, dashboard));
    owner.compose(Arc::new(NoProcesses)).await.unwrap();
    (overlay_port, dashboard_port)
}

#[tokio::test]
async fn static_root_reload_creates_target_and_retains_previous_root_on_failure() {
    vosu_proptest::bounded!(async {
        let root = tempfile::tempdir().unwrap();
        let mut owner = TestDaemon::new(root.path(), &BUNDLE).await;
        isolated_ports(&mut owner).await;
        let active = root.path().join("next/counters");
        owner
            .config()
            .update(|config| config.static_folder_path = active.to_str().unwrap().into());
        assert_eq!(owner.current_overlay_root().await.as_ref(), Some(&active));
        assert!(active.is_dir());
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, "not a directory").unwrap();
        owner.config().update(|config| {
            config.static_folder_path = blocked.join("counters").to_str().unwrap().into()
        });
        owner.logger().flush().await.unwrap();
        assert!(owner.output().contains("retaining previous counter root"));
        assert_eq!(owner.current_overlay_root().await.as_ref(), Some(&active));
        owner.shutdown().await.unwrap();
    });
}

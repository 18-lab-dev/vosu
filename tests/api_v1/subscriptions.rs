use super::support::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Arc;
use vosu_core::api::SessionTarget;
use vosu_core::server::subscription_wire::Outbound;
use vosu_plugin_api::PublishValueRef;

#[test]
fn invalid_requests_immutable_selection_ping_and_timeout_cross_transport() {
    let mut h = Harness::new();
    h.publisher("test.s4.protocol", 894_020);
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        for (request, code) in [
            ("{}".to_owned(), 1),
            ("[]".into(), 1),
            ("[\"not-a-pointer\"]".into(), 2),
            ("[\"/a~2\"]".into(), 2),
            ("[\"/a\",\"/a/b\"]".into(), 2),
            (json!(vec!["/a"; 257]).to_string(), 1),
            (json!([format!("/{}", "a".repeat(512))]).to_string(), 1),
            (json!(["/a".repeat(33)]).to_string(), 1),
        ] {
            let mut socket = server.ws("/sessions/active/subscribe").await;
            socket.send(Message::Text(request.into())).await.unwrap();
            assert_eq!(frame(&mut socket).await[1], code);
            assert_eq!(close_code(&mut socket).await, 1008);
        }
        let mut socket = server.ws("/sessions/active/subscribe").await;
        let initial = select(&mut socket, &["/not-yet-filled"]).await;
        assert_eq!(initial[2], json!([]));
        socket
            .send(Message::Ping(bytes::Bytes::from_static(b"S4-ping-7913")))
            .await
            .unwrap();
        assert_eq!(
            bounded(socket.next()).await.unwrap().unwrap(),
            Message::Pong(bytes::Bytes::from_static(b"S4-ping-7913"))
        );
        socket.send(Message::Text("[\"\"]".into())).await.unwrap();
        assert_eq!(
            frame(&mut socket).await,
            json!([2, 5, "subscription already established"])
        );
        assert_eq!(close_code(&mut socket).await, 1008);
        let mut idle = server.ws("/sessions/active/subscribe").await;
        assert_eq!(
            frame(&mut idle).await,
            json!([2, 3, "subscription timed out"])
        );
        assert_eq!(close_code(&mut idle).await, 1008);
        server.shutdown().await;
    }));
}

#[test]
fn upgrade_detach_barrier_distinguishes_fixed_id_from_active() {
    let mut h = Harness::new();
    h.publisher("test.s4.race", 894_021);
    let id = h.id();
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        let mut fixed = server.ws(&format!("/sessions/{id}/subscribe")).await;
        let mut active = server.ws("/sessions/active/subscribe").await;
        // Both successful upgrades are acknowledged before detach, and both
        // first selections are held until detach finishes. No timing sleep.
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let detached = barrier.clone();
        let host = h.host.clone();
        let detach = tokio::spawn(async move {
            host.process_exited(894_021);
            bounded(detached.wait()).await;
        });
        bounded(barrier.wait()).await;
        bounded(detach).await.unwrap();
        fixed
            .send(Message::Text("[\"/vosu.client/pid\"]".into()))
            .await
            .unwrap();
        assert_eq!(frame(&mut fixed).await[1], 6);
        assert_eq!(close_code(&mut fixed).await, 1008);
        assert_eq!(
            select(&mut active, &["/vosu.client/pid"]).await[2],
            json!([])
        );
        drop(active);
        drop(fixed);
        server.shutdown().await;
    }));
}

#[test]
fn established_fixed_id_removes_indices_then_closes_normally() {
    let mut h = Harness::new();
    h.publisher("test.s4.fixed", 894_022);
    let id = h.id();
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        let mut socket = server.ws(&format!("/sessions/{id}/subscribe")).await;
        assert_eq!(
            select(&mut socket, &["/vosu.client/pid", "/missing"]).await[2],
            json!([[0, 894_022]])
        );
        h.host.process_exited(894_022);
        let final_frame = frame(&mut socket).await;
        assert_eq!(final_frame[0], 1);
        assert_eq!(final_frame[2], json!([]));
        assert_eq!(final_frame[3], json!([0]));
        assert_eq!(close_code(&mut socket).await, 1000);
        drop(socket);
        server.shutdown().await;
    }));
}

#[test]
fn actual_pong_slot_overflow_closes_and_preserves_first_payload() {
    let mut h = Harness::new();
    h.publisher("test.s4.pong", 894_023);
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        let (connection, mut output) = server.hub.connect(SessionTarget::Active).unwrap();
        // Intentionally do not run a writer: fill the actual production control
        // slot, then exercise its refusal (not a simulated capacity flag).
        connection.ping(bytes::Bytes::from_static(b"first-7913"));
        connection.ping(bytes::Bytes::from_static(b"second-8914"));
        assert_eq!(
            bounded(output.recv()).await,
            Some(Outbound::Close { code: 1011 })
        );
        assert_eq!(
            bounded(output.recv()).await,
            Some(Outbound::Pong(bytes::Bytes::from_static(b"first-7913")))
        );
        assert_eq!(bounded(output.recv()).await, None);
        drop(connection);
        server.shutdown().await;
    }));
}

#[test]
fn slow_writer_retains_only_latest_pending_snapshot() {
    let mut h = Harness::new();
    let ns = "test.s4.slow";
    let (handle, _) = h.publisher(ns, 894_024);
    publish(ns, handle, &[("/value", PublishValueRef::I64(1))]);
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        let (connection, mut output) = server.hub.connect(SessionTarget::Active).unwrap();
        connection.text(json!([format!("/{ns}/value")]).to_string().as_bytes());
        let Some(Outbound::Text(initial)) = bounded(output.recv()).await else {
            panic!("initial");
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&initial).unwrap()[2],
            json!([[0, 1]])
        );
        // A real transport keeps this reservation until its sink.send completes.
        // Hold it while 127 successful ABI commits race the hub reader.
        for value in 2..=128 {
            publish(ns, handle, &[("/value", PublishValueRef::I64(value))]);
        }
        connection.write_completed();
        let Some(Outbound::Text(latest)) = bounded(output.recv()).await else {
            panic!("latest");
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&latest).unwrap()[2],
            json!([[0, 128]])
        );
        connection.write_completed();
        drop(connection);
        drop(output);
        server.shutdown().await;
    }));
}

#[test]
fn escaped_output_at_one_mib_succeeds_and_one_byte_over_fails() {
    let mut h = Harness::new();
    let ns = "test.s4.boundary";
    let (handle, _) = h.publisher(ns, 894_025);
    let overhead = json!([0, 1, [[0, ""]]]).to_string().len();
    let encoded = 1024 * 1024 - overhead;
    let at_limit = format!("{}{}", "\0".repeat(encoded / 6), "x".repeat(encoded % 6));
    publish(
        ns,
        handle,
        &[("/large", PublishValueRef::String(&at_limit))],
    );
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        let mut socket = server.ws("/sessions/active/subscribe").await;
        let selected = select(&mut socket, &[&format!("/{ns}/large")]).await;
        assert_eq!(selected.to_string().len(), 1024 * 1024);
        assert_eq!(selected[2][0][1], at_limit);
        drop(socket);
        let over = format!("{at_limit}x");
        publish(ns, handle, &[("/large", PublishValueRef::String(&over))]);
        let mut socket = server.ws("/sessions/active/subscribe").await;
        assert_eq!(
            select(&mut socket, &[&format!("/{ns}/large")]).await,
            json!([2, 4, "state serialization failed"])
        );
        assert_eq!(close_code(&mut socket).await, 1008);
        drop(socket);
        server.shutdown().await;
    }));
}

#[test]
fn transport_subscription_registers_and_releases_root_demand() {
    use vosu_core::state::SessionKey;
    let mut h = Harness::new();
    let ns = "test.s4.demand";
    let (handle, _) = h.publisher(ns, 894_026);
    let demand = || {
        h.host
            .state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
    };
    assert!(demand().selectors.is_empty());
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        assert_eq!(server.get("/sessions/active/state").await.0, 200);
        assert!(demand().selectors.is_empty());
        let mut socket = server.ws("/sessions/active/subscribe").await;
        let initial = select(&mut socket, &[""]).await;
        assert_eq!(initial[0], 0);
        assert!(
            demand()
                .selectors
                .iter()
                .any(|(namespace, pointer)| namespace.as_ref() == ns && pointer.is_empty())
        );
        assert!(
            demand()
                .selectors
                .iter()
                .any(|(namespace, _)| namespace.as_ref() == "vosu.client")
        );
        // The root selection must gain namespaces first materialized after the
        // subscription, not freeze the initial document's set of filled keys.
        publish(ns, handle, &[("/late", PublishValueRef::I64(8914))]);
        let update = frame(&mut socket).await;
        assert_eq!(update[2][0][1][ns]["late"], 8914);
        socket.close(None).await.unwrap();
        bounded(async {
            while !demand().selectors.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        drop(socket);
        server.shutdown().await;
    }));
}

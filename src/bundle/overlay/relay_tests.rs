use super::*;
use bytes::Bytes;
use serde_json::{Value, json};
use vosu_core::server::websocket::{self, Outbound, WebSocketHub};
use vosu_proptest::bounded;

struct NoState {
    events: tokio::sync::watch::Receiver<u64>,
}

impl websocket::StateProvider for NoState {
    fn poll_rate(&self, _: websocket::Schema) -> std::time::Duration {
        std::time::Duration::from_millis(100)
    }
    fn subscribe(&self, _: websocket::Schema) -> tokio::sync::watch::Receiver<u64> {
        self.events.clone()
    }
    fn capture(&self, _: websocket::Schema) -> Option<Arc<dyn websocket::StateSnapshot>> {
        None
    }
}

struct CommandProviders;
impl vosu_overlay_host::commands::SettingsProvider for CommandProviders {
    fn get(&self, _: &str) -> Result<Value, String> {
        Ok(json!({}))
    }
    fn save(&self, _: &str, _: Value) -> Result<(), String> {
        Ok(())
    }
}

fn namespaced_client_id(namespace: u64, local_id: u64) -> Option<u64> {
    local_id.checked_mul(8)?.checked_add(namespace)
}

#[test]
fn client_id_retains_the_originating_endpoint() {
    for namespace in 1..=COMMANDS_NAMESPACE {
        let id = namespaced_client_id(namespace, 1).unwrap();
        assert_eq!(client_channel(id), Some(namespace));
    }
    assert_eq!(client_channel(8), None);
}

#[tokio::test]
async fn relays_are_processed_in_order_before_browser_delivery() {
    let (_, events) = tokio::sync::watch::channel(0);
    let provider = Arc::new(NoState { events });
    let hub = WebSocketHub::new(websocket::Schema::V1, provider);
    let (connection, mut outbound) = hub.connect_commands();
    let local_id = connection.id();
    let connection = Arc::new(tokio::sync::Mutex::new(connection));

    let sink = Arc::new(WebSocketRelaySink::new());
    let providers = Arc::new(CommandProviders);
    let processor = Arc::new(CommandProcessor::new(providers, sink.clone()));
    sink.bind_processor(&processor);

    let client_id = namespaced_client_id(COMMANDS_NAMESPACE, local_id).unwrap();
    let client = CommandClient::from_query(client_id, Some("counter")).unwrap();
    let _registration = sink.register(
        COMMANDS_NAMESPACE,
        client,
        Arc::new(bounded!(connection.lock()).control_handle()),
    );
    for value in [1, 2] {
        sink.relay(Relay {
            except: None,
            command: "updateSettings".into(),
            overlay: "counter".into(),
            payload: Some(format!(r#"{{"value":{value}}}"#)),
        });
    }

    for value in [1, 2] {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(1), outbound.recv())
            .await
            .unwrap()
            .unwrap();
        let Outbound::Reply(bytes) = frame else {
            panic!("relay must produce a command reply");
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({ "command": "updateSettings", "message": { "value": value } })
        );
    }

    bounded!(connection.lock()).cleanup();
    bounded!(processor.shutdown());
    bounded!(sink.shutdown());
}
#[tokio::test]
async fn relay_reply_survives_connection_mutex_contention() {
    let (_, events) = tokio::sync::watch::channel(0);
    let hub = WebSocketHub::new(websocket::Schema::V1, Arc::new(NoState { events }));
    let (connection, mut outbound) = hub.connect_commands();
    let (healthy, mut healthy_outbound) = hub.connect_commands();
    let local_id = connection.id();
    let healthy_id = namespaced_client_id(COMMANDS_NAMESPACE, healthy.id()).unwrap();
    let healthy = Arc::new(tokio::sync::Mutex::new(healthy));
    let connection = Arc::new(tokio::sync::Mutex::new(connection));

    let sink = Arc::new(WebSocketRelaySink::new());
    let processor = Arc::new(CommandProcessor::new(
        Arc::new(CommandProviders),
        sink.clone(),
    ));
    sink.bind_processor(&processor);
    let client_id = namespaced_client_id(COMMANDS_NAMESPACE, local_id).unwrap();
    let client = CommandClient::from_query(client_id, Some("counter")).unwrap();
    let _registration = sink.register(
        COMMANDS_NAMESPACE,
        client,
        Arc::new(bounded!(connection.lock()).control_handle()),
    );
    let healthy_client = CommandClient::from_query(healthy_id, Some("counter")).unwrap();
    let _healthy_registration = sink.register(
        COMMANDS_NAMESPACE,
        healthy_client,
        Arc::new(bounded!(healthy.lock()).control_handle()),
    );

    let guard = bounded!(connection.lock());
    sink.relay(Relay {
        except: None,
        command: "updateSettings".into(),
        overlay: "counter".into(),
        payload: Some(r#"{"value":1}"#.into()),
    });
    tokio::task::yield_now().await;

    let Some(Outbound::Reply(healthy_reply)) =
        tokio::time::timeout(std::time::Duration::from_secs(1), healthy_outbound.recv())
            .await
            .unwrap()
    else {
        panic!("healthy recipient must progress while another connection mutex is held");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&healthy_reply).unwrap(),
        json!({ "command": "updateSettings", "message": { "value": 1 } })
    );

    let Some(Outbound::Reply(reply)) =
        tokio::time::timeout(std::time::Duration::from_secs(1), outbound.recv())
            .await
            .unwrap()
    else {
        panic!("relay reply must arrive while the connection mutex remains held");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&reply).unwrap(),
        json!({ "command": "updateSettings", "message": { "value": 1 } })
    );
    drop(guard);

    bounded!(connection.lock()).cleanup();
    bounded!(healthy.lock()).cleanup();
    bounded!(processor.shutdown());
    bounded!(sink.shutdown());
}

#[tokio::test]
async fn full_relay_recipient_does_not_block_healthy_recipients() {
    let (_, events) = tokio::sync::watch::channel(0);
    let provider = Arc::new(NoState { events });
    let hub = WebSocketHub::new(websocket::Schema::V1, provider);
    let (stalled, _stalled_outbound) = hub.connect_commands();
    let (healthy, mut healthy_outbound) = hub.connect_commands();
    let stalled_id = namespaced_client_id(COMMANDS_NAMESPACE, stalled.id()).unwrap();
    let healthy_id = namespaced_client_id(COMMANDS_NAMESPACE, healthy.id()).unwrap();
    let stalled = Arc::new(tokio::sync::Mutex::new(stalled));
    let healthy = Arc::new(tokio::sync::Mutex::new(healthy));

    let sink = Arc::new(WebSocketRelaySink::new());
    let processor = Arc::new(CommandProcessor::new(
        Arc::new(CommandProviders),
        sink.clone(),
    ));
    sink.bind_processor(&processor);
    let stalled_client = CommandClient::from_query(stalled_id, Some("counter")).unwrap();
    let healthy_client = CommandClient::from_query(healthy_id, Some("counter")).unwrap();
    let _stalled_registration = sink.register(
        COMMANDS_NAMESPACE,
        stalled_client.clone(),
        Arc::new(bounded!(stalled.lock()).control_handle()),
    );
    let _healthy_registration = sink.register(
        COMMANDS_NAMESPACE,
        healthy_client,
        Arc::new(bounded!(healthy.lock()).control_handle()),
    );

    // Saturate control traffic without draining this recipient.
    for _ in 0..16 {
        assert!(bounded!(stalled.lock()).try_send_text(Bytes::from_static(b"occupied")));
    }
    sink.relay(Relay {
        except: None,
        command: "updateSettings".into(),
        overlay: "counter".into(),
        payload: Some(r#"{"value":1}"#.into()),
    });
    let received = tokio::time::timeout(std::time::Duration::from_secs(1), healthy_outbound.recv())
        .await
        .unwrap();
    let Some(Outbound::Reply(received)) = received else {
        panic!("healthy recipient must receive the settings update");
    };
    assert_eq!(
        serde_json::from_slice::<Value>(&received).unwrap(),
        json!({ "command": "updateSettings", "message": { "value": 1 } })
    );
    assert!(!bounded!(stalled.lock()).try_send_text(Bytes::from_static(b"still-open")));
    bounded!(stalled.lock()).cleanup();
    bounded!(healthy.lock()).cleanup();
    bounded!(processor.shutdown());
    bounded!(sink.shutdown());
}

#[tokio::test]
async fn relay_worker_shutdown_is_joined() {
    let sink = WebSocketRelaySink::new();
    assert!(!sink.is_shutdown());
    bounded!(sink.shutdown());
    assert!(sink.is_shutdown());
}

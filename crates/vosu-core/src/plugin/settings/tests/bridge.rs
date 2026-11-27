use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::runtime::Handle;
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{CborValue, Message};

use crate::config::ConfigStore;
use crate::plugin::settings::{
    BRIDGE_B2_PLUGIN_ID, ConfigSettingsBridgeB2, PluginSettingsService, SettingsChangeDelivery,
    SettingsError, SettingsSchemaSource, SettingsStorage,
};

/// A test-local stand-in for bridge B2's own five `vosu.toml` keys
/// (mirroring `config/schema.rs`'s own defaults), now that the bridge
/// no longer embeds a `settings.schema.json` of its own (P0; the real
/// one is `vosu-osu`'s, P1).
fn bridge_b2_test_schema() -> SettingsSchema {
    SettingsSchema::parse(
        r#"[
            {"uniqueID":"CALCULATE_PP","type":"checkbox","title":"Calculate pp","value":true},
            {"uniqueID":"POLL_RATE","type":"number","title":"Poll rate","value":150.0},
            {"uniqueID":"PRECISE_DATA_POLL_RATE","type":"number","title":"Precise data poll rate","value":10.0},
            {"uniqueID":"SHOW_MP_COMMANDS","type":"checkbox","title":"Show multiplayer commands","value":false},
            {"uniqueID":"READ_MANIA_SCROLL_SPEED","type":"checkbox","title":"Read mania scroll speed","value":true}
        ]"#,
    )
    .expect("the test fixture's own embedded schema is valid JSON")
}

async fn config_store() -> (tempfile::TempDir, Arc<ConfigStore>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vosu.toml");
    let store = ConfigStore::initialize(&path).await.unwrap();
    (dir, Arc::new(store))
}

#[tokio::test]
async fn bridge_b2_loads_current_config_values() {
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    config.update(|c| {
        c.poll_rate = 200.0;
        c.calculate_pp = false;
    });
    let bridge = ConfigSettingsBridgeB2::new(config);
    let values = bridge.load(BRIDGE_B2_PLUGIN_ID).unwrap();
    assert_eq!(values.get("POLL_RATE"), Some(&Value::from(200.0)));
    assert_eq!(values.get("CALCULATE_PP"), Some(&Value::Bool(false)));
}

#[tokio::test]
async fn bridge_b2_store_writes_through_to_the_config() {
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    let bridge = ConfigSettingsBridgeB2::new(Arc::clone(&config));
    let mut values = Map::new();
    values.insert("POLL_RATE".into(), Value::from(300.0));
    values.insert("SHOW_MP_COMMANDS".into(), Value::Bool(true));
    bridge.store(BRIDGE_B2_PLUGIN_ID, &values).unwrap();
    assert_eq!(config.current().poll_rate, 300.0);
    assert!(config.current().show_mp_commands);
}

#[tokio::test]
async fn bridge_b2_refuses_an_unbridged_plugin_id() {
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    let bridge = ConfigSettingsBridgeB2::new(config);
    assert_eq!(
        bridge.load("some.other.plugin").unwrap_err(),
        SettingsError::NotBridged("some.other.plugin".into())
    );
}

#[tokio::test]
async fn apply_validates_stores_and_builds_settings_changed() {
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    let storage: Arc<dyn SettingsStorage> = Arc::new(ConfigSettingsBridgeB2::new(config));
    let service = PluginSettingsService::new(storage);
    let mut patch = Map::new();
    patch.insert("POLL_RATE".into(), Value::from(250.0));
    let message = service
        .apply(BRIDGE_B2_PLUGIN_ID, &bridge_b2_test_schema(), &patch)
        .unwrap();
    let Message::SettingsChanged(changed) = message else {
        panic!("expected SettingsChanged");
    };
    let CborValue::Map(entries) = changed.values else {
        panic!("expected a CBOR map");
    };
    assert!(entries.iter().any(|(key, value)| {
        *key == CborValue::Text("POLL_RATE".into()) && *value == CborValue::Float(250.0)
    }));
}

#[tokio::test]
async fn apply_reports_the_clamped_value_actually_stored_not_the_submitted_one() {
    // `ConfigDocument::validate` raises `POLL_RATE` below 100 up to
    // 100 (`config/document.rs`): `apply` must broadcast what was
    // actually saved, never the value this call merely submitted.
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    let storage: Arc<dyn SettingsStorage> = Arc::new(ConfigSettingsBridgeB2::new(config));
    let service = PluginSettingsService::new(storage);
    let mut patch = Map::new();
    patch.insert("POLL_RATE".into(), Value::from(50.0));
    let message = service
        .apply(BRIDGE_B2_PLUGIN_ID, &bridge_b2_test_schema(), &patch)
        .unwrap();
    let Message::SettingsChanged(changed) = message else {
        panic!("expected SettingsChanged");
    };
    let CborValue::Map(entries) = changed.values else {
        panic!("expected a CBOR map");
    };
    assert!(
        entries.iter().any(|(key, value)| {
            *key == CborValue::Text("POLL_RATE".into()) && *value == CborValue::Float(100.0)
        }),
        "expected the clamped 100.0, got: {entries:?}"
    );
}

#[tokio::test]
async fn effective_settings_is_an_empty_map_without_a_schema() {
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    let storage: Arc<dyn SettingsStorage> = Arc::new(ConfigSettingsBridgeB2::new(config));
    let service = PluginSettingsService::new(storage);
    let value = service
        .effective_settings(BRIDGE_B2_PLUGIN_ID, None)
        .unwrap();
    assert_eq!(value, CborValue::Map(Vec::new()));
}

struct FixedSchemaSource(SettingsSchema);

impl SettingsSchemaSource for FixedSchemaSource {
    fn schema_for(&self, _plugin_id: &str) -> Option<SettingsSchema> {
        Some(self.0.clone())
    }
}

struct RecordingSettingsDelivery {
    tx: std::sync::Mutex<std::sync::mpsc::Sender<(String, Message)>>,
}

impl SettingsChangeDelivery for RecordingSettingsDelivery {
    fn settings_changed(&self, plugin_id: &str, message: Message) {
        let _ = self
            .tx
            .lock()
            .unwrap()
            .send((plugin_id.to_owned(), message));
    }
}

#[tokio::test]
async fn a_dashboard_style_config_update_reaches_the_plugin_as_settings_changed() {
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    let bridge = Arc::new(ConfigSettingsBridgeB2::new(Arc::clone(&config)));
    let service = Arc::new(PluginSettingsService::new(bridge.clone()));
    let (tx, rx) = std::sync::mpsc::channel();
    let delivery = Arc::new(RecordingSettingsDelivery {
        tx: std::sync::Mutex::new(tx),
    });
    bridge.watch_config_changes(
        &Handle::current(),
        service,
        Arc::new(FixedSchemaSource(bridge_b2_test_schema())),
        delivery,
    );

    // A plain `ConfigStore::update` call, exactly the shape the
    // existing dashboard settings page (`src/runtime/render/settings.rs`)
    // and a `vosu.toml` hot reload both use — not a write through
    // this bridge's own `store` method.
    config.update(|c| {
        c.poll_rate = 275.0;
    });

    let (plugin_id, message) = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(received) = rx.try_recv() {
                return received;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("expected a SETTINGS_CHANGED delivery after the config update");

    assert_eq!(plugin_id, BRIDGE_B2_PLUGIN_ID);
    let Message::SettingsChanged(changed) = message else {
        panic!("expected SettingsChanged");
    };
    let CborValue::Map(entries) = changed.values else {
        panic!("expected a CBOR map");
    };
    assert!(
        entries.iter().any(|(key, value)| {
            *key == CborValue::Text("POLL_RATE".into()) && *value == CborValue::Float(275.0)
        }),
        "expected POLL_RATE=275.0, got: {entries:?}"
    );
}

#[tokio::test]
async fn an_unrelated_config_change_does_not_trigger_a_settings_changed() {
    let (_dir, config) = vosu_proptest::bounded!(config_store());
    let bridge = Arc::new(ConfigSettingsBridgeB2::new(Arc::clone(&config)));
    let service = Arc::new(PluginSettingsService::new(bridge.clone()));
    let (tx, rx) = std::sync::mpsc::channel();
    let delivery = Arc::new(RecordingSettingsDelivery {
        tx: std::sync::Mutex::new(tx),
    });
    bridge.watch_config_changes(
        &Handle::current(),
        service,
        Arc::new(FixedSchemaSource(bridge_b2_test_schema())),
        delivery,
    );

    config.update(|c| {
        c.debug_log = !c.debug_log;
    });
    // Let the watcher task observe and discard the change.
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(rx.try_recv().is_err());
}

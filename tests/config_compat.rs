use std::{fs, time::Duration};

use tempfile::TempDir;
use vosu::config::{BINDING_ORDER, ConfigDocument, ConfigStore};

const EXPECTED_DEFAULT: &str = concat!(
    "DEBUG_LOG = false\n",
    "ENABLE_AUTOUPDATE = true\n",
    "OPEN_DASHBOARD_ON_STARTUP = true\n",
    "START_AT_LOGIN = false\n",
    "\n",
    "SHOW_MP_COMMANDS = false\n",
    "CALCULATE_PP = true\n",
    "READ_MANIA_SCROLL_SPEED = true\n",
    "\n",
    "ENABLE_KEY_OVERLAY = true\n",
    "\n",
    "POLL_RATE = 150.0\n",
    "PRECISE_DATA_POLL_RATE = 10.0\n",
    "V2_SUBSCRIPTION_DEBOUNCE = 0.0\n",
    "\n",
    "SERVER_IP = \"127.0.0.1\"\n",
    "SERVER_PORT = 24050.0\n",
    "ALLOWED_IPS = \"127.0.0.1,localhost,absolute\"\n",
    "\n",
    "STATIC_FOLDER_PATH = \"./static\"\n",
);

#[test]
fn formatting_order_defaults_and_group_boundaries_are_stable() {
    let serialized = ConfigDocument::default().serialize();
    assert_eq!(serialized, EXPECTED_DEFAULT);
    let bindings: Vec<_> = serialized
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.split_once('=').unwrap().0.trim())
        .collect();
    assert_eq!(bindings, BINDING_ORDER);
    assert!(
        serialized.ends_with('\n'),
        "canonical TOML has one trailing newline"
    );
}

#[test]
fn parsing_fallbacks_clamps_and_refresh_retention_match_source() {
    let mut document = ConfigDocument::parse(
        "POLL_RATE = 99.9\nPRECISE_DATA_POLL_RATE = 0.0\nV2_SUBSCRIPTION_DEBOUNCE = 0.125\nSERVER_PORT = \"not-a-number\"\nDEBUG_LOG = \"true\"\nSERVER_IP = \"localhost\"\nUNKNOWN = \"value\"\n",
    )
    .expect("parse typed TOML");
    let config = document.config();
    assert_eq!(config.poll_rate, 100.0);
    assert_eq!(config.precise_data_poll_rate, 1.0);
    assert_eq!(config.v2_subscription_debounce, 0.125);
    assert!(
        document
            .serialize()
            .contains("V2_SUBSCRIPTION_DEBOUNCE = 0.125")
    );
    assert_eq!(
        config.server_port, 24_050.0,
        "wrong TOML type falls back per binding"
    );
    assert!(!config.debug_log, "wrong TOML type uses the schema default");
    assert_eq!(config.server_ip, "localhost");

    let before_malformed = document.config().clone();
    assert!(document.refresh("SERVER_PORT = [\n").is_err());
    assert_eq!(
        document.config(),
        &before_malformed,
        "malformed TOML cannot partially refresh the document"
    );

    let warnings = document
        .refresh("CALCULATE_PP = false\nPOLL_RATE = nan\nV2_SUBSCRIPTION_DEBOUNCE = -0.25\n")
        .expect("refresh valid typed TOML");
    assert_eq!(
        warnings,
        ["Config %V2_SUBSCRIPTION_DEBOUNCE% value %-0.25% below minimum %0%, clamping."]
    );
    assert!(!document.config().calculate_pp);
    assert_eq!(document.config().poll_rate, 150.0);
    assert_eq!(document.config().v2_subscription_debounce, 0.0);
    assert!(
        document
            .serialize()
            .contains("V2_SUBSCRIPTION_DEBOUNCE = 0.0")
    );
    assert_eq!(
        document.config().server_ip,
        "localhost",
        "missing bindings retain live values"
    );
}

#[tokio::test]
async fn legacy_env_files_migrate_to_typed_toml_with_binding_precedence() {
    let root = TempDir::new().unwrap();
    let current = root.path().join("vosu.toml");
    let newer_legacy = root.path().join("vosu.env");
    let older_legacy = root.path().join("tosu.env");
    fs::write(
        &newer_legacy,
        "SERVER_PORT=24051\nSERVER_PORT=24052 # duplicate-last-wins\nPOLL_RATE=70\nSTATIC_FOLDER_PATH=\"./from-vosu-env\"\n",
    )
    .unwrap();
    fs::write(
        &older_legacy,
        "SERVER_PORT=24053\nPRECISE_DATA_POLL_RATE=5\nV2_SUBSCRIPTION_DEBOUNCE=0.125\nDEPRECATED_BINDING=drop-me\n",
    )
    .unwrap();

    let store = ConfigStore::initialize(&current).await.unwrap();
    assert!(current.is_file());
    assert!(!newer_legacy.exists());
    assert!(!older_legacy.exists());
    assert_eq!(store.current().server_port, 24_052.0);
    assert_eq!(store.current().poll_rate, 100.0);
    assert_eq!(store.current().precise_data_poll_rate, 5.0);
    assert_eq!(store.current().v2_subscription_debounce, 0.125);
    assert_eq!(store.current().static_folder_path, "./from-vosu-env");

    let persisted = fs::read_to_string(&current).unwrap();
    assert!(persisted.ends_with('\n'));
    assert!(!persisted.contains("DEPRECATED_BINDING"));
    let persisted: toml::Table = toml::from_str(&persisted).unwrap();
    assert_eq!(
        persisted.get("DEBUG_LOG").and_then(toml::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        persisted.get("SERVER_PORT").and_then(toml::Value::as_float),
        Some(24_052.0)
    );
    assert_eq!(
        persisted
            .get("STATIC_FOLDER_PATH")
            .and_then(toml::Value::as_str),
        Some("./from-vosu-env")
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn current_toml_priority_reload_last_good_and_shutdown_flush() {
    let root = TempDir::new().unwrap();
    let current = root.path().join("vosu.toml");
    let newer_legacy = root.path().join("vosu.env");
    let older_legacy = root.path().join("tosu.env");
    fs::write(
        &current,
        "SERVER_PORT = 24061.0\nPOLL_RATE = \"wrong-type\"\nSTATIC_FOLDER_PATH = \"./from-toml\"\n",
    )
    .unwrap();
    fs::write(
        &newer_legacy,
        "SERVER_PORT=24062\nPOLL_RATE=175\nPRECISE_DATA_POLL_RATE=5\nSTATIC_FOLDER_PATH=./from-vosu-env\n",
    )
    .unwrap();
    fs::write(
        &older_legacy,
        "SERVER_PORT=24063\nPOLL_RATE=180\nPRECISE_DATA_POLL_RATE=6\nV2_SUBSCRIPTION_DEBOUNCE=0.5\n",
    )
    .unwrap();

    let store = ConfigStore::initialize(&current).await.unwrap();
    assert!(!newer_legacy.exists());
    assert!(!older_legacy.exists());
    assert_eq!(store.current().server_port, 24_061.0);
    assert_eq!(
        store.current().poll_rate,
        150.0,
        "present wrong TOML type shadows both legacy sources"
    );
    assert_eq!(store.current().precise_data_poll_rate, 5.0);
    assert_eq!(store.current().v2_subscription_debounce, 0.5);
    assert_eq!(store.current().static_folder_path, "./from-toml");

    let mut changes = store.subscribe();
    fs::write(&current, "SERVER_PORT = 24064.0\nBROKEN = [\n").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), changes.recv())
            .await
            .is_err(),
        "malformed external TOML must not publish"
    );
    assert_eq!(store.current().server_port, 24_061.0);

    fs::write(
        &current,
        "SERVER_PORT = 24064.0\nV2_SUBSCRIPTION_DEBOUNCE = 1.75\nSTATIC_FOLDER_PATH = \"./changed\"\n",
    )
    .unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("watcher deadline")
        .expect("config change event");
    assert_eq!(changed.old.server_port, 24_061.0);
    assert_eq!(changed.new.server_port, 24_064.0);
    assert_eq!(changed.new.static_folder_path, "./changed");
    assert_eq!(changed.new.v2_subscription_debounce, 1.75);
    assert_eq!(
        changed.new.precise_data_poll_rate, 5.0,
        "missing reload bindings retain the last-good value"
    );

    assert!(store.update(|config| {
        config.debug_log = true;
        config.server_port = 24_065.0;
    }));
    assert!(!store.update(|config| config.server_port = 24_065.0));
    store.flush().await.unwrap();
    let persisted = fs::read_to_string(&current).unwrap();
    assert!(persisted.ends_with('\n'));
    let persisted: toml::Table = toml::from_str(&persisted).unwrap();
    assert_eq!(
        persisted.get("DEBUG_LOG").and_then(toml::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        persisted.get("SERVER_PORT").and_then(toml::Value::as_float),
        Some(24_065.0)
    );
    assert!(
        BINDING_ORDER
            .iter()
            .all(|binding| persisted.contains_key(*binding))
    );
    store.shutdown().await.unwrap();
}

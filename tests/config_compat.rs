use std::{fs, time::Duration};

use tempfile::TempDir;
use vosu::config::{BINDING_ORDER, ConfigDocument, ConfigStore};

const EXPECTED_DEFAULT: &str = "DEBUG_LOG=false\nENABLE_AUTOUPDATE=true\nOPEN_DASHBOARD_ON_STARTUP=true\n\nSHOW_MP_COMMANDS=false\nCALCULATE_PP=true\nREAD_MANIA_SCROLL_SPEED=true\n\nENABLE_KEY_OVERLAY=true\nENABLE_INGAME_OVERLAY=false\n\nPOLL_RATE=150\nPRECISE_DATA_POLL_RATE=10\nV2_SUBSCRIPTION_DEBOUNCE=0\n\nINGAME_OVERLAY_KEYBIND=Control + Shift + Space\nINGAME_OVERLAY_MAX_FPS=60\n\nSERVER_IP=127.0.0.1\nSERVER_PORT=24050\nALLOWED_IPS=127.0.0.1,localhost,absolute\n\nSTATIC_FOLDER_PATH=./static";

#[test]
fn formatting_order_defaults_and_group_boundaries_are_stable() {
    let serialized = ConfigDocument::default().serialize();
    assert_eq!(serialized, EXPECTED_DEFAULT);
    let bindings: Vec<_> = serialized
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.split_once('=').unwrap().0)
        .collect();
    assert_eq!(bindings, BINDING_ORDER);
    assert!(
        !serialized.ends_with('\n'),
        "source config has no trailing newline"
    );
}

#[test]
fn parsing_fallbacks_clamps_and_refresh_retention_match_source() {
    let mut document = ConfigDocument::parse(
        "POLL_RATE=99.9\nPRECISE_DATA_POLL_RATE=0\nV2_SUBSCRIPTION_DEBOUNCE=0.125\nSERVER_PORT=not-a-number\nDEBUG_LOG=TRUE\nSERVER_IP=localhost\nUNKNOWN=value\n",
    );
    let config = document.config();
    assert_eq!(config.poll_rate, 100.0);
    assert_eq!(config.precise_data_poll_rate, 1.0);
    assert_eq!(config.v2_subscription_debounce, 0.125);
    assert!(
        document
            .serialize()
            .contains("V2_SUBSCRIPTION_DEBOUNCE=0.125")
    );
    assert_eq!(
        config.server_port, 24_050.0,
        "invalid number falls back per binding"
    );
    assert!(!config.debug_log, "only exact unquoted `true` is true");
    assert_eq!(config.server_ip, "localhost");

    let warnings =
        document.refresh("CALCULATE_PP=false\nPOLL_RATE=NaN\nV2_SUBSCRIPTION_DEBOUNCE=-0.25\n");
    assert_eq!(
        warnings,
        ["Config %V2_SUBSCRIPTION_DEBOUNCE% value %-0.25% below minimum %0%, clamping."]
    );
    assert!(!document.config().calculate_pp);
    assert_eq!(document.config().poll_rate, 150.0);
    assert_eq!(document.config().v2_subscription_debounce, 0.0);
    assert!(document.serialize().contains("V2_SUBSCRIPTION_DEBOUNCE=0"));
    assert_eq!(
        document.config().server_ip,
        "localhost",
        "missing bindings retain live values"
    );
}

#[tokio::test]
async fn legacy_file_migrates_reload_publishes_once_and_shutdown_flushes() {
    let root = TempDir::new().unwrap();
    let current = root.path().join("vosu.env");
    // `tosu.env` is accepted only as a one-time migration input.
    let legacy = root.path().join("tosu.env");
    fs::write(
        &legacy,
        "SERVER_PORT=24051\nPOLL_RATE=70\nPRECISE_DATA_POLL_RATE=10\nV2_SUBSCRIPTION_DEBOUNCE=0.125\nDEPRECATED_BINDING=drop-me\n",
    )
    .unwrap();

    let store = ConfigStore::initialize(&current).await.unwrap();
    assert!(current.is_file());
    assert!(!legacy.exists());
    assert_eq!(store.current().server_port, 24_051.0);
    assert_eq!(store.current().poll_rate, 100.0);
    assert_eq!(store.current().v2_subscription_debounce, 0.125);
    assert!(
        !fs::read_to_string(&current)
            .unwrap()
            .contains("DEPRECATED_BINDING")
    );

    let mut changes = store.subscribe();
    fs::write(
        &current,
        "SERVER_PORT=24052\nPOLL_RATE=175\nPRECISE_DATA_POLL_RATE=5\nV2_SUBSCRIPTION_DEBOUNCE=1.75\nSTATIC_FOLDER_PATH=./changed\n",
    )
    .unwrap();
    let changed = tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("watcher deadline")
        .expect("config change event");
    assert_eq!(changed.old.server_port, 24_051.0);
    assert_eq!(changed.new.server_port, 24_052.0);
    assert_eq!(changed.new.static_folder_path, "./changed");
    assert_eq!(changed.new.v2_subscription_debounce, 1.75);

    assert!(store.update(|config| config.server_port = 24_053.0));
    assert!(!store.update(|config| config.server_port = 24_053.0));
    store.flush().await.unwrap();
    assert!(
        fs::read_to_string(&current)
            .unwrap()
            .contains("SERVER_PORT=24053")
    );
    store.shutdown().await.unwrap();
}

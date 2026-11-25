#[test]
fn package_limit_settings_control_matches_integer_disk_contract() {
    let settings = render_runtime_settings(&Config::default());
    assert!(settings.contains(
        r#"data-id="MAX_PACKAGE_MIB" type="number" value="512" min="1" max="4096" step="1" required"#
    ));
    let settings = render_runtime_settings(&Config {
        max_package_mib: 1536.0,
        ..Config::default()
    });
    assert!(settings.contains(
        r#"data-id="MAX_PACKAGE_MIB" type="number" value="1536" min="1" max="4096" step="1" required"#
    ));
}

#[tokio::test]
async fn package_limit_save_is_atomic_bounded_and_persisted_for_resolver_composition() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vosu.toml");
    let store = Arc::new(bounded!(ConfigStore::initialize(path.clone())).unwrap());
    let service = RuntimeConfig::new(Arc::clone(&store));
    bounded!(service.save(serde_json::json!({"MAX_PACKAGE_MIB":" 1536 "}))).unwrap();
    assert_eq!(store.current().max_package_bytes(), Some(1_610_612_736));
    bounded!(store.flush()).unwrap();
    let original = std::fs::read(&path).unwrap();
    for invalid in [
        serde_json::json!(0),
        serde_json::json!(4097),
        serde_json::json!(1.5),
        serde_json::json!("wrong"),
        serde_json::json!("1e300"),
        serde_json::json!(null),
    ] {
        assert!(
            bounded!(service.save(serde_json::json!({
                "MAX_PACKAGE_MIB":invalid,"DEBUG_LOG":true
            })))
            .is_err()
        );
        assert_eq!(store.current().max_package_bytes(), Some(1_610_612_736));
        assert!(!store.current().debug_log);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    drop(service);
    bounded!(store.shutdown()).unwrap();
    drop(store);
    let reloaded = bounded!(ConfigStore::initialize(path)).unwrap();
    assert_eq!(reloaded.current().max_package_bytes(), Some(1_610_612_736));
    bounded!(reloaded.shutdown()).unwrap();
}

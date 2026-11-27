//! Catalog sources verbatim migration, exact official trust predicate,
//! fallback root path, and malformed input fail-closed checks (M8-M06).

use std::fs;
use std::sync::Arc;

use serde_json::json;
use vosu_core::catalog::sources::{OFFICIAL_SOURCES, SourceStore};
use vosu_core::migration::{MigrationError, MigrationOutcome, journal_path, run_local_migration};
use vosu_core::plugin::settings::PluginSettingsStore;
use vosu_sys::platform::BuildChannel;

use crate::support::{NoopSourceLifecycle, bundled_migration_schemas, run_release_startup};

#[tokio::test]
async fn verbatim_sources_order_enabled_flags_and_exact_official_trust() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools")).unwrap();
    fs::create_dir_all(&data).unwrap();

    // Order, verbatim spellings, disabled flags, URL aliases, case variants,
    // fork lookalikes, and third-party sources.
    let legacy_sources = json!([
        {"source": "18-lab-dev/vosu-tools", "enabled": true},
        {"source": "https://github.com/18-lab-dev/vosu-tools", "enabled": true},
        {"source": "18-lab-dev/vosu-overlays", "enabled": false},
        {"source": "18-Lab-Dev/vosu-tools", "enabled": true},
        {"source": "fork-dev/vosu-tools", "enabled": true},
        {"source": "18-lab-dev/vosulay", "enabled": false},
        {"source": "community-org/custom-tools", "enabled": false},
        {"source": "https://codeberg.org/example/catalog.json", "enabled": true}
    ]);
    fs::write(
        config.join("tools").join("catalog-sources.json"),
        serde_json::to_vec_pretty(&legacy_sources).unwrap(),
    )
    .unwrap();

    let (outcome, cfg, _) = run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed");
    };
    let receipt = journal.catalog_sources.receipt().unwrap();
    assert!(receipt.migrated);
    assert_eq!(receipt.source_count, 8);
    assert_eq!(receipt.trusted_official_count, 3);

    // Verify through the real production SourceStore.
    let store = SourceStore::open(&config, Arc::new(NoopSourceLifecycle)).unwrap();
    let (_generation, records) = store.snapshot().unwrap();
    assert_eq!(records.len(), 8);

    let expected: [(&str, bool, bool); 8] = [
        ("18-lab-dev/vosu-tools", true, true),
        ("https://github.com/18-lab-dev/vosu-tools", true, false),
        ("18-lab-dev/vosu-overlays", false, true),
        ("18-Lab-Dev/vosu-tools", true, false),
        ("fork-dev/vosu-tools", true, false),
        ("18-lab-dev/vosulay", false, true),
        ("community-org/custom-tools", false, false),
        ("https://codeberg.org/example/catalog.json", true, false),
    ];

    for (index, (expected_source, expected_enabled, expected_trusted)) in
        expected.into_iter().enumerate()
    {
        let record = &records[index];
        assert_eq!(record.order, index, "source order drifted at {index}");
        assert_eq!(
            record.identity.as_str(),
            expected_source,
            "source string must stay verbatim at {index}"
        );
        assert_eq!(
            record.enabled, expected_enabled,
            "enabled flag changed for {expected_source}"
        );
        assert_eq!(
            record.trusted, expected_trusted,
            "trust predicate mismatch for {expected_source}"
        );
        assert_eq!(
            record.trusted,
            OFFICIAL_SOURCES.contains(&record.identity.as_str()),
            "only exact fixed OFFICIAL_SOURCES may receive trusted: true"
        );
        assert!(
            !record.private_network,
            "private_network must stay false for migrated source {expected_source}"
        );
    }

    // Zero invented credentials or extra secret files created in config root.
    assert!(!config.join("catalog-credentials.json").exists());
    assert!(!config.join("credentials.json").exists());
    assert!(!config.join("broker.json").exists());

    vosu_proptest::bounded!(cfg.shutdown()).unwrap();
}

#[tokio::test]
async fn fallback_root_catalog_sources_json_migrates_when_tools_subdir_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();

    fs::write(
        config.join("catalog-sources.json"),
        serde_json::to_vec_pretty(&json!([
            {"source": "18-lab-dev/vosu-overlays", "enabled": false},
            {"source": "community-org/fallback-only", "enabled": true}
        ]))
        .unwrap(),
    )
    .unwrap();

    let (outcome, cfg, _) = run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed");
    };
    let receipt = journal.catalog_sources.receipt().unwrap();
    assert!(receipt.migrated);
    assert_eq!(receipt.source_count, 2);
    assert_eq!(receipt.trusted_official_count, 1);

    let store = SourceStore::open(&config, Arc::new(NoopSourceLifecycle)).unwrap();
    let (_, records) = store.snapshot().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].identity.as_str(), "18-lab-dev/vosu-overlays");
    assert!(!records[0].enabled);
    assert!(records[0].trusted);
    assert_eq!(records[1].identity.as_str(), "community-org/fallback-only");
    assert!(records[1].enabled);
    assert!(!records[1].trusted);

    vosu_proptest::bounded!(cfg.shutdown()).unwrap();
}

#[test]
fn malformed_duplicate_and_invalid_catalog_sources_fail_closed() {
    let schemas = bundled_migration_schemas();
    let cases: [(&str, &str); 4] = [
        ("corrupt_json", "{not-an-array"),
        (
            "unknown_trusted_field_injection",
            r#"[{"source":"community-org/evil","enabled":true,"trusted":true}]"#,
        ),
        (
            "duplicate_source",
            r#"[{"source":"community-org/dup","enabled":true},{"source":"community-org/dup","enabled":false}]"#,
        ),
        (
            "insecure_http_source",
            r#"[{"source":"http://example.invalid/catalog.json","enabled":true}]"#,
        ),
    ];

    for (name, body) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(config.join("tools")).unwrap();
        fs::create_dir_all(&data).unwrap();

        let toml_bytes = b"POLL_RATE = 200.0\nV2_SUBSCRIPTION_DEBOUNCE = 9.0\n";
        let env_bytes = b"CALCULATE_PP=false\n";
        fs::write(config.join("vosu.toml"), toml_bytes).unwrap();
        fs::write(config.join("vosu.env"), env_bytes).unwrap();
        fs::write(config.join("tools").join("catalog-sources.json"), body).unwrap();

        let store = PluginSettingsStore::open(&config);
        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .expect_err(&format!("case {name} must fail closed"));
        assert!(
            matches!(err, MigrationError::MalformedInput { .. }),
            "case {name}: unexpected error {err:?}"
        );

        // Zero destructive writes when catalog-sources.json is invalid.
        assert!(!journal_path(&config).exists(), "case {name}");
        assert!(!store.path().exists(), "case {name}");
        assert!(
            !config.join("catalog-source-settings.json").exists(),
            "case {name}"
        );
        assert_eq!(
            fs::read(config.join("vosu.toml")).unwrap(),
            toml_bytes,
            "case {name}"
        );
        assert_eq!(
            fs::read(config.join("vosu.env")).unwrap(),
            env_bytes,
            "case {name}"
        );
    }
}

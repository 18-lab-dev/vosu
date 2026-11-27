//! S0 counterexample tests: A8.1/A8.3 idempotent legacy import, explicit
//! `false`/`0`/`0.0`/`""` preservation, fail-closed corrupt/oversized/malformed
//! handling, per-plugin and channel-root isolation, and durable `atomic_replace`
//! + `read_published` round-trips.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value, json};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{CborValue, Message};
use vosu_sys::platform::{BuildChannel, PathResolver, Paths};

use crate::plugin::settings::{
    MAX_FILE_BYTES, PluginSettingsService, PluginSettingsStore, SettingsError, SettingsStorage,
};

const PLUGIN_A: &str = "acme.alpha";
const PLUGIN_B: &str = "acme.beta";
const SECRET: &str = "sentinel-import-secret-9a4d";

fn rich_schema() -> SettingsSchema {
    SettingsSchema::parse(
        r#"[
            {"uniqueID":"ENABLED","type":"checkbox","title":"Enabled","value":true},
            {"uniqueID":"COUNT","type":"number","title":"Count","value":42},
            {"uniqueID":"RATE","type":"number","title":"Rate","value":150.0},
            {"uniqueID":"LABEL","type":"text","title":"Label","value":"default-label"},
            {"uniqueID":"SHOW","type":"checkbox","title":"Show","value":true},
            {"uniqueID":"TOKEN","type":"password","title":"Token","value":""},
            {"uniqueID":"SECTION","type":"header","title":"Section"}
        ]"#,
    )
    .expect("test schema is valid")
}

fn open(root: &Path) -> (Arc<PluginSettingsStore>, PluginSettingsService) {
    let store = Arc::new(PluginSettingsStore::open(root));
    let service = PluginSettingsService::new(store.clone() as Arc<dyn SettingsStorage>);
    (store, service)
}

fn cbor_entries(message: Message) -> Vec<(CborValue, CborValue)> {
    let Message::SettingsChanged(changed) = message else {
        panic!("expected SettingsChanged");
    };
    let CborValue::Map(entries) = changed.values else {
        panic!("expected CBOR map");
    };
    entries
}

/// S0 counterexample: explicit `false`, `0`, `0.0`, and `""` must be stored
/// and preserved verbatim — never replaced by schema defaults on import or
/// across reopen.
#[test]
fn explicit_false_zero_and_empty_string_are_preserved_not_replaced_by_defaults() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    let schema = rich_schema();

    let legacy = Map::from_iter([
        ("ENABLED".into(), json!(false)),
        ("COUNT".into(), json!(0)),
        ("RATE".into(), json!(0.0)),
        ("LABEL".into(), json!("")),
    ]);

    let outcome = store
        .import_missing_overrides(PLUGIN_A, &schema, &legacy)
        .unwrap();

    assert!(outcome.changed());
    assert_eq!(
        outcome.imported_keys,
        vec!["COUNT", "ENABLED", "LABEL", "RATE"]
    );
    assert!(outcome.preserved_existing_keys.is_empty());
    assert_eq!(outcome.effective["ENABLED"], json!(false));
    assert_eq!(outcome.effective["COUNT"], json!(0));
    assert_eq!(outcome.effective["RATE"], json!(0.0));
    assert_eq!(outcome.effective["LABEL"], json!(""));
    // Absent key falls back to schema default.
    assert_eq!(outcome.effective["SHOW"], json!(true));

    // Reopen from disk via `read_published` and verify through both store and service.
    let (reopened_store, reopened_service) = open(root.path());
    let raw = reopened_store.load(PLUGIN_A).unwrap();
    assert_eq!(raw.get("ENABLED"), Some(&json!(false)));
    assert_eq!(raw.get("COUNT"), Some(&json!(0)));
    assert_eq!(raw.get("RATE"), Some(&json!(0.0)));
    assert_eq!(raw.get("LABEL"), Some(&json!("")));
    assert!(!raw.contains_key("SHOW"));

    let entries = cbor_entries(outcome.settings_changed_message());
    assert!(entries.contains(&(CborValue::Text("ENABLED".into()), CborValue::Bool(false))));
    assert!(entries.contains(&(
        CborValue::Text("LABEL".into()),
        CborValue::Text(String::new())
    )));

    let effective_cbor = reopened_service
        .effective_settings(PLUGIN_A, Some(&schema))
        .unwrap();
    let CborValue::Map(reopened_entries) = effective_cbor else {
        panic!("expected CBOR map");
    };
    assert!(
        reopened_entries.contains(&(CborValue::Text("ENABLED".into()), CborValue::Bool(false)))
    );
    assert!(reopened_entries.contains(&(
        CborValue::Text("COUNT".into()),
        CborValue::Integer(0.into())
    )));
    assert!(reopened_entries.contains(&(CborValue::Text("RATE".into()), CborValue::Float(0.0))));
    assert!(reopened_entries.contains(&(
        CborValue::Text("LABEL".into()),
        CborValue::Text(String::new())
    )));
    assert!(reopened_entries.contains(&(CborValue::Text("SHOW".into()), CborValue::Bool(true))));
    // Also verify the live service view matches.
    let view = service.dashboard_view(PLUGIN_A, Some(&schema)).unwrap();
    assert_eq!(view["values"]["ENABLED"], json!(false));
    assert_eq!(view["values"]["COUNT"], json!(0));
    assert_eq!(view["values"]["RATE"], json!(0.0));
    assert_eq!(view["values"]["LABEL"], json!(""));
}

/// A8.1 precedence and A8.3 idempotence: existing valid overrides win per
/// explicit key, absent keys are imported, repeated replay is a no-op, and
/// post-import user edits are never overwritten by a later replay.
#[test]
fn existing_valid_overrides_win_absent_keys_import_and_post_import_edits_survive_replay() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    let schema = rich_schema();

    // Seed existing valid overrides (including an explicit `false` override).
    service
        .submit(
            PLUGIN_A,
            &schema,
            vec![
                ("ENABLED".into(), json!(false)),
                ("RATE".into(), json!(250.0)),
            ],
        )
        .unwrap();

    let legacy = Map::from_iter([
        ("ENABLED".into(), json!(true)),
        ("RATE".into(), json!(120.0)),
        ("COUNT".into(), json!(7)),
        ("LABEL".into(), json!("from-legacy")),
    ]);

    let first = service
        .import_missing_overrides(PLUGIN_A, &schema, &legacy)
        .unwrap();
    assert!(first.changed());
    assert_eq!(first.imported_keys, vec!["COUNT", "LABEL"]);
    assert_eq!(first.preserved_existing_keys, vec!["ENABLED", "RATE"]);
    assert_eq!(first.effective["ENABLED"], json!(false));
    assert_eq!(first.effective["RATE"], json!(250.0));
    assert_eq!(first.effective["COUNT"], json!(7));
    assert_eq!(first.effective["LABEL"], json!("from-legacy"));
    assert_eq!(first.effective["SHOW"], json!(true));

    // Crash replay: calling import again must not write or alter any value.
    let bytes_after_first = std::fs::read(store.path()).unwrap();
    let replay = store
        .import_missing_overrides(PLUGIN_A, &schema, &legacy)
        .unwrap();
    assert!(!replay.changed());
    assert!(replay.imported_keys.is_empty());
    assert_eq!(
        replay.preserved_existing_keys,
        vec!["COUNT", "ENABLED", "LABEL", "RATE"]
    );
    assert_eq!(replay.effective, first.effective);
    assert_eq!(std::fs::read(store.path()).unwrap(), bytes_after_first);

    // User edits a previously imported setting; another replay must preserve
    // the user's edit.
    service
        .submit(
            PLUGIN_A,
            &schema,
            vec![("LABEL".into(), json!("user-edited"))],
        )
        .unwrap();
    let after_edit = store
        .import_missing_overrides(PLUGIN_A, &schema, &legacy)
        .unwrap();
    assert!(!after_edit.changed());
    assert!(after_edit.imported_keys.is_empty());
    assert_eq!(after_edit.effective["LABEL"], json!("user-edited"));

    let (_, reopened) = open(root.path());
    let view = reopened.dashboard_view(PLUGIN_A, Some(&schema)).unwrap();
    assert_eq!(view["values"]["ENABLED"], json!(false));
    assert_eq!(view["values"]["RATE"], json!(250.0));
    assert_eq!(view["values"]["COUNT"], json!(7));
    assert_eq!(view["values"]["LABEL"], json!("user-edited"));
}

/// A8.1 specifies that existing *valid* overrides win; a stale stored entry
/// whose type no longer matches the schema does not block importing a valid
/// legacy override for that key.
#[test]
fn stale_invalid_stored_override_does_not_block_valid_legacy_import() {
    let root = tempfile::tempdir().unwrap();
    let (store, _) = open(root.path());
    let schema = rich_schema();

    store
        .store(
            PLUGIN_A,
            &Map::from_iter([
                ("RATE".into(), json!("not-a-number-anymore")),
                ("LABEL".into(), json!("valid-existing")),
            ]),
        )
        .unwrap();

    let legacy = Map::from_iter([
        ("RATE".into(), json!(175.0)),
        ("LABEL".into(), json!("legacy-label")),
    ]);

    let outcome = store
        .import_missing_overrides(PLUGIN_A, &schema, &legacy)
        .unwrap();
    assert_eq!(outcome.imported_keys, vec!["RATE"]);
    assert_eq!(outcome.preserved_existing_keys, vec!["LABEL"]);
    assert_eq!(outcome.effective["RATE"], json!(175.0));
    assert_eq!(outcome.effective["LABEL"], json!("valid-existing"));
}

/// Malformed legacy values fail closed BEFORE any write — never creating a new
/// file, never modifying an existing file, and never echoing secrets.
#[test]
fn malformed_legacy_values_fail_closed_without_creating_or_modifying_store() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    let schema = rich_schema();

    let bad_batches = [
        Map::from_iter([
            ("TOKEN".into(), json!(SECRET)),
            ("RATE".into(), json!("not-a-number")),
        ]),
        Map::from_iter([
            ("TOKEN".into(), json!(SECRET)),
            ("UNKNOWN_KEY".into(), json!(true)),
        ]),
        Map::from_iter([
            ("TOKEN".into(), json!(SECRET)),
            ("SECTION".into(), json!("layout-only")),
        ]),
    ];

    // No file exists yet: every malformed batch must fail without creating one.
    for bad in &bad_batches {
        let error = store
            .import_missing_overrides(PLUGIN_A, &schema, bad)
            .unwrap_err();
        assert!(matches!(error, SettingsError::Schema(_)), "{error:?}");
        assert!(!error.to_string().contains(SECRET), "{error}");
        assert!(!error.to_string().contains("not-a-number"), "{error}");
        assert!(!store.path().exists());
    }

    // Seed valid data, snapshot file bytes, then retry every malformed batch
    // (including a malformed key that already exists in storage).
    service
        .submit(PLUGIN_A, &schema, vec![("RATE".into(), json!(200.0))])
        .unwrap();
    let before = std::fs::read(store.path()).unwrap();

    for bad in &bad_batches {
        let error = service
            .import_missing_overrides(PLUGIN_A, &schema, bad)
            .unwrap_err();
        assert!(matches!(error, SettingsError::Schema(_)), "{error:?}");
        assert!(!error.to_string().contains(SECRET), "{error}");
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        assert_eq!(store.load(PLUGIN_A).unwrap()["RATE"], json!(200.0));
    }
}

/// Corrupt, unsupported-version, unknown-field, and oversized settings files
/// all fail closed on load, store, submit, and import without overwriting the
/// file or leaking raw file contents.
#[test]
fn corrupt_and_oversized_files_fail_closed_without_overwriting_or_leaking_contents() {
    let schema = rich_schema();
    let valid_legacy = Map::from_iter([("RATE".into(), json!(125.0))]);

    let cases: [(&str, Vec<u8>); 4] = [
        ("corrupt json", format!("{{not json {SECRET}").into_bytes()),
        (
            "unsupported version",
            br#"{"version":99,"plugins":{}}"#.to_vec(),
        ),
        (
            "unknown top-level field",
            format!(r#"{{"version":1,"plugins":{{}},"{SECRET}":1}}"#).into_bytes(),
        ),
        (
            "oversized document",
            vec![b' '; (MAX_FILE_BYTES as usize) + 1],
        ),
    ];

    for (label, raw) in cases {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("plugins").join("settings.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &raw).unwrap();

        let (store, service) = open(root.path());

        let load_err = store.load(PLUGIN_A).unwrap_err();
        let store_err = store
            .store(PLUGIN_A, &Map::from_iter([("RATE".into(), json!(200.0))]))
            .unwrap_err();
        let import_store_err = store
            .import_missing_overrides(PLUGIN_A, &schema, &valid_legacy)
            .unwrap_err();
        let import_service_err = service
            .import_missing_overrides(PLUGIN_A, &schema, &valid_legacy)
            .unwrap_err();
        let empty_import_err = store
            .import_missing_overrides(PLUGIN_A, &schema, &Map::new())
            .unwrap_err();

        for err in [
            load_err,
            store_err,
            import_store_err,
            import_service_err,
            empty_import_err,
        ] {
            let SettingsError::Storage(message) = err else {
                panic!("{label}: expected SettingsError::Storage, got {err:?}");
            };
            assert!(
                !message.contains(SECRET),
                "{label}: leaked secret: {message}"
            );
            assert!(
                !message.contains(&root.path().display().to_string()),
                "{label}: leaked local path: {message}"
            );
        }

        // The damaged file must remain byte-for-byte untouched on disk.
        assert_eq!(std::fs::read(&file).unwrap(), raw, "{label}");
    }
}

/// Oversized write is rejected before modifying the existing on-disk document
/// or in-memory store state.
#[test]
fn oversized_write_fails_closed_leaving_prior_state_and_file_intact() {
    let root = tempfile::tempdir().unwrap();
    let (store, _) = open(root.path());
    store
        .store(PLUGIN_A, &Map::from_iter([("LABEL".into(), json!("safe"))]))
        .unwrap();
    let before = std::fs::read(store.path()).unwrap();

    let huge = "x".repeat((MAX_FILE_BYTES as usize) + 128);
    let error = store
        .store(PLUGIN_A, &Map::from_iter([("LABEL".into(), json!(huge))]))
        .unwrap_err();
    assert!(matches!(error, SettingsError::Storage(_)), "{error:?}");
    assert_eq!(store.load(PLUGIN_A).unwrap()["LABEL"], json!("safe"));
    assert_eq!(std::fs::read(store.path()).unwrap(), before);
}

/// S0 counterexample: two distinct plugin IDs sharing the same setting key
/// names never share, collide on, or clobber each other's overrides.
#[test]
fn distinct_plugin_ids_stay_strictly_isolated() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    let schema = rich_schema();

    store
        .import_missing_overrides(
            PLUGIN_A,
            &schema,
            &Map::from_iter([
                ("ENABLED".into(), json!(false)),
                ("RATE".into(), json!(111.0)),
                ("LABEL".into(), json!("alpha-only")),
            ]),
        )
        .unwrap();

    store
        .import_missing_overrides(
            PLUGIN_B,
            &schema,
            &Map::from_iter([
                ("ENABLED".into(), json!(true)),
                ("RATE".into(), json!(222.0)),
                ("LABEL".into(), json!("beta-only")),
            ]),
        )
        .unwrap();

    // Update plugin A; plugin B must remain unchanged.
    service
        .submit(PLUGIN_A, &schema, vec![("RATE".into(), json!(333.0))])
        .unwrap();

    let (reopened_store, reopened_service) = open(root.path());
    assert_eq!(reopened_store.load(PLUGIN_A).unwrap()["RATE"], json!(333.0));
    assert_eq!(reopened_store.load(PLUGIN_B).unwrap()["RATE"], json!(222.0));
    assert_eq!(
        reopened_store.load(PLUGIN_B).unwrap()["LABEL"],
        json!("beta-only")
    );
    assert_eq!(
        reopened_service
            .dashboard_view(PLUGIN_A, Some(&schema))
            .unwrap()["values"]["RATE"],
        json!(333.0)
    );
    assert_eq!(
        reopened_service
            .dashboard_view(PLUGIN_B, Some(&schema))
            .unwrap()["values"]["RATE"],
        json!(222.0)
    );

    // Clearing plugin A must leave plugin B intact.
    reopened_store.store(PLUGIN_A, &Map::new()).unwrap();
    let (_, reopened_again) = open(root.path());
    let view_a = reopened_again
        .dashboard_view(PLUGIN_A, Some(&schema))
        .unwrap();
    let view_b = reopened_again
        .dashboard_view(PLUGIN_B, Some(&schema))
        .unwrap();
    assert_eq!(view_a["values"]["RATE"], json!(150.0));
    assert_eq!(view_b["values"]["RATE"], json!(222.0));
    assert_eq!(view_b["values"]["LABEL"], json!("beta-only"));
}

struct InjectedResolver {
    executable: PathBuf,
    launch: PathBuf,
    env: BTreeMap<String, OsString>,
}

impl PathResolver for InjectedResolver {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }
    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.launch.clone())
    }
    fn env_var(&self, name: &str) -> Option<OsString> {
        self.env.get(name).cloned()
    }
}

/// S0 counterexample: prerelease/alpha `config_root` and normal release
/// `config_root` are completely isolated; writing or importing in alpha never
/// writes to or touches normal-root settings.
#[test]
fn prerelease_and_release_config_roots_are_isolated() {
    let temp = tempfile::tempdir().unwrap();
    let resolver = InjectedResolver {
        executable: temp.path().join("bin").join("vosu"),
        launch: temp.path().to_path_buf(),
        env: BTreeMap::from([
            (
                "XDG_CONFIG_HOME".into(),
                temp.path().join("xdg-config").into_os_string(),
            ),
            (
                "XDG_DATA_HOME".into(),
                temp.path().join("xdg-data").into_os_string(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                temp.path().join("xdg-cache").into_os_string(),
            ),
        ]),
    };

    let release_paths = Paths::resolve(&resolver, BuildChannel::Release).unwrap();
    let prerelease_paths = Paths::resolve(&resolver, BuildChannel::Prerelease).unwrap();
    assert_ne!(release_paths.config(), prerelease_paths.config());

    let release_store = PluginSettingsStore::open(release_paths.config());
    let prerelease_store = PluginSettingsStore::open(prerelease_paths.config());
    assert_ne!(release_store.path(), prerelease_store.path());

    let schema = rich_schema();
    prerelease_store
        .import_missing_overrides(
            PLUGIN_A,
            &schema,
            &Map::from_iter([("LABEL".into(), json!("alpha-sentinel"))]),
        )
        .unwrap();

    // Release root must remain completely untouched by the alpha write.
    assert!(!release_store.path().exists());
    assert!(
        PluginSettingsStore::open(release_paths.config())
            .load(PLUGIN_A)
            .unwrap()
            .is_empty()
    );

    // Writing to release root must not alter alpha settings.
    release_store
        .store(
            PLUGIN_A,
            &Map::from_iter([("LABEL".into(), json!("release-sentinel"))]),
        )
        .unwrap();
    assert_eq!(
        PluginSettingsStore::open(release_paths.config())
            .load(PLUGIN_A)
            .unwrap()["LABEL"],
        json!("release-sentinel")
    );
    assert_eq!(
        PluginSettingsStore::open(prerelease_paths.config())
            .load(PLUGIN_A)
            .unwrap()["LABEL"],
        json!("alpha-sentinel")
    );
}

/// Repeated `atomic_replace` writes round-trip cleanly through `read_published`
/// without leaving stray temp files behind in the plugins directory.
#[test]
fn atomic_replace_and_read_published_round_trip_leaves_no_temp_files() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    let schema = rich_schema();

    for step in 1..=5 {
        service
            .submit(
                PLUGIN_A,
                &schema,
                vec![
                    ("COUNT".into(), json!(step)),
                    ("LABEL".into(), json!(format!("rev-{step}"))),
                ],
            )
            .unwrap();
        let reopened = PluginSettingsStore::open(root.path());
        assert_eq!(reopened.load(PLUGIN_A).unwrap()["COUNT"], json!(step));
        assert_eq!(
            reopened.load(PLUGIN_A).unwrap()["LABEL"],
            json!(format!("rev-{step}"))
        );
    }

    let entries: Vec<_> = std::fs::read_dir(store.path().parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![OsString::from("settings.json")]);
}

/// Failed durable storage write must fail `import_missing_overrides` and
/// `submit` before returning an outcome or `SettingsChanged` message.
#[test]
fn failed_durable_write_returns_error_without_publishing_outcome() {
    struct FlakyStorage {
        inner: PluginSettingsStore,
        fail_writes: AtomicBool,
    }

    impl SettingsStorage for FlakyStorage {
        fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
            self.inner.load(plugin_id)
        }
        fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError> {
            if self.fail_writes.load(Ordering::SeqCst) {
                return Err(SettingsError::Storage("injected write failure".into()));
            }
            self.inner.store(plugin_id, values)
        }
    }

    let root = tempfile::tempdir().unwrap();
    let storage = Arc::new(FlakyStorage {
        inner: PluginSettingsStore::open(root.path()),
        fail_writes: AtomicBool::new(true),
    });
    let service = PluginSettingsService::new(storage.clone());
    let schema = rich_schema();

    let legacy = Map::from_iter([("ENABLED".into(), json!(false))]);
    let error = service
        .import_missing_overrides(PLUGIN_A, &schema, &legacy)
        .unwrap_err();
    assert!(matches!(error, SettingsError::Storage(_)));
    assert!(!storage.inner.path().exists());
    assert!(storage.inner.load(PLUGIN_A).unwrap().is_empty());

    // Once storage recovers, the same import succeeds cleanly.
    storage.fail_writes.store(false, Ordering::SeqCst);
    let outcome = service
        .import_missing_overrides(PLUGIN_A, &schema, &legacy)
        .unwrap();
    assert_eq!(outcome.imported_keys, vec!["ENABLED"]);
    assert_eq!(outcome.effective["ENABLED"], json!(false));
}

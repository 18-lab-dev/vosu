use std::sync::Arc;

use serde_json::{Value, json};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{CborValue, Message};

use super::super::{PluginSettingsService, SettingsError, SettingsStorage};
use super::PluginSettingsStore;

const ID: &str = "acme.settings";
const SECRET: &str = "sentinel-plugin-password-7c1e";

fn schema() -> SettingsSchema {
    SettingsSchema::parse(
        r#"[
            {"uniqueID":"LABEL","type":"text","title":"Label","value":"hello"},
            {"uniqueID":"RATE","type":"number","title":"Rate","value":150},
            {"uniqueID":"TOKEN","type":"password","title":"Token","value":"built-in default"},
            {"uniqueID":"NOTE","type":"header","title":"Section"}
        ]"#,
    )
    .unwrap()
}

fn open(root: &std::path::Path) -> (Arc<PluginSettingsStore>, PluginSettingsService) {
    let store = Arc::new(PluginSettingsStore::open(root));
    let service = PluginSettingsService::new(store.clone() as Arc<dyn SettingsStorage>);
    (store, service)
}

fn values(message: Message) -> Vec<(CborValue, CborValue)> {
    let Message::SettingsChanged(changed) = message else {
        panic!("expected SettingsChanged");
    };
    let CborValue::Map(entries) = changed.values else {
        panic!("expected a CBOR map");
    };
    entries
}

fn text(key: &str, value: &str) -> (CborValue, CborValue) {
    (CborValue::Text(key.into()), CborValue::Text(value.into()))
}

#[test]
fn defaults_apply_and_passwords_never_leave_the_view() {
    let root = tempfile::tempdir().unwrap();
    let (_, service) = open(root.path());
    let view = service.dashboard_view(ID, Some(&schema())).unwrap();
    assert_eq!(
        view["values"],
        json!({"LABEL":"hello","RATE":150,"TOKEN":""})
    );
    // The built-in default counts as a secret in effect, but is never shown.
    assert_eq!(view["secrets"], json!({"TOKEN": true}));
    assert_eq!(view["schema"].as_array().unwrap().len(), 4);
    assert_eq!(view["schema"][2]["value"], "");
    assert!(!view.to_string().contains("built-in default"));
    assert_eq!(
        service.dashboard_view(ID, None).unwrap(),
        json!({"schema": [], "values": {}, "secrets": {}})
    );
}

#[test]
fn a_valid_submit_stores_overrides_and_returns_one_complete_object() {
    let root = tempfile::tempdir().unwrap();
    let (_, service) = open(root.path());
    let message = service
        .submit(
            ID,
            &schema(),
            vec![
                ("LABEL".into(), json!("set")),
                ("TOKEN".into(), json!(SECRET)),
            ],
        )
        .unwrap();
    let entries = values(message);
    assert_eq!(entries.len(), 3, "complete effective object: {entries:?}");
    assert!(entries.contains(&text("LABEL", "set")));
    assert!(entries.contains(&text("TOKEN", SECRET)));
    // Persisted across a reopen; still never shown.
    let (_, reopened) = open(root.path());
    let view = reopened.dashboard_view(ID, Some(&schema())).unwrap();
    assert_eq!(view["values"]["LABEL"], "set");
    assert_eq!(view["values"]["TOKEN"], "");
    assert_eq!(view["secrets"]["TOKEN"], true);
    assert!(!view.to_string().contains(SECRET));
}

#[test]
fn blank_keeps_and_null_clears_a_password() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    service
        .submit(ID, &schema(), vec![("TOKEN".into(), json!(SECRET))])
        .unwrap();
    service
        .submit(ID, &schema(), vec![("TOKEN".into(), json!(""))])
        .unwrap();
    assert_eq!(store.load(ID).unwrap()["TOKEN"], SECRET);
    let entries = values(
        service
            .submit(ID, &schema(), vec![("TOKEN".into(), Value::Null)])
            .unwrap(),
    );
    assert!(entries.contains(&text("TOKEN", "")));
    let view = service.dashboard_view(ID, Some(&schema())).unwrap();
    assert_eq!(view["secrets"]["TOKEN"], false);
}

/// D1 counterexample: a bad setting must not change the plugin's
/// effective object, and a failed save never echoes the password.
#[test]
fn an_invalid_submit_changes_nothing_and_never_echoes_the_password() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    service
        .submit(ID, &schema(), vec![("LABEL".into(), json!("kept"))])
        .unwrap();
    let before = std::fs::read(store.path()).unwrap();
    for bad in [
        vec![
            ("TOKEN".into(), json!(SECRET)),
            ("RATE".into(), json!("fast")),
        ],
        vec![
            ("TOKEN".into(), json!(SECRET)),
            ("UNKNOWN".into(), json!(1)),
        ],
        vec![("TOKEN".into(), json!(SECRET)), ("NOTE".into(), json!("x"))],
    ] {
        let error = service.submit(ID, &schema(), bad).unwrap_err();
        assert!(matches!(error, SettingsError::Schema(_)), "{error:?}");
        assert!(!error.to_string().contains(SECRET), "{error}");
    }
    assert_eq!(std::fs::read(store.path()).unwrap(), before);
    let view = service.dashboard_view(ID, Some(&schema())).unwrap();
    assert_eq!(
        view["values"],
        json!({"LABEL":"kept","RATE":150,"TOKEN":""})
    );
    assert!(!view.to_string().contains(SECRET));
}

#[test]
fn a_stale_stored_key_falls_back_alone() {
    let root = tempfile::tempdir().unwrap();
    let (store, service) = open(root.path());
    store
        .store(
            ID,
            &serde_json::Map::from_iter([
                ("LABEL".into(), json!("kept")),
                ("RATE".into(), json!("no longer a number")),
                ("REMOVED".into(), json!(true)),
            ]),
        )
        .unwrap();
    let entries = values(
        service
            .submit(ID, &schema(), vec![("TOKEN".into(), json!(""))])
            .unwrap(),
    );
    assert!(entries.contains(&text("LABEL", "kept")));
    assert!(entries.contains(&(
        CborValue::Text("RATE".into()),
        CborValue::Integer(150.into())
    )));
}

#[test]
fn an_unreadable_document_is_never_rewritten() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("plugins")).unwrap();
    std::fs::write(root.path().join("plugins/settings.json"), b"{not json").unwrap();
    let (store, service) = open(root.path());
    assert!(matches!(store.load(ID), Err(SettingsError::Storage(_))));
    assert!(matches!(
        service.submit(ID, &schema(), vec![("LABEL".into(), json!("x"))]),
        Err(SettingsError::Storage(_))
    ));
    assert_eq!(
        std::fs::read(root.path().join("plugins/settings.json")).unwrap(),
        b"{not json"
    );
}

struct Managed;

impl SettingsStorage for Managed {
    fn load(&self, _: &str) -> Result<serde_json::Map<String, Value>, SettingsError> {
        Ok(serde_json::Map::new())
    }
    fn store(&self, _: &str, _: &serde_json::Map<String, Value>) -> Result<(), SettingsError> {
        panic!("a managed id is never stored through the plugin settings route")
    }
    fn editable(&self, _: &str) -> bool {
        false
    }
}

#[test]
fn a_bridged_id_is_refused_before_anything_is_stored() {
    let service = PluginSettingsService::new(Arc::new(Managed));
    assert_eq!(
        service
            .submit(ID, &schema(), vec![("LABEL".into(), json!("x"))])
            .unwrap_err(),
        SettingsError::ManagedElsewhere(ID.into())
    );
}

#[test]
fn post_read_size_limit_rejects_oversized_payload_even_after_read() {
    let payload = br#"{"version":1,"plugins":{}}"#;
    assert_eq!(
        super::parse_bytes_with_limit(payload, (payload.len() as u64) - 1),
        Err(super::ReadFailure::Oversized)
    );
    assert!(super::parse_bytes_with_limit(payload, payload.len() as u64).is_ok());
}

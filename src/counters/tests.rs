use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[derive(Default)]
struct RecordingOpener(Mutex<Vec<PathBuf>>);
impl CounterOpener for RecordingOpener {
    fn open(&self, path: &Path) -> io::Result<()> {
        lock(&self.0).push(path.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct RecordingDownloader(Mutex<Vec<DownloadRequest>>);
impl CounterDownloader for RecordingDownloader {
    fn download_and_install(
        &self,
        request: DownloadRequest,
        destination: PathBuf,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), CounterError>> + Send + 'static>> {
        lock(&self.0).push(request);
        Box::pin(async move {
            std::fs::create_dir_all(&destination)?;
            std::fs::write(destination.join("index.html"), "ok")?;
            Ok(())
        })
    }
}

fn fixture() -> (
    tempfile::TempDir,
    CounterService,
    Arc<RecordingOpener>,
    Arc<RecordingDownloader>,
) {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    std::fs::create_dir(&static_root).unwrap();
    let opener = Arc::new(RecordingOpener::default());
    let downloader = Arc::new(RecordingDownloader::default());
    let service = CounterService::new(
        static_root,
        root.path().join("settings"),
        opener.clone(),
        downloader.clone(),
    );
    (root, service, opener, downloader)
}

fn all_settings() -> Vec<CounterSetting> {
    [
        ("text", SettingType::Text, json!("hello")),
        ("color", SettingType::Color, json!("#fff")),
        ("number", SettingType::Number, json!(12)),
        ("checkbox", SettingType::Checkbox, json!(false)),
        ("button", SettingType::Button, json!("press")),
        ("options", SettingType::Options, json!("one")),
        ("commands", SettingType::Commands, json!(["a"])),
        ("textarea", SettingType::Textarea, json!("many")),
        ("password", SettingType::Password, json!("secret")),
        ("header", SettingType::Header, Value::Null),
    ]
    .into_iter()
    .map(|(id, kind, value)| CounterSetting {
        unique_id: id.into(),
        unique_check: None,
        kind,
        title: id.into(),
        options: None,
        description: String::new(),
        value,
    })
    .collect()
}

#[test]
fn discovers_metadata_html_order_and_immediate_ignore_scope() {
    let (root, service, _, _) = fixture();
    let static_root = root.path().join("static");
    let ignored = static_root.join("ignored");
    let nested_ignored = static_root.join("parent/deep");
    let metadata_counter = static_root.join("meta");
    let local = static_root.join("local");
    for path in [&ignored, &nested_ignored, &metadata_counter, &local] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(ignored.join("ignore.txt"), "").unwrap();
    std::fs::write(ignored.join("index.html"), "").unwrap();
    std::fs::write(nested_ignored.join("ignore.txt"), "").unwrap();
    std::fs::write(nested_ignored.join("index.html"), "").unwrap();
    std::fs::write(metadata_counter.join("metadata.txt"), "Name: Fancy ## comment\r\nAuthor: Alice\nVersion:\nResolution: 1920 x 1080 ## pixels\nAuthorLinks: https://a,https://b\nUnknown line").unwrap();
    std::fs::write(metadata_counter.join("index.html"), "").unwrap();
    std::fs::write(local.join("index.html"), "").unwrap();
    let counters = service.discover().unwrap();
    assert_eq!(counters.len(), 3);
    assert_eq!(counters[0].name, "Fancy");
    assert!(
        !counters
            .iter()
            .any(|counter| counter.folder_name == "ignored")
    );
    assert!(
        counters
            .iter()
            .any(|counter| counter.folder_name == "parent/deep")
    );
    let meta = counters
        .iter()
        .find(|counter| counter.name == "Fancy")
        .unwrap();
    assert_eq!(meta.author, "Alice");
    assert_eq!(meta.version, "1.0");
    assert_eq!(meta.resolution, vec![json!(1920), json!(1080)]);
    assert_eq!(meta.authorlinks, vec!["https://a", "https://b"]);
    assert_eq!(
        counters
            .iter()
            .filter(|counter| counter.folder_name == "meta")
            .count(),
        1
    );
}

#[test]
fn discovery_skips_counters_nested_under_node_modules() {
    let (root, service, _, _) = fixture();
    let overlay = root.path().join("static/overlay");
    let dependency = overlay.join("node_modules/dependency");
    std::fs::create_dir_all(&dependency).unwrap();
    std::fs::write(overlay.join("index.html"), "").unwrap();
    std::fs::write(dependency.join("index.html"), "").unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["overlay"]);
}

#[cfg(unix)]
#[test]
fn discovery_follows_outside_directory_symlink_under_alias_name() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("index.html"), "").unwrap();
    symlink(outside.path(), root.path().join("static/alias")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["alias"]);
}

#[cfg(unix)]
#[test]
fn discovery_mounted_alias_skips_descendant_directory_symlink_that_escapes_target() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let alias_target = tempfile::tempdir().unwrap();
    let escaped = tempfile::tempdir().unwrap();
    std::fs::write(alias_target.path().join("index.html"), "").unwrap();
    std::fs::write(escaped.path().join("index.html"), "").unwrap();
    symlink(escaped.path(), alias_target.path().join("unservable")).unwrap();
    symlink(alias_target.path(), root.path().join("static/alias")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["alias"]);
}

#[cfg(unix)]
#[test]
fn discovery_does_not_promote_directory_alias_nested_under_ordinary_counter() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let ordinary = root.path().join("static/ordinary");
    let nested_target = tempfile::tempdir().unwrap();
    std::fs::create_dir(&ordinary).unwrap();
    std::fs::write(ordinary.join("index.html"), "").unwrap();
    std::fs::write(nested_target.path().join("index.html"), "").unwrap();
    symlink(nested_target.path(), ordinary.join("nested-alias")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["ordinary"]);
}

#[cfg(unix)]
#[test]
fn discovery_stops_top_level_directory_symlink_cycles() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let overlay = root.path().join("static/overlay");
    std::fs::create_dir(&overlay).unwrap();
    std::fs::write(overlay.join("index.html"), "").unwrap();
    symlink(root.path().join("static"), root.path().join("static/loop")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["overlay"]);
}

#[cfg(unix)]
#[test]
fn discovery_skips_unresolvable_and_file_symlinks() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let static_root = root.path().join("static");
    let overlay = static_root.join("overlay");
    let linked_file = static_root.join("linked-file");
    std::fs::create_dir(&overlay).unwrap();
    std::fs::create_dir(&linked_file).unwrap();
    std::fs::write(overlay.join("index.html"), "").unwrap();
    std::fs::write(root.path().join("outside-index.html"), "").unwrap();
    symlink(
        root.path().join("outside-index.html"),
        linked_file.join("index.html"),
    )
    .unwrap();
    symlink(root.path().join("missing"), static_root.join("dangling")).unwrap();
    symlink("loop-b", static_root.join("loop-a")).unwrap();
    symlink("loop-a", static_root.join("loop-b")).unwrap();

    let folder_names = service
        .discover()
        .unwrap()
        .into_iter()
        .map(|counter| counter.folder_name)
        .collect::<Vec<_>>();

    assert_eq!(folder_names, ["overlay"]);
}

#[test]
fn legacy_root_json_creates_authoritative_schema_without_modifying_root_sources() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let legacy_schema = serde_json::to_vec_pretty(&all_settings()).unwrap();
    let legacy_values = br#"{
  "text": "legacy",
  "header": null
}
"#;
    std::fs::write(folder.join("settings.json"), &legacy_schema).unwrap();
    std::fs::write(folder.join("settings.values.json"), legacy_values).unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(settings, all_settings());
    assert_eq!(values["text"], "legacy");
    assert!(!values.contains_key("header"));
    assert_eq!(
        std::fs::read(folder.join("settings.json")).unwrap(),
        legacy_schema
    );
    assert_eq!(
        std::fs::read(folder.join("settings.values.json")).unwrap(),
        legacy_values
    );
    assert!(!folder.join("settings.old-values.json").exists());
    assert_eq!(
        service.settings().schema_path("counter"),
        folder.join("settings.schema.toml")
    );
    assert_eq!(
        service
            .settings()
            .values_path("counter")
            .file_name()
            .unwrap(),
        "counter.settings.values.toml"
    );

    let schema: toml::Value = toml::from_str(
        &std::fs::read_to_string(service.settings().schema_path("counter")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        schema
            .as_table()
            .and_then(|table| table.get("settings"))
            .and_then(toml::Value::as_array)
            .map(Vec::len),
        Some(10)
    );
    let values: toml::Value = toml::from_str(
        &std::fs::read_to_string(service.settings().values_path("counter")).unwrap(),
    )
    .unwrap();
    let values = values.as_table().unwrap();
    assert_eq!(
        values.get("text").and_then(toml::Value::as_str),
        Some("legacy")
    );
    assert!(!values.contains_key("header"));
}

#[test]
fn root_json_defines_schema_while_root_toml_seeds_values() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let json_settings = vec![CounterSetting {
        unique_id: "shared".into(),
        unique_check: None,
        kind: SettingType::Text,
        title: "Shared".into(),
        options: None,
        description: String::new(),
        value: json!("json-default"),
    }];
    let root_json = serde_json::to_vec_pretty(&json_settings).unwrap();
    let root_toml = b"shared = true\nextra = 42\n";
    std::fs::write(folder.join("settings.json"), &root_json).unwrap();
    std::fs::write(folder.join("settings.toml"), root_toml).unwrap();

    let settings_root = service.settings().settings_root().to_owned();
    std::fs::create_dir_all(&settings_root).unwrap();
    let prior_xdg_toml = settings_root.join("counter.settings.toml");
    let prior_xdg_json = settings_root.join("counter.settings.json");
    std::fs::write(&prior_xdg_toml, "settings = [").unwrap();
    std::fs::write(&prior_xdg_json, "not JSON").unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(settings, json_settings);
    assert_eq!(values["shared"], true);
    assert_eq!(values["extra"], 42);
    assert_eq!(
        std::fs::read(folder.join("settings.json")).unwrap(),
        root_json
    );
    assert_eq!(
        std::fs::read(folder.join("settings.toml")).unwrap(),
        root_toml
    );
    assert_eq!(std::fs::read(&prior_xdg_toml).unwrap(), b"settings = [");
    assert_eq!(std::fs::read(&prior_xdg_json).unwrap(), b"not JSON");
}

#[test]
fn root_toml_infers_scalar_schema_and_seeds_values() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let root_toml = b"text = \"hello\"\ninteger = 7\nfraction = 2.5\nenabled = true\n";
    std::fs::write(folder.join("settings.toml"), root_toml).unwrap();

    let settings_root = service.settings().settings_root().to_owned();
    std::fs::create_dir_all(&settings_root).unwrap();
    let prior_xdg_toml = settings_root.join("counter.settings.toml");
    let prior_xdg_json = settings_root.join("counter.settings.json");
    std::fs::write(&prior_xdg_toml, "settings = [").unwrap();
    std::fs::write(&prior_xdg_json, "not JSON").unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(settings.len(), 4);
    let setting = |id| {
        settings
            .iter()
            .find(|setting| setting.unique_id == id)
            .unwrap()
    };
    assert_eq!(setting("text").kind, SettingType::Text);
    assert_eq!(setting("text").title, "text");
    assert_eq!(setting("text").value, "hello");
    assert_eq!(setting("integer").kind, SettingType::Number);
    assert_eq!(setting("integer").value, 7);
    assert_eq!(setting("fraction").kind, SettingType::Number);
    assert_eq!(setting("fraction").value, json!(2.5));
    assert_eq!(setting("enabled").kind, SettingType::Checkbox);
    assert_eq!(setting("enabled").value, true);
    assert_eq!(values["text"], "hello");
    assert_eq!(values["integer"], 7);
    assert_eq!(values["fraction"], json!(2.5));
    assert_eq!(values["enabled"], true);
    assert_eq!(
        std::fs::read(folder.join("settings.toml")).unwrap(),
        root_toml
    );
    assert_eq!(std::fs::read(&prior_xdg_toml).unwrap(), b"settings = [");
    assert_eq!(std::fs::read(&prior_xdg_json).unwrap(), b"not JSON");
    assert!(service.settings().schema_path("counter").exists());
    assert!(service.settings().values_path("counter").exists());
}

#[test]
fn root_toml_inference_rejects_composites_and_datetime_without_writing_schema() {
    let (root, service, _, _) = fixture();
    for (name, source) in [
        ("array", "value = [1, 2]\n"),
        ("table", "value = { nested = 1 }\n"),
        ("datetime", "value = 1979-05-27T07:32:00Z\n"),
    ] {
        let folder = root.path().join("static").join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let source_path = folder.join("settings.toml");
        std::fs::write(&source_path, source).unwrap();

        assert!(matches!(
            service.settings().load(name),
            Err(SettingsError::TomlConversion(_))
        ));
        assert_eq!(std::fs::read(&source_path).unwrap(), source.as_bytes());
        assert!(!service.settings().schema_path(name).exists());
        assert!(!service.settings().values_path(name).exists());
    }
}

#[test]
fn prior_xdg_toml_schema_migrates_to_root_without_modifying_sources() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let settings_root = service.settings().settings_root().to_owned();
    std::fs::create_dir_all(&settings_root).unwrap();
    let prior_xdg_toml = settings_root.join("counter.settings.toml");
    let prior_xdg_json = settings_root.join("counter.settings.json");
    let xdg_schema = br#"[[settings]]
uniqueID = "xdg"
type = "number"
title = "XDG"
value = 3
"#;
    std::fs::write(&prior_xdg_toml, xdg_schema).unwrap();
    std::fs::write(&prior_xdg_json, "not JSON").unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(settings.len(), 1);
    assert_eq!(settings[0].unique_id, "xdg");
    assert_eq!(settings[0].kind, SettingType::Number);
    assert_eq!(values["xdg"], 3);
    assert_eq!(
        service.settings().schema_path("counter"),
        folder.join("settings.schema.toml")
    );
    assert!(service.settings().schema_path("counter").exists());
    assert_eq!(std::fs::read(&prior_xdg_toml).unwrap(), xdg_schema);
    assert_eq!(std::fs::read(&prior_xdg_json).unwrap(), b"not JSON");
}

#[test]
fn authoritative_root_schema_wins_and_seeds_coerced_defaults() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let native_schema = br#"[[settings]]
uniqueID = "native-number"
type = "number"
value = "7.5"

[[settings]]
uniqueID = "native-check"
type = "checkbox"
value = "true"

[[settings]]
uniqueID = "heading"
type = "header"

[[settings]]
uniqueID = "unset-number"
type = "number"
"#;
    let legacy_schema = serde_json::to_vec(&all_settings()).unwrap();
    std::fs::write(folder.join("settings.schema.toml"), native_schema).unwrap();
    std::fs::write(folder.join("settings.json"), &legacy_schema).unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(
        settings
            .iter()
            .map(|setting| setting.unique_id.as_str())
            .collect::<Vec<_>>(),
        ["native-number", "native-check", "heading", "unset-number"]
    );
    assert_eq!(settings[0].value, json!(7.5));
    assert_eq!(settings[1].value, json!(true));
    assert_eq!(settings[2].value, Value::Null);
    assert_eq!(settings[3].value, Value::Null);
    assert_eq!(values["native-number"], json!(7.5));
    assert_eq!(values["native-check"], true);
    assert!(!values.contains_key("heading"));
    assert!(!values.contains_key("unset-number"));
    assert_eq!(
        std::fs::read(folder.join("settings.schema.toml")).unwrap(),
        native_schema
    );
    assert_eq!(
        std::fs::read(folder.join("settings.json")).unwrap(),
        legacy_schema
    );
    assert_eq!(
        service.settings().schema_path("counter"),
        folder.join("settings.schema.toml")
    );
    service
        .settings()
        .save_user_values(
            "counter",
            &[SettingInput {
                unique_id: "native-number".into(),
                value: json!("9.25"),
            }],
        )
        .unwrap();
    let (_, reloaded) = service.settings().load("counter").unwrap();
    assert_eq!(reloaded["native-number"], json!(9.25));
    assert!(!reloaded.contains_key("nativenumber"));
    assert_eq!(
        std::fs::read(folder.join("settings.schema.toml")).unwrap(),
        native_schema
    );
}

#[test]
fn paired_native_values_toml_precedes_json_values_without_mutating_sources() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let native_schema = br#"[[settings]]
uniqueID = "number"
type = "number"
value = 1
"#;
    let native_values = b"number = 8\n";
    let root_values = br#"{"number":6}"#;
    std::fs::write(folder.join("settings.schema.toml"), native_schema).unwrap();
    std::fs::write(folder.join("settings.toml"), native_values).unwrap();
    std::fs::write(folder.join("settings.values.json"), root_values).unwrap();
    std::fs::create_dir_all(service.settings().settings_root()).unwrap();
    let internal_values = service
        .settings()
        .settings_root()
        .join("counter.values.json");
    std::fs::write(&internal_values, br#"{"number":7}"#).unwrap();

    let (_, values) = service.settings().load("counter").unwrap();

    assert_eq!(values["number"], 8);
    assert!(internal_values.exists());
    assert_eq!(
        std::fs::read(folder.join("settings.schema.toml")).unwrap(),
        native_schema
    );
    assert_eq!(
        std::fs::read(folder.join("settings.toml")).unwrap(),
        native_values
    );
    assert_eq!(
        std::fs::read(folder.join("settings.values.json")).unwrap(),
        root_values
    );
}

#[test]
fn internal_legacy_json_fallback_migrates_without_modifying_sources() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let settings_root = service.settings().settings_root();
    std::fs::create_dir_all(settings_root).unwrap();
    let internal_schema = settings_root.join("counter.settings.json");
    let internal_values = settings_root.join("counter.values.json");
    let legacy_schema = serde_json::to_vec(&all_settings()).unwrap();
    let legacy_values = br#"{"text":"central","header":null}"#;
    std::fs::write(&internal_schema, &legacy_schema).unwrap();
    std::fs::write(&internal_values, legacy_values).unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(settings, all_settings());
    assert_eq!(values["text"], "central");
    assert!(!values.contains_key("header"));
    assert_eq!(std::fs::read(&internal_schema).unwrap(), legacy_schema);
    assert_eq!(std::fs::read(&internal_values).unwrap(), legacy_values);
    assert!(service.settings().schema_path("counter").exists());
    assert!(service.settings().values_path("counter").exists());
}

#[test]
fn missing_schema_migration_preserves_existing_authoritative_values() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("settings.json"),
        serde_json::to_vec(&all_settings()).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(service.settings().settings_root()).unwrap();
    std::fs::write(
        service.settings().values_path("counter"),
        "text = \"preserved\"\n",
    )
    .unwrap();
    let legacy_values = service
        .settings()
        .settings_root()
        .join("counter.values.json");
    std::fs::write(&legacy_values, br#"{"text":"legacy"}"#).unwrap();

    let (_, values) = service.settings().load("counter").unwrap();

    assert_eq!(values["text"], "preserved");
    assert!(service.settings().schema_path("counter").exists());
    assert!(legacy_values.exists());
}

#[test]
fn root_schema_and_config_values_ignore_every_obsolete_source() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    service
        .settings()
        .save_developer_settings("counter", all_settings())
        .unwrap();
    service
        .settings()
        .save_user_values(
            "counter",
            &[SettingInput {
                unique_id: "text".into(),
                value: json!("authoritative"),
            }],
        )
        .unwrap();
    let root_schema = std::fs::read(service.settings().schema_path("counter")).unwrap();

    std::fs::write(folder.join("settings.toml"), "value = [").unwrap();
    std::fs::write(folder.join("settings.json"), "not JSON").unwrap();
    std::fs::write(folder.join("settings.values.json"), "not JSON").unwrap();
    let prior_xdg_toml = service
        .settings()
        .settings_root()
        .join("counter.settings.toml");
    let internal_schema = service
        .settings()
        .settings_root()
        .join("counter.settings.json");
    let internal_values = service
        .settings()
        .settings_root()
        .join("counter.values.json");
    std::fs::write(&prior_xdg_toml, "settings = [").unwrap();
    std::fs::write(&internal_schema, "not JSON").unwrap();
    std::fs::write(&internal_values, "not JSON").unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(settings, all_settings());
    assert_eq!(values["text"], "authoritative");
    assert_eq!(
        std::fs::read(service.settings().schema_path("counter")).unwrap(),
        root_schema
    );
    assert_eq!(std::fs::read(&prior_xdg_toml).unwrap(), b"settings = [");
    assert_eq!(std::fs::read(&internal_schema).unwrap(), b"not JSON");
    assert_eq!(std::fs::read(&internal_values).unwrap(), b"not JSON");
}

#[test]
fn schema_shaped_root_toml_never_seeds_compact_values() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    service
        .settings()
        .save_developer_settings("counter", all_settings())
        .unwrap();
    let obsolete_schema = br#"[[settings]]
uniqueID = "obsolete"
type = "text"
value = "wrong"
"#;
    std::fs::write(folder.join("settings.toml"), obsolete_schema).unwrap();
    std::fs::create_dir_all(service.settings().settings_root()).unwrap();
    let internal_values = service
        .settings()
        .settings_root()
        .join("counter.values.json");
    std::fs::write(&internal_values, br#"{"text":"fallback"}"#).unwrap();

    let (settings, values) = service.settings().load("counter").unwrap();

    assert_eq!(settings, all_settings());
    assert_eq!(values["text"], "fallback");
    assert!(!values.contains_key("settings"));
    assert_eq!(
        std::fs::read(&internal_values).unwrap(),
        br#"{"text":"fallback"}"#
    );
    assert_eq!(
        std::fs::read(folder.join("settings.toml")).unwrap(),
        obsolete_schema
    );
}

#[cfg(unix)]
#[test]
fn settings_rejects_symlinked_root_and_unmigrated_xdg_schema() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let external = root.path().join("external-settings.schema.toml");
    std::fs::write(
        &external,
        "[[settings]]\nuniqueID = \"safe\"\ntype = \"text\"\n",
    )
    .unwrap();

    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    symlink(&external, folder.join("settings.schema.toml")).unwrap();
    assert!(matches!(
        service.settings().load("counter"),
        Err(SettingsError::UnsafeSettingsFile)
    ));
    assert!(!service.settings().values_path("counter").exists());

    let other_folder = root.path().join("static/other");
    std::fs::create_dir_all(&other_folder).unwrap();
    std::fs::create_dir_all(service.settings().settings_root()).unwrap();
    symlink(
        &external,
        service
            .settings()
            .settings_root()
            .join("other.settings.toml"),
    )
    .unwrap();
    assert!(matches!(
        service.settings().load("other"),
        Err(SettingsError::UnsafeSettingsFile)
    ));
    assert!(!service.settings().schema_path("other").exists());
    assert!(!service.settings().values_path("other").exists());
}

#[test]
fn toml_round_trips_schema_values_and_coercion_without_encoding_null() {
    let (root, service, _, _) = fixture();
    std::fs::create_dir_all(root.path().join("static/counter")).unwrap();
    let mut settings = all_settings();
    settings[0].options = Some(json!({"labels": ["a", "b"], "enabled": true}));
    settings[2].value = json!("9.25");
    settings[3].value = json!("true");

    let saved = service
        .settings()
        .save_developer_settings("counter", settings)
        .unwrap();
    assert_eq!(saved[2].value, json!(9.25));
    assert_eq!(saved[3].value, true);
    let (reloaded, _) = service.settings().load("counter").unwrap();
    assert_eq!(reloaded, saved);
    assert_eq!(reloaded[9].value, Value::Null);

    let schema: toml::Value = toml::from_str(
        &std::fs::read_to_string(service.settings().schema_path("counter")).unwrap(),
    )
    .unwrap();
    let rows = schema
        .as_table()
        .and_then(|table| table.get("settings"))
        .and_then(toml::Value::as_array)
        .unwrap();
    let header = rows
        .iter()
        .filter_map(toml::Value::as_table)
        .find(|row| row.get("uniqueID").and_then(toml::Value::as_str) == Some("header"))
        .unwrap();
    assert!(!header.contains_key("value"));

    let persisted = service
        .settings()
        .save_user_values(
            "counter",
            &[
                SettingInput {
                    unique_id: "number".into(),
                    value: json!("7.5"),
                },
                SettingInput {
                    unique_id: "checkbox".into(),
                    value: json!("true"),
                },
                SettingInput {
                    unique_id: "text".into(),
                    value: Value::Null,
                },
            ],
        )
        .unwrap();
    assert_eq!(persisted["number"], json!(7.5));
    assert_eq!(persisted["checkbox"], true);
    assert!(!persisted.contains_key("text"));
    let values: toml::Value = toml::from_str(
        &std::fs::read_to_string(service.settings().values_path("counter")).unwrap(),
    )
    .unwrap();
    let values = values.as_table().unwrap();
    assert_eq!(
        values.get("number").and_then(toml::Value::as_float),
        Some(7.5)
    );
    assert_eq!(
        values.get("checkbox").and_then(toml::Value::as_bool),
        Some(true)
    );
    assert!(!values.contains_key("text"));
    assert_eq!(
        service.settings().counter_values("counter").unwrap()["text"],
        "hello"
    );

    let schema_before = std::fs::read(service.settings().schema_path("counter")).unwrap();
    let mut unsupported = all_settings();
    unsupported[0].value = json!({"nested": null});
    assert!(matches!(
        service
            .settings()
            .save_developer_settings("counter", unsupported),
        Err(SettingsError::TomlConversion(_))
    ));
    assert_eq!(
        std::fs::read(service.settings().schema_path("counter")).unwrap(),
        schema_before
    );
}

#[test]
fn developer_update_requires_yes_and_writes_only_root_schema() {
    let (root, service, _, _) = fixture();
    let folder = root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    let legacy_schema = br#"[
  {"uniqueID":"legacy","type":"text","value":"unchanged"}
]
"#;
    std::fs::write(folder.join("settings.json"), legacy_schema).unwrap();
    std::fs::create_dir_all(service.settings().settings_root()).unwrap();
    let prior_xdg_toml = service
        .settings()
        .settings_root()
        .join("counter.settings.toml");
    std::fs::write(&prior_xdg_toml, "settings = [").unwrap();
    assert!(
        service
            .save_developer_settings("counter", all_settings(), Some("no"))
            .is_err()
    );
    assert!(!folder.join("settings.schema.toml").exists());

    let saved = service
        .save_developer_settings("counter", all_settings(), Some("yes"))
        .unwrap();

    assert_eq!(saved.len(), 10);
    assert_eq!(
        service.settings().schema_path("counter"),
        folder.join("settings.schema.toml")
    );
    assert!(service.settings().schema_path("counter").exists());
    assert!(!service.settings().values_path("counter").exists());
    assert_eq!(
        std::fs::read(folder.join("settings.json")).unwrap(),
        legacy_schema
    );
    assert_eq!(std::fs::read(&prior_xdg_toml).unwrap(), b"settings = [");
}

#[tokio::test]
async fn debounce_commits_only_latest_payload() {
    let (root, service, _, _) = fixture();
    std::fs::create_dir_all(root.path().join("static/counter")).unwrap();
    service
        .settings()
        .save_developer_settings("counter", all_settings())
        .unwrap();
    let first_service = service.clone();
    let first = tokio::spawn(async move {
        first_service
            .save_user_settings_debounced(
                "counter".into(),
                vec![SettingInput {
                    unique_id: "text".into(),
                    value: json!("first"),
                }],
            )
            .await
            .unwrap()
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    let second = service
        .save_user_settings_debounced(
            "counter".into(),
            vec![SettingInput {
                unique_id: "text".into(),
                value: json!("second"),
            }],
        )
        .await
        .unwrap();
    assert!(!first.await.unwrap());
    assert!(second);
    assert_eq!(
        service.get_counter_values("counter").unwrap()["text"],
        "second"
    );
}

#[tokio::test]
async fn parses_raw_download_input_once_and_honors_update_lifecycle() {
    let (root, service, _, downloader) = fixture();
    let request = CounterService::parse_download_request(
        "https%3A%2F%2Fexample.com%2Fa%252Fb.zip%3Fx%3Da%2526b",
        Some("counter"),
        Some("false"),
    )
    .unwrap();
    assert_eq!(request.url, "https://example.com/a%2Fb.zip?x=a%26b");
    assert_eq!(request.name, "counter");
    service.download(&request).await.unwrap();
    assert!(service.download(&request).await.is_err());
    let update = DownloadRequest {
        update: true,
        ..request
    };
    service.download(&update).await.unwrap();
    assert_eq!(lock(&downloader.0).len(), 2);
    assert!(root.path().join("static/counter/index.html").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(
            root.path().join("does-not-exist"),
            root.path().join("static/dangling"),
        )
        .unwrap();
        let dangling = DownloadRequest {
            url: "https://example.com/d.zip".into(),
            name: "dangling".into(),
            update: true,
        };
        assert!(matches!(
            service.download(&dangling).await,
            Err(CounterError::Symlink)
        ));
    }
}

#[test]
fn open_follows_directory_symlink_and_delete_unlinks_symlinks() {
    let (root, service, opener, _) = fixture();
    let counter = root.path().join("static/counter");
    std::fs::create_dir(&counter).unwrap();
    service.open_root().unwrap();
    assert!(service.open("../outside").is_err());
    assert!(service.delete("a/b").is_err());
    service.open("counter").unwrap();
    assert_eq!(
        lock(&opener.0).as_slice(),
        &[root.path().join("static"), counter.clone()]
    );
    service.delete("counter").unwrap();
    assert!(!counter.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, root.path().join("static/link")).unwrap();
        service.open("link").unwrap();
        assert_eq!(
            lock(&opener.0).last(),
            Some(&root.path().join("static/link"))
        );
        service.delete("link").unwrap();
        assert!(std::fs::symlink_metadata(root.path().join("static/link")).is_err());
        assert!(outside.exists());
        let outside_file = root.path().join("outside-file");
        std::fs::write(&outside_file, "").unwrap();
        symlink(&outside_file, root.path().join("static/file-link")).unwrap();
        assert!(matches!(
            service.open("file-link"),
            Err(CounterError::Symlink)
        ));
        service.delete("file-link").unwrap();
        assert!(std::fs::symlink_metadata(root.path().join("static/file-link")).is_err());
        assert!(outside_file.exists());
        symlink(
            root.path().join("gone"),
            root.path().join("static/dangling"),
        )
        .unwrap();
        assert!(matches!(
            service.open("dangling"),
            Err(CounterError::Symlink)
        ));
        service.delete("dangling").unwrap();
        assert!(std::fs::symlink_metadata(root.path().join("static/dangling")).is_err());
    }
}

#[tokio::test]
async fn reloadable_service_switches_assets_counters_settings_and_mutations_atomically() {
    use std::io::Read as _;

    let root = tempfile::tempdir().unwrap();
    let root_a = root.path().join("static-a");
    let root_b = root.path().join("static-b");
    let settings_root = root.path().join("settings");
    std::fs::create_dir_all(root_a.join("alpha")).unwrap();
    std::fs::create_dir_all(root_b.join("beta")).unwrap();
    std::fs::write(root_a.join("alpha/index.html"), "asset-a").unwrap();
    std::fs::write(root_b.join("beta/index.html"), "asset-b").unwrap();
    std::fs::write(
        root_b.join("beta/settings.json"),
        serde_json::to_vec(&all_settings()).unwrap(),
    )
    .unwrap();
    let opener = Arc::new(RecordingOpener::default());
    let downloader = Arc::new(RecordingDownloader::default());
    let service = ReloadableCounterService::from_service(CounterService::new(
        root_a.clone(),
        settings_root.clone(),
        opener.clone(),
        downloader.clone(),
    ));

    assert_eq!(service.discover().unwrap()[0].folder_name, "alpha");
    let crate::server::files::ResourceOutcome::File(mut asset_a) =
        crate::server::files::resolve_resource(
            Some(&service.current_root()),
            "alpha/index.html",
            "/alpha/index.html",
            crate::server::files::MissingKind::NotFound,
        )
        .unwrap()
    else {
        panic!("root A asset was not resolved");
    };
    let mut body = String::new();
    asset_a.file.read_to_string(&mut body).unwrap();
    assert_eq!(body, "asset-a");

    service.reload_static_root(root_b.clone());
    let counters = service.discover().unwrap();
    assert_eq!(counters.len(), 1);
    assert_eq!(counters[0].folder_name, "beta");
    assert!(matches!(
        crate::server::files::resolve_resource(
            Some(&service.current_root()),
            "alpha/index.html",
            "/alpha/index.html",
            crate::server::files::MissingKind::NotFound,
        ),
        Ok(crate::server::files::ResourceOutcome::Missing(
            crate::server::files::MissingKind::NotFound
        ))
    ));
    let crate::server::files::ResourceOutcome::File(mut asset_b) =
        crate::server::files::resolve_resource(
            Some(&service.current_root()),
            "beta/index.html",
            "/beta/index.html",
            crate::server::files::MissingKind::NotFound,
        )
        .unwrap()
    else {
        panic!("root B asset was not resolved");
    };
    body.clear();
    asset_b.file.read_to_string(&mut body).unwrap();
    assert_eq!(body, "asset-b");

    service
        .save_user_values(
            "beta",
            &[SettingInput {
                unique_id: "text".into(),
                value: json!("root-b"),
            }],
        )
        .unwrap();
    assert_eq!(
        service.get_counter_values("beta").unwrap()["text"],
        "root-b"
    );
    assert!(settings_root.join("beta.settings.values.toml").exists());
    service.open_root().unwrap();
    service.open("beta").unwrap();
    assert_eq!(
        lock(&opener.0).as_slice(),
        &[root_b.clone(), root_b.join("beta")]
    );

    service
        .download(&DownloadRequest {
            url: "https://example.com/new.zip".into(),
            name: "downloaded".into(),
            update: false,
        })
        .await
        .unwrap();
    assert!(root_b.join("downloaded/index.html").exists());
    assert!(!root_a.join("downloaded").exists());
    service.delete("downloaded").unwrap();

    let missing = root.path().join("not-created");
    service.reload_static_root(missing.clone());
    assert_eq!(service.current_root(), missing);
    assert!(service.discover().is_err());
    service.reload_static_root(root_b);
    assert_eq!(service.discover().unwrap()[0].folder_name, "beta");
    service.shutdown().await;
}

#[tokio::test]
async fn reloadable_service_keeps_one_debounce_generation_across_root_changes() {
    let root = tempfile::tempdir().unwrap();
    let root_a = root.path().join("a");
    let root_b = root.path().join("b");
    for static_root in [&root_a, &root_b] {
        std::fs::create_dir_all(static_root.join("counter")).unwrap();
        std::fs::write(
            static_root.join("counter/settings.json"),
            serde_json::to_vec(&all_settings()).unwrap(),
        )
        .unwrap();
    }
    let service = Arc::new(ReloadableCounterService::from_service(CounterService::new(
        root_a,
        root.path().join("settings"),
        Arc::new(RecordingOpener::default()),
        Arc::new(RecordingDownloader::default()),
    )));
    let first_service = Arc::clone(&service);
    let first = tokio::spawn(async move {
        first_service
            .save_user_settings_debounced(
                "counter".into(),
                vec![SettingInput {
                    unique_id: "text".into(),
                    value: json!("old"),
                }],
            )
            .await
            .unwrap()
    });
    while service.pending_save_generation("counter").await != Some(1) {
        tokio::task::yield_now().await;
    }
    service.reload_static_root(root_b);
    let second = service
        .save_user_settings_debounced(
            "counter".into(),
            vec![SettingInput {
                unique_id: "text".into(),
                value: json!("new"),
            }],
        )
        .await
        .unwrap();
    assert!(!first.await.unwrap());
    assert!(second);
    assert_eq!(
        service.get_counter_values("counter").unwrap()["text"],
        "new"
    );
}

#[test]
fn settings_reject_missing_and_non_directory_aliases_without_creating_files() {
    let (root, service, _, _) = fixture();
    assert!(matches!(
        service.get_settings("missing"),
        Err(CounterError::Settings(SettingsError::MissingCounter))
    ));
    assert!(!root.path().join("static/missing").exists());
    assert!(!service.settings().values_path("missing").exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let file_target = root.path().join("outside-file");
        std::fs::write(&file_target, "not a directory").unwrap();
        symlink(&file_target, root.path().join("static/file-alias")).unwrap();
        assert!(matches!(
            service.get_settings("file-alias"),
            Err(CounterError::Settings(SettingsError::UnsafeCounterPath))
        ));
        symlink(
            root.path().join("missing-target"),
            root.path().join("static/dangling-alias"),
        )
        .unwrap();
        assert!(matches!(
            service.get_settings("dangling-alias"),
            Err(CounterError::Settings(SettingsError::UnsafeCounterPath))
        ));
        assert!(!service.settings().values_path("file-alias").exists());
        assert!(!service.settings().values_path("dangling-alias").exists());
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("settings.json"), "[]").unwrap();
        let ordinary = root.path().join("static/ordinary");
        std::fs::create_dir(&ordinary).unwrap();
        symlink(
            outside.join("settings.json"),
            ordinary.join("settings.json"),
        )
        .unwrap();
        assert!(matches!(
            service.get_settings("ordinary"),
            Err(CounterError::Settings(SettingsError::UnsafeSettingsFile))
        ));
    }
}

#[cfg(unix)]
#[test]
fn settings_support_top_level_symlinked_overlay_directories() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let outside = root.path().join("outside-overlay");
    std::fs::create_dir(&outside).unwrap();
    // The alias itself is the mount declaration; no index marker is required.
    std::fs::write(
        outside.join("settings.json"),
        serde_json::to_vec(&all_settings()).unwrap(),
    )
    .unwrap();
    symlink(&outside, root.path().join("static/linked")).unwrap();

    let (settings, _) = service.get_settings("linked").unwrap();

    assert_eq!(settings.len(), all_settings().len());
}

#[cfg(unix)]
#[test]
fn settings_keep_alias_values_separate_and_prioritize_authoritative_toml() {
    use std::os::unix::fs::symlink;

    let (root, service, _, _) = fixture();
    let outside = root.path().join("outside-overlay");
    std::fs::create_dir(&outside).unwrap();
    // Settings authorization follows the same top-level alias contract as serving.
    std::fs::write(
        outside.join("settings.json"),
        serde_json::to_vec(&all_settings()).unwrap(),
    )
    .unwrap();
    symlink(&outside, root.path().join("static/linked")).unwrap();
    service
        .settings()
        .save_user_values(
            "linked",
            &[SettingInput {
                unique_id: "text".into(),
                value: json!("alias"),
            }],
        )
        .unwrap();
    assert_eq!(
        service.get_counter_values("linked").unwrap()["text"],
        "alias"
    );
    assert!(service.settings().values_path("linked").exists());

    let external_values = root.path().join("external-settings.values.json");
    std::fs::write(&external_values, "{}").unwrap();
    std::fs::remove_file(service.settings().values_path("linked")).unwrap();
    symlink(&external_values, service.settings().values_path("linked")).unwrap();
    assert!(matches!(
        service.get_settings("linked"),
        Err(CounterError::Settings(SettingsError::UnsafeSettingsFile))
    ));
    std::fs::remove_file(service.settings().values_path("linked")).unwrap();

    let external_definition = root.path().join("external-settings.json");
    std::fs::write(&external_definition, "[]").unwrap();
    std::fs::remove_file(outside.join("settings.json")).unwrap();
    symlink(&external_definition, outside.join("settings.json")).unwrap();
    let (settings, values) = service.get_settings("linked").unwrap();
    assert_eq!(settings, all_settings());
    assert_eq!(values["text"], "hello");
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_settings_file() {
    use std::os::unix::fs::symlink;
    let (root, service, _, _) = fixture();
    let counter = root.path().join("static/counter");
    std::fs::create_dir(&counter).unwrap();
    std::fs::write(counter.join("index.html"), "").unwrap();
    let outside = root.path().join("outside-settings.json");
    std::fs::write(&outside, "[]").unwrap();
    symlink(outside, counter.join("settings.json")).unwrap();
    assert!(matches!(service.discover(), Err(CounterError::Symlink)));
}

struct CancellingDownloader {
    started: Arc<tokio::sync::Semaphore>,
    finished: Arc<AtomicBool>,
}

impl CounterDownloader for CancellingDownloader {
    fn download_and_install(
        &self,
        _request: DownloadRequest,
        _destination: PathBuf,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), CounterError>> + Send + 'static>> {
        let started = self.started.clone();
        let finished = self.finished.clone();
        Box::pin(async move {
            started.add_permits(1);
            cancel.cancelled().await;
            finished.store(true, Ordering::Release);
            Ok(())
        })
    }
}

#[tokio::test]
async fn shutdown_cancels_and_joins_active_installs() {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    std::fs::create_dir(&static_root).unwrap();
    let downloader = Arc::new(CancellingDownloader {
        started: Arc::new(tokio::sync::Semaphore::new(0)),
        finished: Arc::new(AtomicBool::new(false)),
    });
    let service = CounterService::new(
        static_root,
        root.path().join("settings"),
        Arc::new(RecordingOpener::default()),
        downloader.clone(),
    );
    let request = DownloadRequest {
        url: "https://example.com/a.zip".into(),
        name: "counter".into(),
        update: false,
    };
    let downloading_service = service.clone();
    let download = tokio::spawn(async move { downloading_service.download(&request).await });
    let permit = downloader.started.acquire().await.unwrap();
    permit.forget();
    let shutdown_a = service.clone();
    let shutdown_b = service.clone();
    tokio::join!(shutdown_a.shutdown(), shutdown_b.shutdown());
    assert!(downloader.finished.load(Ordering::Acquire));
    assert!(download.await.unwrap().is_ok());
    assert!(matches!(
        service
            .download(&DownloadRequest {
                url: "https://example.com/b.zip".into(),
                name: "other".into(),
                update: false
            })
            .await,
        Err(CounterError::ShuttingDown)
    ));
}

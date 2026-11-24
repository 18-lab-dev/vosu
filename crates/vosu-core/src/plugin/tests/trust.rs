//! Trust and settings-schema loader boundaries.
use super::*;

fn expect_in_process_refused(result: Result<Vec<LoadedPlugin>, HostError>) {
    match result {
        Err(HostError::InProcessRefused { .. }) => {}
        Err(other) => panic!("expected InProcessRefused, got {other}"),
        Ok(_) => panic!("expected in-process to be refused"),
    }
}

// --- Trust refusal (§7.1) ------------------------------------------------

#[test]
fn development_plugin_is_refused_outside_debug_builds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        manifest(
            "acme.test",
            "\n[targets.x86_64-unknown-linux-gnu]\nlibrary='lib/libacme.so'\n",
        ),
    )
    .unwrap();

    match registry::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        dir.path(),
        false,
    ) {
        Err(HostError::InProcessRefused { .. }) => {}
        Err(other) => panic!("expected InProcessRefused, got {other}"),
        Ok(_) => panic!("expected in-process to be refused outside debug builds"),
    }
}

#[test]
fn development_plugin_with_an_invalid_settings_schema_refuses_with_settings_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        manifest(
            "acme.dev-badschema",
            "settings_schema='settings.schema.json'\n",
        ),
    )
    .unwrap();
    std::fs::write(dir.path().join("settings.schema.json"), "not valid json").unwrap();

    match registry::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        dir.path(),
        true,
    ) {
        Err(HostError::Settings(_)) => {}
        Err(other) => panic!("expected HostError::Settings, got {other}"),
        Ok(_) => panic!("expected an invalid settings_schema to refuse the load"),
    }
}

#[test]
fn development_plugin_with_a_missing_settings_schema_file_refuses_with_io_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        manifest(
            "acme.dev-missingschema",
            "settings_schema='settings.schema.json'\n",
        ),
    )
    .unwrap();
    // Deliberately not writing `settings.schema.json`.

    match registry::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        dir.path(),
        true,
    ) {
        Err(HostError::Io(_)) => {}
        Err(other) => panic!("expected HostError::Io, got {other}"),
        Ok(_) => panic!("expected a missing settings_schema file to refuse the load"),
    }
}

mod dummy_plugin {
    use super::{Init, Message, Plugin, PluginError};

    pub struct Dummy;
    impl Plugin for Dummy {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            _init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self)
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled Dummy);
}

#[test]
fn bundled_manifest_requesting_process_runtime_is_rejected() {
    let text = "kind='plugin'\nmanifest_version=1\nid='acme.test'\nname='Test'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='process'\n";
    expect_in_process_refused(registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_plugin::vosu_plugin_vtable,
        }],
    ));
}

#[test]
fn bundled_settings_schema_is_parsed_and_served() {
    let text = manifest("acme.schema", "settings_schema='settings.schema.json'\n");
    let schema_text = r#"[{"uniqueID":"RATE","type":"number","value":42.0}]"#;
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: Some(schema_text),
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_plugin::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let defaults = loaded[0]
        .settings_schema()
        .expect("this bundled entry declared settings_schema")
        .defaults();
    assert_eq!(defaults.get("RATE"), Some(&serde_json::Value::from(42.0)));
}

#[test]
fn invalid_bundled_settings_schema_refuses_load() {
    let text = manifest("acme.badschema", "settings_schema='settings.schema.json'\n");
    let result = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: Some("not valid json"),
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_plugin::vosu_plugin_vtable,
        }],
    );
    match result {
        Err(HostError::Settings(_)) => {}
        Err(other) => panic!("expected HostError::Settings, got {other}"),
        Ok(_) => panic!("expected an invalid settings_schema to refuse the load"),
    }
}

#[test]
fn bundled_settings_schema_presence_mismatch_is_refused() {
    // Manifest declares a schema, but the `BundledPlugin` entry does not
    // (the exact shape a missed `Some(SETTINGS_SCHEMA)` in a production
    // `BUNDLED` entry would have, P0 review).
    let declaring_manifest = manifest(
        "acme.schema-mismatch",
        "settings_schema='settings.schema.json'\n",
    );
    match registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &declaring_manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_plugin::vosu_plugin_vtable,
        }],
    ) {
        Err(HostError::SettingsSchemaMismatch {
            manifest_declares_one: true,
            entry_declares_one: false,
            ..
        }) => {}
        Err(other) => panic!("expected SettingsSchemaMismatch, got {other}"),
        Ok(_) => panic!("expected the manifest/entry schema mismatch to refuse the load"),
    }

    // The reverse: the entry declares one, but the manifest does not.
    let non_declaring_manifest = manifest("acme.schema-mismatch2", "");
    match registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &non_declaring_manifest,
            settings_schema: Some(r#"[{"uniqueID":"RATE","type":"number","value":1.0}]"#),
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_plugin::vosu_plugin_vtable,
        }],
    ) {
        Err(HostError::SettingsSchemaMismatch {
            manifest_declares_one: false,
            entry_declares_one: true,
            ..
        }) => {}
        Err(other) => panic!("expected SettingsSchemaMismatch, got {other}"),
        Ok(_) => panic!("expected the manifest/entry schema mismatch to refuse the load"),
    }
}

#[test]
fn installed_process_plugin_requires_m5_and_is_never_loaded_in_process() {
    let dir = tempfile::tempdir().unwrap();
    // Positive control for the refusal reason: the same package with an
    // in-process request reaches library loading (and fails on the absent
    // library), so only the runtime decides the typed refusal.
    std::fs::write(
        dir.path().join("manifest.toml"),
        manifest(
            "acme.installed",
            "\n[targets.x86_64-unknown-linux-gnu]\nlibrary='lib/libacme.so'\n[targets.x86_64-pc-windows-msvc]\nlibrary='lib/acme.dll'\n",
        ),
    )
    .unwrap();
    let load = |dir: &std::path::Path| {
        registry::load_installed(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &crate::plugin::ValidationSnapshot::default(),
            dir,
            "alice/plugins",
            false,
        )
    };
    match load(dir.path()) {
        Err(HostError::Io(_)) => {}
        Err(other) => panic!("expected the absent library to fail, got {other}"),
        Ok(_) => panic!("fixture has no library"),
    }
    let text = std::fs::read_to_string(dir.path().join("manifest.toml"))
        .unwrap()
        .replace("runtime='in-process'", "runtime='process'");
    std::fs::write(dir.path().join("manifest.toml"), text).unwrap();
    match load(dir.path()) {
        Err(HostError::ProcessRuntimeUnavailable) => {}
        Err(other) => panic!("expected ProcessRuntimeUnavailable, got {other}"),
        Ok(_) => panic!("a process plugin must never load in-process"),
    }
}

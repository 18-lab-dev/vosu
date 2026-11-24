//! P1 acceptance: `vosu_osu::MANIFEST` parses, its `[match]` table obeys
//! the DB (revised) `linux_name_contains` length rule, and
//! `vosu_osu::SETTINGS_SCHEMA`'s defaults agree with
//! `vosu_osu_state::ReaderSettings::default()` for the five D19 keys.
//!
//! `vosu-osu` has no dependency edge onto `vosu-core`
//! (`scripts/crate-boundaries.txt`), so this does not compile the
//! manifest's `[match]` table through `vosu_core::plugin::matcher`'s
//! `MatcherEngine` — it validates through `vosu_plugin_api::PluginManifest::parse`
//! instead (the plan's documented fallback) plus this test's own length
//! check, which reproduces the same 1-15 byte rule
//! `vosu_core::plugin::matcher::MatcherError::InvalidLinuxNameContains`
//! enforces at compile time there.

use vosu_plugin_api::PluginManifest;

#[test]
fn manifest_parses() {
    let manifest = PluginManifest::parse(vosu_osu::MANIFEST).expect("manifest.toml parses");
    assert_eq!(manifest.id, "vosu.osu");
    assert_eq!(manifest.abi_version, 1);
    assert!(manifest.concurrent_dispatch);
    assert_eq!(
        manifest.settings_schema.as_deref(),
        Some("settings.schema.json")
    );
}

#[test]
fn match_table_has_the_revised_db_fields() {
    let manifest = PluginManifest::parse(vosu_osu::MANIFEST).expect("manifest.toml parses");
    let table = manifest.matcher.expect("manifest declares [match]");
    assert_eq!(table.process_names, vec!["osu!.exe", "osulazer.exe"]);
    assert!(table.process_path_patterns.is_empty());
    assert!(table.command_line_patterns.is_empty());
    // comm_hints stays a pre-filter-only field (DB revised); this
    // manifest declares the equivalent coverage through
    // `linux_name_contains` instead, so it has nothing left to add here.
    assert!(table.comm_hints.is_empty());
    assert_eq!(table.linux_name_contains, vec!["osu!", "osulazer.exe"]);
}

#[test]
fn every_linux_name_contains_entry_is_at_most_15_bytes() {
    let manifest = PluginManifest::parse(vosu_osu::MANIFEST).expect("manifest.toml parses");
    let table = manifest.matcher.expect("manifest declares [match]");
    for entry in &table.linux_name_contains {
        assert!(
            !entry.is_empty() && entry.len() <= 15,
            "linux_name_contains entry {entry:?} must be 1-15 bytes"
        );
    }
}

#[test]
fn settings_schema_defaults_match_reader_settings_default() {
    let schema = vosu_core_settings_schema_parse(vosu_osu::SETTINGS_SCHEMA);
    let reader_defaults = vosu_osu_state::ReaderSettings::default();

    assert_eq!(
        bool_default(&schema, "CALCULATE_PP"),
        reader_defaults.calculate_pp
    );
    assert_eq!(
        number_default(&schema, "POLL_RATE"),
        reader_defaults.poll_rate
    );
    assert_eq!(
        number_default(&schema, "PRECISE_DATA_POLL_RATE"),
        reader_defaults.precise_data_poll_rate
    );
    assert_eq!(
        bool_default(&schema, "SHOW_MP_COMMANDS"),
        reader_defaults.show_mp_commands
    );
    assert_eq!(
        bool_default(&schema, "READ_MANIA_SCROLL_SPEED"),
        reader_defaults.read_mania_scroll_speed
    );
}

/// `vosu-osu` has no `vosu-core` dependency, so this parses
/// `settings.schema.json` as the plain overlay-settings array
/// `docs/tools/protocol.md` defines (the same shape
/// `vosu_core::plugin::settings::SettingsSchema::parse` validates) rather
/// than reusing that core-only type.
fn vosu_core_settings_schema_parse(text: &str) -> Vec<(String, serde_json::Value)> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(text).expect("settings.schema.json is a JSON array");
    entries
        .into_iter()
        .map(|entry| {
            let id = entry["uniqueID"]
                .as_str()
                .expect("every entry has a uniqueID")
                .to_string();
            (id, entry["value"].clone())
        })
        .collect()
}

fn bool_default(schema: &[(String, serde_json::Value)], key: &str) -> bool {
    schema
        .iter()
        .find(|(id, _)| id == key)
        .unwrap_or_else(|| panic!("schema has no entry {key:?}"))
        .1
        .as_bool()
        .unwrap_or_else(|| panic!("{key:?}'s default is not a bool"))
}

fn number_default(schema: &[(String, serde_json::Value)], key: &str) -> f64 {
    schema
        .iter()
        .find(|(id, _)| id == key)
        .unwrap_or_else(|| panic!("schema has no entry {key:?}"))
        .1
        .as_f64()
        .unwrap_or_else(|| panic!("{key:?}'s default is not a number"))
}

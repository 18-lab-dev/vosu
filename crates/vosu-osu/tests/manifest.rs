//! P1 acceptance: `vosu_osu::MANIFEST` parses, its `[match]` table obeys
//! the DB (revised) `linux_name_contains` length rule, and
//! `vosu_osu::SETTINGS_SCHEMA`'s shape/defaults agree with
//! `vosu_osu::settings::OsuSettings` and `vosu_osu_state::ReaderSettings::default()`
//! for the five D19 keys.
//!
//! `vosu-osu` has no dependency edge onto `vosu-core`
//! (`scripts/crate-boundaries.txt`), so this does not compile the
//! manifest's `[match]` table through `vosu_core::plugin::matcher`'s
//! `MatcherEngine` — it validates through `vosu_plugin_api::manifest::PackageManifest::parse`
//! instead, using the same portable document validator as the core matcher.
//! The independent byte-bound assertions below retain the fixture contract.
//! Likewise `parse_overlay_entries` below
//! is this test's own narrow reader of the overlay settings array shape
//! (`docs/tools/protocol.md`) — it is not a stand-in for core's
//! `vosu_core::plugin::settings::SettingsSchema::parse`, which this crate
//! cannot depend on and which real bundled-plugin loading still runs
//! (`invalid_bundled_settings_schema_refuses_load`, unit P0).

use vosu_osu::settings::OsuSettings;

#[test]
fn manifest_parses() {
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(vosu_osu::MANIFEST)
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .expect("manifest.toml parses");
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
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(vosu_osu::MANIFEST)
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .expect("manifest.toml parses");
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
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(vosu_osu::MANIFEST)
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .expect("manifest.toml parses");
    let table = manifest.matcher.expect("manifest declares [match]");
    for entry in &table.linux_name_contains {
        assert!(
            !entry.is_empty() && entry.len() <= 15,
            "linux_name_contains entry {entry:?} must be 1-15 bytes"
        );
    }
}

/// One `settings.schema.json` entry, this test's own narrow view (module
/// doc above): `uniqueID`, `type`, and the entry's own default `value`.
struct OverlayEntry {
    id: String,
    kind: String,
    value: serde_json::Value,
}

/// Parses `text` as the plain overlay-settings array `docs/tools/protocol.md`
/// defines. Not `vosu_core::plugin::settings::SettingsSchema::parse`
/// (module doc above): this only reads the three fields this test needs
/// and does not validate every entry shape core's parser does.
fn parse_overlay_entries(text: &str) -> Vec<OverlayEntry> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(text).expect("settings.schema.json is a JSON array");
    entries
        .into_iter()
        .map(|entry| OverlayEntry {
            id: entry["uniqueID"]
                .as_str()
                .expect("every entry has a uniqueID")
                .to_string(),
            kind: entry["type"]
                .as_str()
                .expect("every entry has a type")
                .to_string(),
            value: entry["value"].clone(),
        })
        .collect()
}

fn find<'a>(entries: &'a [OverlayEntry], key: &str) -> &'a OverlayEntry {
    entries
        .iter()
        .find(|entry| entry.id == key)
        .unwrap_or_else(|| panic!("schema has no entry {key:?}"))
}

#[test]
fn schema_ids_are_unique_and_types_match_from_cbor() {
    let entries = parse_overlay_entries(vosu_osu::SETTINGS_SCHEMA);

    let mut seen = std::collections::HashSet::new();
    for entry in &entries {
        assert!(
            seen.insert(entry.id.clone()),
            "duplicate uniqueID {:?}",
            entry.id
        );
    }

    // Every key `OsuSettings::from_cbor` decodes as a bool must declare
    // `checkbox`, and every key it decodes as a number must declare
    // `number` (`crates/vosu-osu/src/settings.rs`'s `as_bool`/`as_number`
    // dispatch by these same five uppercase ids).
    let checkbox_keys = [
        "CALCULATE_PP",
        "SHOW_MP_COMMANDS",
        "READ_MANIA_SCROLL_SPEED",
    ];
    let number_keys = ["POLL_RATE", "PRECISE_DATA_POLL_RATE"];
    for key in checkbox_keys {
        assert_eq!(
            find(&entries, key).kind,
            "checkbox",
            "{key} should be a checkbox"
        );
    }
    for key in number_keys {
        assert_eq!(
            find(&entries, key).kind,
            "number",
            "{key} should be a number"
        );
    }
    assert_eq!(entries.len(), checkbox_keys.len() + number_keys.len());
}

#[test]
fn schema_defaults_match_reader_settings_default() {
    let entries = parse_overlay_entries(vosu_osu::SETTINGS_SCHEMA);
    let reader_defaults = vosu_osu_state::ReaderSettings::default();

    let as_bool = |key: &str| {
        find(&entries, key)
            .value
            .as_bool()
            .unwrap_or_else(|| panic!("{key:?}'s default is not a bool"))
    };
    let as_number = |key: &str| {
        find(&entries, key)
            .value
            .as_f64()
            .unwrap_or_else(|| panic!("{key:?}'s default is not a number"))
    };

    assert_eq!(as_bool("CALCULATE_PP"), reader_defaults.calculate_pp);
    assert_eq!(as_number("POLL_RATE"), reader_defaults.poll_rate);
    assert_eq!(
        as_number("PRECISE_DATA_POLL_RATE"),
        reader_defaults.precise_data_poll_rate
    );
    assert_eq!(
        as_bool("SHOW_MP_COMMANDS"),
        reader_defaults.show_mp_commands
    );
    assert_eq!(
        as_bool("READ_MANIA_SCROLL_SPEED"),
        reader_defaults.read_mania_scroll_speed
    );
}

#[test]
fn osu_settings_default_matches_reader_settings_default() {
    let defaults = OsuSettings::default();
    let reader_defaults = vosu_osu_state::ReaderSettings::default();

    assert_eq!(defaults.calculate_pp, reader_defaults.calculate_pp);
    assert_eq!(defaults.poll_rate, reader_defaults.poll_rate);
    assert_eq!(
        defaults.precise_data_poll_rate,
        reader_defaults.precise_data_poll_rate
    );
    assert_eq!(defaults.show_mp_commands, reader_defaults.show_mp_commands);
    assert_eq!(
        defaults.read_mania_scroll_speed,
        reader_defaults.read_mania_scroll_speed
    );
}

#[test]
fn manifest_covers_match_coverage_table() {
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(vosu_osu::MANIFEST)
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .unwrap();
    let expected = [
        "field:vosu.status.status",
        "field:vosu.chart.title",
        "field:vosu.chart.artist",
        "field:vosu.chart.difficultyName",
        "field:vosu.chart.charter",
        "field:vosu.chart.length",
        "field:vosu.chart.bpm",
        "field:vosu.chart.keyCount",
        "field:vosu.play",
        "field:vosu.skin.name",
    ];
    assert_eq!(manifest.covers.len(), expected.len());
    for field in expected {
        assert!(
            manifest.covers.iter().any(|cover| cover == field),
            "{field}"
        );
    }
    assert_eq!(manifest.resource_kinds.len(), 5);
    assert_eq!(manifest.root_slots, ["songs", "skins", "files"]);
}

#[test]
fn provides_version_equals_schema_version() {
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(vosu_osu::MANIFEST)
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .unwrap();
    let schema: serde_json::Value = serde_json::from_str(vosu_osu::OSU_NAMESPACE_SCHEMA).unwrap();
    assert_eq!(manifest.provides.len(), 2);
    assert_eq!(manifest.provides[1].id, "action:vosu.osu.beatmap.import");
    assert_eq!(manifest.provides[1].version, "1.0.0");
    assert_eq!(manifest.actions.len(), 1);
    assert_eq!(manifest.actions[0].name, "beatmap.import");
    assert!(manifest.actions[0].cli);
    assert_eq!(
        manifest.actions[0].session,
        vosu_plugin_api::ActionSessionMode::Required
    );
    let provider = &manifest.provides[0];
    assert_eq!(provider.id, "field:vosu.osu");
    assert_eq!(provider.version, schema["version"].as_str().unwrap());
    assert_eq!(
        provider.schema.as_deref(),
        Some("schemas/vosu.osu.schema.json")
    );
}

//! Proves `vosu_osu::SETTINGS_SCHEMA` (the real `settings.schema.json` P1
//! embeds, replacing bridge B2's own hard-coded one, P0) actually gets
//! past the SDK's `SettingsSchema::parse`, the one parser the host's
//! `load_bundled` (`C3`'s production `BUNDLED` entry), catalog install and
//! `vosu-package-check` all use. A rejection here would otherwise first
//! show up at runtime once `load_bundled` refuses a bad schema.
//!
//! Also pins the schema's defaults to `vosu_osu_state::ReaderSettings::
//! default()` (the same five D19 keys bridge B2 used to serve,
//! `crates/vosu-core/src/plugin/settings.rs`'s old
//! `ConfigSettingsBridgeB2::schema`), so a schema/default drift between
//! the two crates is caught here rather than only once `vosu-osu` is
//! wired into production settings storage.

use vosu_osu_state::ReaderSettings;
use vosu_plugin_api::manifest::settings::SettingsSchema;

#[test]
fn vosu_osu_settings_schema_parses_and_matches_reader_settings_defaults() {
    let schema = SettingsSchema::parse(vosu_osu::SETTINGS_SCHEMA)
        .expect("vosu-osu's embedded settings.schema.json must parse through core's validator");
    let defaults = schema.defaults();
    let reader_defaults = ReaderSettings::default();

    assert_eq!(
        defaults.get("CALCULATE_PP"),
        Some(&serde_json::Value::Bool(reader_defaults.calculate_pp))
    );
    assert_eq!(
        defaults.get("POLL_RATE"),
        Some(&serde_json::Value::from(reader_defaults.poll_rate))
    );
    assert_eq!(
        defaults.get("PRECISE_DATA_POLL_RATE"),
        Some(&serde_json::Value::from(
            reader_defaults.precise_data_poll_rate
        ))
    );
    assert_eq!(
        defaults.get("SHOW_MP_COMMANDS"),
        Some(&serde_json::Value::Bool(reader_defaults.show_mp_commands))
    );
    assert_eq!(
        defaults.get("READ_MANIA_SCROLL_SPEED"),
        Some(&serde_json::Value::Bool(
            reader_defaults.read_mania_scroll_speed
        ))
    );
    assert_eq!(
        defaults.len(),
        5,
        "the schema should declare exactly the five D19 keys, got: {defaults:?}"
    );
}

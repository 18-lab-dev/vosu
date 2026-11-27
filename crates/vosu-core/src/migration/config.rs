//! Legacy configuration and plugin-settings snapshot/conversion (A8.1 / A8.3).
//!
//! Precedence: `vosu.toml` > `vosu.env` > pre-rename legacy `.env`.
//! Raw TOML and `.env` inputs are parsed and validated BEFORE any serializer
//! can discard keys or touch disk:
//! - Extracts explicit present values (distinguishing absent vs explicit
//!   `false`/`0`/`""`) for `vosu.osu`'s 5 D19 keys and `vosu.overlayhost`'s
//!   `STATIC_FOLDER_PATH` -> `static_folder_path`.
//! - Validates extracted overrides against each plugin's real `SettingsSchema`.
//! - Strips only the top-level 6 moved keys + 4 N0 retired keys from canonical
//!   `vosu.toml` while preserving `API_V1_SUBSCRIPTION_DEBOUNCE`, all remaining
//!   core keys, unrelated unknown top-level keys/tables, and nested keys with
//!   coincidentally matching leaf names (e.g. `[custom] POLL_RATE = 999`).
//! - Fails closed on malformed TOML/env or invalid moved-setting values before
//!   any destructive write, with redacted diagnostics.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Number, Value};

use crate::config::{ConfigDocument, PRE_RENAME_ENV_FILENAME, parse_real_number};

use super::journal::{
    CoreConfigSnapshot, LegacyEnvKind, SourceFingerprint, atomic_publish_bytes, read_plain_bounded,
    read_published_bounded, reject_target_symlink, sync_parent_dir,
};
use super::{MigrationError, MigrationSchemas};

/// Maximum legacy config file size (1 MiB).
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Target plugin ID for D19's five osu!-specific keys.
pub const OSU_PLUGIN_ID: &str = "vosu.osu";
/// Target plugin ID for A7.1-OR's `static_folder_path` setting.
pub const OVERLAY_HOST_PLUGIN_ID: &str = "vosu.overlayhost";

/// The 5 D19 keys moved from top-level `vosu.toml` into `vosu.osu` settings.
pub const OSU_MOVED_KEYS: [&str; 5] = [
    "CALCULATE_PP",
    "POLL_RATE",
    "PRECISE_DATA_POLL_RATE",
    "SHOW_MP_COMMANDS",
    "READ_MANIA_SCROLL_SPEED",
];

/// The A7.1-OR key moved from top-level `vosu.toml` into `vosu.overlayhost`.
pub const OVERLAY_HOST_LEGACY_KEY: &str = "STATIC_FOLDER_PATH";
/// Destination key in `vosu.overlayhost`'s `settings.schema.json`.
pub const OVERLAY_HOST_SETTING_KEY: &str = "static_folder_path";

/// The exact 4 N0 retired keys stripped from top-level `vosu.toml`.
pub const RETIRED_KEYS: [&str; 4] = [
    "ENABLE_KEY_OVERLAY",
    "ENABLE_MANIA_KEY_HIT_ERRORS",
    "ENABLE_HIT_OFFSETS",
    "V2_SUBSCRIPTION_DEBOUNCE",
];

/// The 12 core keys retained in 2.0 `vosu.toml` in canonical serialization order.
const CORE_BINDING_ORDER: [&str; 12] = [
    "DEBUG_LOG",
    "ENABLE_AUTOUPDATE",
    "ENABLE_DASHBOARD",
    "OPEN_DASHBOARD_ON_STARTUP",
    "START_AT_LOGIN",
    "API_V1_SUBSCRIPTION_DEBOUNCE",
    "SERVER_IP",
    "SERVER_PORT",
    "DASHBOARD_IP",
    "DASHBOARD_PORT",
    "ALLOWED_IPS",
    "MAX_PACKAGE_MIB",
];

const CORE_NEWLINE_INSERTIONS: [&str; 3] = [
    "START_AT_LOGIN",
    "API_V1_SUBSCRIPTION_DEBOUNCE",
    "ALLOWED_IPS",
];

/// All 10 top-level bindings stripped from `vosu.toml` during 1.x -> 2.0 migration.
fn is_stripped_top_level_key(key: &str) -> bool {
    OSU_MOVED_KEYS.contains(&key) || key == OVERLAY_HOST_LEGACY_KEY || RETIRED_KEYS.contains(&key)
}

fn is_core_key(key: &str) -> bool {
    CORE_BINDING_ORDER.contains(&key)
}

#[derive(Debug, Clone)]
struct CoreValues {
    debug_log: bool,
    enable_auto_update: bool,
    enable_dashboard: bool,
    open_dashboard_on_startup: bool,
    start_at_login: bool,
    api_v1_subscription_debounce: f64,
    server_ip: String,
    server_port: f64,
    dashboard_ip: String,
    dashboard_port: f64,
    allowed_ips: String,
    max_package_mib: f64,
}

impl Default for CoreValues {
    fn default() -> Self {
        Self {
            debug_log: false,
            enable_auto_update: true,
            enable_dashboard: true,
            open_dashboard_on_startup: true,
            start_at_login: false,
            api_v1_subscription_debounce: 1.0,
            server_ip: "127.0.0.1".to_owned(),
            server_port: 24_050.0,
            dashboard_ip: "127.0.0.1".to_owned(),
            dashboard_port: 24_051.0,
            allowed_ips: "127.0.0.1,localhost,absolute".to_owned(),
            max_package_mib: 512.0,
        }
    }
}

impl CoreValues {
    fn apply_env(&mut self, env: &HashMap<String, String>) {
        if let Some(raw) = env.get("DEBUG_LOG") {
            self.debug_log = raw == "true";
        }
        if let Some(raw) = env.get("ENABLE_AUTOUPDATE") {
            self.enable_auto_update = raw == "true";
        }
        if let Some(raw) = env.get("ENABLE_DASHBOARD") {
            self.enable_dashboard = raw == "true";
        }
        if let Some(raw) = env.get("OPEN_DASHBOARD_ON_STARTUP") {
            self.open_dashboard_on_startup = raw == "true";
        }
        if let Some(raw) = env.get("START_AT_LOGIN") {
            self.start_at_login = raw == "true";
        }
        if let Some(raw) = env.get("API_V1_SUBSCRIPTION_DEBOUNCE") {
            self.api_v1_subscription_debounce = parse_real_number(raw).unwrap_or(1.0);
        }
        if let Some(raw) = env.get("SERVER_IP") {
            self.server_ip = raw.clone();
        }
        if let Some(raw) = env.get("SERVER_PORT") {
            self.server_port = parse_real_number(raw).unwrap_or(24_050.0);
        }
        if let Some(raw) = env.get("DASHBOARD_IP") {
            self.dashboard_ip = raw.clone();
        }
        if let Some(raw) = env.get("DASHBOARD_PORT") {
            self.dashboard_port = parse_real_number(raw).unwrap_or(24_051.0);
        }
        if let Some(raw) = env.get("ALLOWED_IPS") {
            self.allowed_ips = raw.clone();
        }
        if let Some(raw) = env.get("MAX_PACKAGE_MIB") {
            self.max_package_mib = parse_real_number(raw).unwrap_or(512.0);
        }
    }

    fn apply_toml(&mut self, table: &toml::Table) {
        if let Some(value) = table.get("DEBUG_LOG") {
            self.debug_log = value.as_bool().unwrap_or(false);
        }
        if let Some(value) = table.get("ENABLE_AUTOUPDATE") {
            self.enable_auto_update = value.as_bool().unwrap_or(true);
        }
        if let Some(value) = table.get("ENABLE_DASHBOARD") {
            self.enable_dashboard = value.as_bool().unwrap_or(true);
        }
        if let Some(value) = table.get("OPEN_DASHBOARD_ON_STARTUP") {
            self.open_dashboard_on_startup = value.as_bool().unwrap_or(true);
        }
        if let Some(value) = table.get("START_AT_LOGIN") {
            self.start_at_login = value.as_bool().unwrap_or(false);
        }
        if let Some(value) = table.get("API_V1_SUBSCRIPTION_DEBOUNCE") {
            self.api_v1_subscription_debounce = toml_number(value).unwrap_or(1.0);
        }
        if let Some(value) = table.get("SERVER_IP") {
            self.server_ip = value.as_str().unwrap_or("127.0.0.1").to_owned();
        }
        if let Some(value) = table.get("SERVER_PORT") {
            self.server_port = toml_number(value).unwrap_or(24_050.0);
        }
        if let Some(value) = table.get("DASHBOARD_IP") {
            self.dashboard_ip = value.as_str().unwrap_or("127.0.0.1").to_owned();
        }
        if let Some(value) = table.get("DASHBOARD_PORT") {
            self.dashboard_port = toml_number(value).unwrap_or(24_051.0);
        }
        if let Some(value) = table.get("ALLOWED_IPS") {
            self.allowed_ips = value
                .as_str()
                .unwrap_or("127.0.0.1,localhost,absolute")
                .to_owned();
        }
        if let Some(value) = table.get("MAX_PACKAGE_MIB") {
            self.max_package_mib = toml_number(value).unwrap_or(512.0);
        }
    }

    fn clamp(&mut self) {
        if !self.max_package_mib.is_finite()
            || self.max_package_mib.fract() != 0.0
            || !(1.0..=4096.0).contains(&self.max_package_mib)
        {
            self.max_package_mib = 512.0;
        }
        if self.api_v1_subscription_debounce < 1.0 {
            self.api_v1_subscription_debounce = 1.0;
        }
        if !self.api_v1_subscription_debounce.is_finite()
            || self.api_v1_subscription_debounce > ConfigDocument::MAX_INTERVAL_MS
        {
            self.api_v1_subscription_debounce = ConfigDocument::MAX_INTERVAL_MS;
        }
    }

    fn toml_value(&self, binding: &str) -> toml::Value {
        match binding {
            "DEBUG_LOG" => toml::Value::Boolean(self.debug_log),
            "ENABLE_AUTOUPDATE" => toml::Value::Boolean(self.enable_auto_update),
            "ENABLE_DASHBOARD" => toml::Value::Boolean(self.enable_dashboard),
            "OPEN_DASHBOARD_ON_STARTUP" => toml::Value::Boolean(self.open_dashboard_on_startup),
            "START_AT_LOGIN" => toml::Value::Boolean(self.start_at_login),
            "API_V1_SUBSCRIPTION_DEBOUNCE" => toml::Value::Float(self.api_v1_subscription_debounce),
            "SERVER_IP" => toml::Value::String(self.server_ip.clone()),
            "SERVER_PORT" => toml::Value::Float(self.server_port),
            "DASHBOARD_IP" => toml::Value::String(self.dashboard_ip.clone()),
            "DASHBOARD_PORT" => toml::Value::Float(self.dashboard_port),
            "ALLOWED_IPS" => toml::Value::String(self.allowed_ips.clone()),
            "MAX_PACKAGE_MIB" => toml::Value::Integer(self.max_package_mib as i64),
            _ => unreachable!("CORE_BINDING_ORDER contains only core bindings"),
        }
    }

    fn serialize(&self, unknown: &toml::Table) -> String {
        let mut output = String::with_capacity(512);
        for (index, binding) in CORE_BINDING_ORDER.iter().enumerate() {
            if index != 0 {
                output.push('\n');
            }
            output.push_str(binding);
            output.push_str(" = ");
            let _ = write!(&mut output, "{}", self.toml_value(binding));
            if CORE_NEWLINE_INSERTIONS.contains(binding) {
                output.push('\n');
            }
        }
        output.push('\n');
        if !unknown.is_empty() {
            output.push('\n');
            output.push_str(&toml::to_string(unknown).expect("parsed TOML values serialize"));
        }
        output
    }
}

fn toml_number(value: &toml::Value) -> Option<f64> {
    match value {
        toml::Value::Integer(integer) => Some(*integer as f64),
        toml::Value::Float(float) if float.is_finite() => Some(*float),
        _ => None,
    }
}

/// Output of parsing and validating legacy configuration sources before any write.
pub(crate) struct ConfigSnapshotOutput {
    pub(crate) fingerprints: BTreeMap<String, SourceFingerprint>,
    pub(crate) osu_overrides: Map<String, Value>,
    pub(crate) overlay_host_overrides: Map<String, Value>,
    pub(crate) core_config: CoreConfigSnapshot,
}

/// Reads, parses, and validates `vosu.toml`, `vosu.env`, and the pre-rename
/// legacy `.env` file without mutating anything on disk.
pub(crate) fn snapshot_legacy_config(
    config_root: &Path,
    schemas: &MigrationSchemas,
) -> Result<ConfigSnapshotOutput, MigrationError> {
    let toml_path = config_root.join("vosu.toml");
    let vosu_env_path = config_root.join("vosu.env");
    let pre_rename_env_path = config_root.join(PRE_RENAME_ENV_FILENAME);

    reject_target_symlink(&toml_path, "vosu.toml")?;

    let toml_bytes = read_published_bounded(&toml_path, MAX_CONFIG_BYTES, "vosu.toml")?;
    let vosu_env_bytes = read_plain_bounded(&vosu_env_path, MAX_CONFIG_BYTES, "vosu.env")?;
    let pre_rename_env_bytes =
        read_plain_bounded(&pre_rename_env_path, MAX_CONFIG_BYTES, "legacy.env")?;

    let mut fingerprints = BTreeMap::new();
    fingerprints.insert(
        "vosu.toml".to_owned(),
        toml_bytes
            .as_deref()
            .map_or_else(SourceFingerprint::absent, SourceFingerprint::from_bytes),
    );
    fingerprints.insert(
        "vosu.env".to_owned(),
        vosu_env_bytes
            .as_deref()
            .map_or_else(SourceFingerprint::absent, SourceFingerprint::from_bytes),
    );
    fingerprints.insert(
        "legacy.env".to_owned(),
        pre_rename_env_bytes
            .as_deref()
            .map_or_else(SourceFingerprint::absent, SourceFingerprint::from_bytes),
    );

    let toml_table = match toml_bytes {
        Some(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| MigrationError::MalformedInput { label: "vosu.toml" })?;
            let table: toml::Table = toml::from_str(&text)
                .map_err(|_| MigrationError::MalformedInput { label: "vosu.toml" })?;
            Some(table)
        }
        None => None,
    };

    let vosu_env = match vosu_env_bytes {
        Some(bytes) => {
            let text = String::from_utf8(bytes)
                .map_err(|_| MigrationError::MalformedInput { label: "vosu.env" })?;
            Some(parse_env_strict(&text, "vosu.env")?)
        }
        None => None,
    };

    let pre_rename_env = match pre_rename_env_bytes {
        Some(bytes) => {
            let text = String::from_utf8(bytes).map_err(|_| MigrationError::MalformedInput {
                label: "legacy.env",
            })?;
            Some(parse_env_strict(&text, "legacy.env")?)
        }
        None => None,
    };

    // Extract moved overrides with strict precedence: vosu.toml > vosu.env > pre-rename env.
    // Every present value is validated so malformed input fails closed before any write.
    let mut osu_overrides = Map::new();
    let mut overlay_host_overrides = Map::new();
    if let Some(env) = pre_rename_env.as_ref() {
        extract_env_moved_keys(env, &mut osu_overrides, &mut overlay_host_overrides)?;
    }
    if let Some(env) = vosu_env.as_ref() {
        extract_env_moved_keys(env, &mut osu_overrides, &mut overlay_host_overrides)?;
    }
    if let Some(table) = toml_table.as_ref() {
        extract_toml_moved_keys(table, &mut osu_overrides, &mut overlay_host_overrides)?;
    }

    // Validate extracted overrides against each plugin's real `settings.schema.json`.
    schemas
        .osu
        .merge(&osu_overrides)
        .map_err(|_| MigrationError::PluginSchemaRejected {
            plugin_id: OSU_PLUGIN_ID,
        })?;
    schemas
        .overlay_host
        .merge(&overlay_host_overrides)
        .map_err(|_| MigrationError::PluginSchemaRejected {
            plugin_id: OVERLAY_HOST_PLUGIN_ID,
        })?;

    // Collect stripped top-level keys (sorted, deduplicated).
    let mut stripped_keys = Vec::new();
    for env in [pre_rename_env.as_ref(), vosu_env.as_ref()]
        .into_iter()
        .flatten()
    {
        for key in env.keys() {
            if is_stripped_top_level_key(key) && !stripped_keys.contains(key) {
                stripped_keys.push(key.clone());
            }
        }
    }
    if let Some(table) = toml_table.as_ref() {
        for key in table.keys() {
            if is_stripped_top_level_key(key) && !stripped_keys.contains(key) {
                stripped_keys.push(key.clone());
            }
        }
    }
    stripped_keys.sort();

    let mut legacy_env_files = Vec::new();
    if vosu_env.is_some() {
        legacy_env_files.push(LegacyEnvKind::VosuEnv);
    }
    if pre_rename_env.is_some() {
        legacy_env_files.push(LegacyEnvKind::PreRenameEnv);
    }

    // Determine whether canonical `vosu.toml` must be written.
    // Fresh installs (no toml and no env files) and already-clean `vosu.toml`
    // files (no env files and no stripped top-level keys) stay untouched.
    let needs_toml_write = !legacy_env_files.is_empty() || !stripped_keys.is_empty();
    let canonical_toml = if needs_toml_write {
        let mut core = CoreValues::default();
        let mut unknown = toml::Table::new();

        for env in [pre_rename_env.as_ref(), vosu_env.as_ref()]
            .into_iter()
            .flatten()
        {
            core.apply_env(env);
            for (key, value) in env {
                if !is_core_key(key) && !is_stripped_top_level_key(key) {
                    unknown.insert(key.clone(), toml::Value::String(value.clone()));
                }
            }
        }
        if let Some(table) = toml_table.as_ref() {
            core.apply_toml(table);
            for (key, value) in table {
                if !is_core_key(key) && !is_stripped_top_level_key(key) {
                    unknown.insert(key.clone(), value.clone());
                }
            }
        }
        core.clamp();
        Some(core.serialize(&unknown))
    } else {
        None
    };

    Ok(ConfigSnapshotOutput {
        fingerprints,
        osu_overrides,
        overlay_host_overrides,
        core_config: CoreConfigSnapshot {
            canonical_toml,
            stripped_keys,
            legacy_env_files,
        },
    })
}

/// Durably publishes canonical `vosu.toml` from the snapshotted string.
pub(crate) fn publish_canonical_toml(
    config_root: &Path,
    canonical_toml: &str,
) -> Result<(), MigrationError> {
    let path = config_root.join("vosu.toml");
    atomic_publish_bytes(&path, canonical_toml.as_bytes(), "vosu.toml")
}

/// Removes readable legacy `.env` files listed in the snapshot after canonical
/// `vosu.toml` is durable. Tolerates `NotFound` for idempotent crash replay.
pub(crate) fn remove_legacy_env_files(
    config_root: &Path,
    files: &[LegacyEnvKind],
) -> Result<Vec<LegacyEnvKind>, MigrationError> {
    let mut removed_any = false;
    let mut removed = Vec::new();
    for &kind in files {
        let path = match kind {
            LegacyEnvKind::VosuEnv => config_root.join("vosu.env"),
            LegacyEnvKind::PreRenameEnv => config_root.join(PRE_RENAME_ENV_FILENAME),
        };
        let label = match kind {
            LegacyEnvKind::VosuEnv => "vosu.env",
            LegacyEnvKind::PreRenameEnv => "legacy.env",
        };
        match fs::remove_file(&path) {
            Ok(()) => {
                removed_any = true;
                removed.push(kind);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                removed.push(kind);
            }
            Err(error) => {
                return Err(MigrationError::Io {
                    label,
                    kind: error.kind(),
                });
            }
        }
    }
    if removed_any {
        sync_parent_dir(config_root, "vosu.toml")?;
    }
    Ok(removed)
}

fn clamp_interval(value: f64, min: f64) -> f64 {
    value.clamp(min, ConfigDocument::MAX_INTERVAL_MS)
}

fn json_number(value: f64, key: &'static str) -> Result<Value, MigrationError> {
    Number::from_f64(value)
        .map(Value::Number)
        .ok_or(MigrationError::InvalidSetting { key })
}

fn extract_toml_moved_keys(
    table: &toml::Table,
    osu_overrides: &mut Map<String, Value>,
    overlay_host_overrides: &mut Map<String, Value>,
) -> Result<(), MigrationError> {
    for key in [
        "CALCULATE_PP",
        "SHOW_MP_COMMANDS",
        "READ_MANIA_SCROLL_SPEED",
    ] {
        if let Some(value) = table.get(key) {
            let parsed = value
                .as_bool()
                .ok_or(MigrationError::InvalidSetting { key })?;
            osu_overrides.insert(key.to_owned(), Value::Bool(parsed));
        }
    }
    for (key, min) in [("POLL_RATE", 100.0), ("PRECISE_DATA_POLL_RATE", 1.0)] {
        if let Some(value) = table.get(key) {
            let raw = toml_number(value).ok_or(MigrationError::InvalidSetting { key })?;
            let clamped = clamp_interval(raw, min);
            osu_overrides.insert(key.to_owned(), json_number(clamped, key)?);
        }
    }
    if let Some(value) = table.get(OVERLAY_HOST_LEGACY_KEY) {
        let parsed = value.as_str().ok_or(MigrationError::InvalidSetting {
            key: OVERLAY_HOST_LEGACY_KEY,
        })?;
        overlay_host_overrides.insert(
            OVERLAY_HOST_SETTING_KEY.to_owned(),
            Value::String(parsed.to_owned()),
        );
    }
    Ok(())
}

fn extract_env_moved_keys(
    env: &HashMap<String, String>,
    osu_overrides: &mut Map<String, Value>,
    overlay_host_overrides: &mut Map<String, Value>,
) -> Result<(), MigrationError> {
    for key in [
        "CALCULATE_PP",
        "SHOW_MP_COMMANDS",
        "READ_MANIA_SCROLL_SPEED",
    ] {
        if let Some(raw) = env.get(key) {
            let parsed = match raw.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(MigrationError::InvalidSetting { key }),
            };
            osu_overrides.insert(key.to_owned(), Value::Bool(parsed));
        }
    }
    for (key, min) in [("POLL_RATE", 100.0), ("PRECISE_DATA_POLL_RATE", 1.0)] {
        if let Some(raw) = env.get(key) {
            let number = parse_real_number(raw).ok_or(MigrationError::InvalidSetting { key })?;
            let clamped = clamp_interval(number, min);
            osu_overrides.insert(key.to_owned(), json_number(clamped, key)?);
        }
    }
    if let Some(raw) = env.get(OVERLAY_HOST_LEGACY_KEY) {
        overlay_host_overrides.insert(
            OVERLAY_HOST_SETTING_KEY.to_owned(),
            Value::String(raw.clone()),
        );
    }
    Ok(())
}

/// Strict dotenv parser matching `config/migration.rs`'s syntax while failing
/// closed on malformed lines, invalid key names, or unterminated quotes.
fn parse_env_strict(
    content: &str,
    label: &'static str,
) -> Result<HashMap<String, String>, MigrationError> {
    let mut parsed = HashMap::new();
    for original_line in content.lines() {
        let mut line = original_line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        line = strip_export(line);

        let (key, raw_value) =
            split_env_binding(line).ok_or(MigrationError::MalformedInput { label })?;
        let key = key.trim();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(MigrationError::MalformedInput { label });
        }

        let value = parse_env_value_strict(raw_value, label)?;
        parsed.insert(key.to_owned(), value);
    }
    Ok(parsed)
}

fn strip_export(line: &str) -> &str {
    let Some(rest) = line.strip_prefix("export") else {
        return line;
    };
    if rest.starts_with(char::is_whitespace) {
        rest.trim_start()
    } else {
        line
    }
}

fn split_env_binding(line: &str) -> Option<(&str, &str)> {
    if let Some(binding) = line.split_once('=') {
        return Some(binding);
    }
    let (key, value) = line.split_once(':')?;
    value
        .starts_with(char::is_whitespace)
        .then_some((key, value.trim_start()))
}

fn parse_env_value_strict(raw: &str, label: &'static str) -> Result<String, MigrationError> {
    let value = raw.trim();
    let Some(quote) = value
        .as_bytes()
        .first()
        .copied()
        .filter(|quote| matches!(quote, b'\'' | b'"' | b'`'))
    else {
        return Ok(value
            .split_once('#')
            .map_or(value, |(before_comment, _)| before_comment)
            .trim_end()
            .to_owned());
    };

    let mut escaped = false;
    let closing_quote = value.as_bytes()[1..]
        .iter()
        .position(|byte| {
            if escaped {
                escaped = false;
                return false;
            }
            if *byte == b'\\' {
                escaped = true;
                return false;
            }
            *byte == quote
        })
        .map(|position| position + 1)
        .ok_or(MigrationError::MalformedInput { label })?;

    let trailing = value[closing_quote + 1..].trim();
    if !trailing.is_empty() && !trailing.starts_with('#') {
        return Err(MigrationError::MalformedInput { label });
    }

    let inner = &value[1..closing_quote];
    if quote == b'"' {
        Ok(inner.replace("\\n", "\n").replace("\\r", "\r"))
    } else {
        Ok(inner.to_owned())
    }
}

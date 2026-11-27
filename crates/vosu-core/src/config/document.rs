use super::parser::{format_number, parse_toml};
use super::schema::{
    BINDING_ORDER, Config, ConfigDefault, ConfigKey, DEFAULT_SCHEMA, NEWLINE_INSERTIONS,
};
use std::fmt::Write as _;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigDocument {
    config: Config,
    unknown: toml::Table,
}

impl ConfigDocument {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            unknown: toml::Table::new(),
        }
    }

    /// Parses a TOML document over the schema defaults.
    pub fn parse(content: &str) -> Result<Self, toml::de::Error> {
        let table = parse_toml(content)?;
        let mut document = Self::default();
        document.apply_table(&table);
        let _ = document.validate();
        Ok(document)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut Config {
        &mut self.config
    }

    pub fn into_config(self) -> Config {
        self.config
    }

    /// Applies bindings present in `content`, retaining current values for
    /// missing bindings.
    ///
    /// Unknown bindings are preserved without being applied. A known binding with the wrong TOML type,
    /// including a non-finite number, is reset to that binding's schema
    /// default. The document is parsed completely before any value is changed,
    /// so a syntax error leaves the current configuration untouched.
    pub fn refresh(&mut self, content: &str) -> Result<Vec<String>, toml::de::Error> {
        let table = parse_toml(content)?;
        self.apply_table(&table);
        Ok(self.validate())
    }

    pub(super) fn apply_table(&mut self, table: &toml::Table) {
        for (key, value) in table {
            if !DEFAULT_SCHEMA
                .items()
                .iter()
                .any(|item| item.binding == key)
                && !is_stripped_binding(key)
            {
                self.unknown.insert(key.clone(), value.clone());
            }
        }
        for schema_item in DEFAULT_SCHEMA.items() {
            let Some(value) = table.get(schema_item.binding) else {
                continue;
            };

            match schema_item.default {
                ConfigDefault::Boolean(default) => {
                    self.set_boolean(schema_item.key, value.as_bool().unwrap_or(default));
                }
                ConfigDefault::Number(default) => {
                    let value = match value {
                        toml::Value::Integer(value) => *value as f64,
                        toml::Value::Float(value) if value.is_finite() => *value,
                        _ => default,
                    };
                    self.set_number(schema_item.key, value);
                }
                ConfigDefault::String(default) => {
                    self.set_string(
                        schema_item.key,
                        value.as_str().unwrap_or(default).to_owned(),
                    );
                }
            }
        }
    }

    /// Practical timer ceiling, also safe for `Instant` arithmetic.
    pub const MAX_INTERVAL_MS: f64 = 60_000.0;

    /// Clamps subscription intervals and package size caps to supported bounds.
    pub fn validate(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.config.max_package_bytes().is_none() {
            warnings.push(
                "Config %MAX_PACKAGE_MIB% must be an integer from %1% to %4096%; using %512%."
                    .into(),
            );
            self.config.max_package_mib = 512.0;
        }

        if self.config.api_v1_subscription_debounce < 1.0 {
            warnings.push(format!(
                "Config %API_V1_SUBSCRIPTION_DEBOUNCE% value %{}% is below the minimum %1%, clamping.",
                format_number(self.config.api_v1_subscription_debounce)
            ));
            self.config.api_v1_subscription_debounce = 1.0;
        }

        if !self.config.api_v1_subscription_debounce.is_finite()
            || self.config.api_v1_subscription_debounce > Self::MAX_INTERVAL_MS
        {
            warnings.push(format!(
                "Config %API_V1_SUBSCRIPTION_DEBOUNCE% interval is outside the supported range, clamping to %{}% ms.",
                Self::MAX_INTERVAL_MS
            ));
            self.config.api_v1_subscription_debounce = Self::MAX_INTERVAL_MS;
        }

        warnings
    }

    /// Serializes as canonical flat TOML in binding order and blank-line groups.
    pub fn serialize(&self) -> String {
        let mut output = String::with_capacity(512);

        for (index, binding) in BINDING_ORDER.iter().enumerate() {
            if index != 0 {
                output.push('\n');
            }
            output.push_str(binding);
            output.push_str(" = ");
            write!(&mut output, "{}", self.toml_value_for_binding(binding))
                .expect("writing to a String cannot fail");
            if NEWLINE_INSERTIONS.contains(binding) {
                output.push('\n');
            }
        }
        output.push('\n');
        if !self.unknown.is_empty() {
            output.push('\n');
            output.push_str(&toml::to_string(&self.unknown).expect("parsed TOML values serialize"));
        }

        output
    }

    fn set_boolean(&mut self, key: ConfigKey, value: bool) {
        match key {
            ConfigKey::EnableAutoUpdate => self.config.enable_auto_update = value,
            ConfigKey::EnableDashboard => self.config.enable_dashboard = value,
            ConfigKey::OpenDashboardOnStartup => self.config.open_dashboard_on_startup = value,
            ConfigKey::StartAtLogin => self.config.start_at_login = value,
            ConfigKey::DebugLog => self.config.debug_log = value,
            _ => unreachable!("schema boolean key has boolean storage"),
        }
    }

    fn set_number(&mut self, key: ConfigKey, value: f64) {
        match key {
            ConfigKey::ApiV1SubscriptionDebounce => {
                self.config.api_v1_subscription_debounce = value
            }
            ConfigKey::ServerPort => self.config.server_port = value,
            ConfigKey::MaxPackageMib => self.config.max_package_mib = value,
            ConfigKey::DashboardPort => self.config.dashboard_port = value,
            _ => unreachable!("schema number key has number storage"),
        }
    }

    fn set_string(&mut self, key: ConfigKey, value: String) {
        match key {
            ConfigKey::ServerIp => self.config.server_ip = value,
            ConfigKey::DashboardIp => self.config.dashboard_ip = value,
            ConfigKey::AllowedIps => self.config.allowed_ips = value,
            _ => unreachable!("schema string key has string storage"),
        }
    }

    fn toml_value_for_binding(&self, binding: &str) -> toml::Value {
        match binding {
            "ENABLE_AUTOUPDATE" => toml::Value::Boolean(self.config.enable_auto_update),
            "ENABLE_DASHBOARD" => toml::Value::Boolean(self.config.enable_dashboard),
            "OPEN_DASHBOARD_ON_STARTUP" => {
                toml::Value::Boolean(self.config.open_dashboard_on_startup)
            }
            "START_AT_LOGIN" => toml::Value::Boolean(self.config.start_at_login),
            "DEBUG_LOG" => toml::Value::Boolean(self.config.debug_log),
            "API_V1_SUBSCRIPTION_DEBOUNCE" => {
                toml::Value::Float(self.config.api_v1_subscription_debounce)
            }
            "SERVER_IP" => toml::Value::String(self.config.server_ip.clone()),
            "SERVER_PORT" => toml::Value::Float(self.config.server_port),
            "DASHBOARD_IP" => toml::Value::String(self.config.dashboard_ip.clone()),
            "DASHBOARD_PORT" => toml::Value::Float(self.config.dashboard_port),
            "ALLOWED_IPS" => toml::Value::String(self.config.allowed_ips.clone()),
            "MAX_PACKAGE_MIB" => toml::Value::Integer(self.config.max_package_mib as i64),
            _ => unreachable!("binding order contains only schema bindings"),
        }
    }
}

pub(super) const RETIRED_BINDINGS: [&str; 4] = [
    "ENABLE_KEY_OVERLAY",
    "ENABLE_MANIA_KEY_HIT_ERRORS",
    "ENABLE_HIT_OFFSETS",
    "V2_SUBSCRIPTION_DEBOUNCE",
];

/// Top-level 1.x keys moved to plugin settings (`vosu.osu` per D19 and
/// `vosu.overlayhost` per A7.1-OR). Stripped from `vosu.toml` rather than
/// preserved as unknown keys.
pub(super) const MOVED_BINDINGS: [&str; 6] = [
    "CALCULATE_PP",
    "POLL_RATE",
    "PRECISE_DATA_POLL_RATE",
    "SHOW_MP_COMMANDS",
    "READ_MANIA_SCROLL_SPEED",
    "STATIC_FOLDER_PATH",
];

pub(super) fn is_stripped_binding(key: &str) -> bool {
    RETIRED_BINDINGS.contains(&key) || MOVED_BINDINGS.contains(&key)
}

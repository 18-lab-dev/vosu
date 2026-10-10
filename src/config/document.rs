use super::parser::{format_number, parse_toml};
use super::schema::{
    BINDING_ORDER, Config, ConfigDefault, ConfigKey, DEFAULT_SCHEMA, NEWLINE_INSERTIONS,
};
use std::fmt::Write as _;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigDocument {
    config: Config,
}

impl ConfigDocument {
    pub fn new(config: Config) -> Self {
        Self { config }
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
    /// Unknown bindings are ignored. A known binding with the wrong TOML type,
    /// including a non-finite number, is reset to that binding's schema
    /// default. The document is parsed completely before any value is changed,
    /// so a syntax error leaves the current configuration untouched.
    pub fn refresh(&mut self, content: &str) -> Result<Vec<String>, toml::de::Error> {
        let table = parse_toml(content)?;
        self.apply_table(&table);
        Ok(self.validate())
    }

    pub(super) fn apply_table(&mut self, table: &toml::Table) {
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

    /// Clamps polling and subscription intervals to supported timer bounds.
    pub fn validate(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();

        if self.config.poll_rate < 100.0 {
            warnings.push(format!(
                "Config %POLL_RATE% value %{}% is below the minimum %100%, clamping.",
                format_number(self.config.poll_rate)
            ));
            self.config.poll_rate = 100.0;
        }

        if self.config.precise_data_poll_rate < 1.0 {
            warnings.push(format!(
                "Config %PRECISE_DATA_POLL_RATE% value %{}% is below the minimum %1%, clamping.",
                format_number(self.config.precise_data_poll_rate)
            ));
            self.config.precise_data_poll_rate = 1.0;
        }

        if self.config.v2_subscription_debounce < 1.0 {
            warnings.push(format!(
                "Config %V2_SUBSCRIPTION_DEBOUNCE% value %{}% is below the minimum %1%, clamping.",
                format_number(self.config.v2_subscription_debounce)
            ));
            self.config.v2_subscription_debounce = 1.0;
        }

        for (name, interval) in [
            ("POLL_RATE", &mut self.config.poll_rate),
            (
                "PRECISE_DATA_POLL_RATE",
                &mut self.config.precise_data_poll_rate,
            ),
            (
                "V2_SUBSCRIPTION_DEBOUNCE",
                &mut self.config.v2_subscription_debounce,
            ),
        ] {
            if !interval.is_finite() || *interval > Self::MAX_INTERVAL_MS {
                warnings.push(format!(
                    "Config %{name}% interval is outside the supported range, clamping to %{}% ms.",
                    Self::MAX_INTERVAL_MS
                ));
                *interval = Self::MAX_INTERVAL_MS;
            }
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

        output
    }

    fn set_boolean(&mut self, key: ConfigKey, value: bool) {
        match key {
            ConfigKey::EnableAutoUpdate => self.config.enable_auto_update = value,
            ConfigKey::OpenDashboardOnStartup => self.config.open_dashboard_on_startup = value,
            ConfigKey::StartAtLogin => self.config.start_at_login = value,
            ConfigKey::DebugLog => self.config.debug_log = value,
            ConfigKey::CalculatePp => self.config.calculate_pp = value,
            ConfigKey::EnableKeyOverlay => self.config.enable_key_overlay = value,
            ConfigKey::ShowMpCommands => self.config.show_mp_commands = value,
            ConfigKey::ReadManiaScrollSpeed => self.config.read_mania_scroll_speed = value,
            _ => unreachable!("schema boolean key has boolean storage"),
        }
    }

    fn set_number(&mut self, key: ConfigKey, value: f64) {
        match key {
            ConfigKey::PollRate => self.config.poll_rate = value,
            ConfigKey::PreciseDataPollRate => self.config.precise_data_poll_rate = value,
            ConfigKey::V2SubscriptionDebounce => self.config.v2_subscription_debounce = value,
            ConfigKey::ServerPort => self.config.server_port = value,
            _ => unreachable!("schema number key has number storage"),
        }
    }

    fn set_string(&mut self, key: ConfigKey, value: String) {
        match key {
            ConfigKey::ServerIp => self.config.server_ip = value,
            ConfigKey::StaticFolderPath => self.config.static_folder_path = value,
            ConfigKey::AllowedIps => self.config.allowed_ips = value,
            _ => unreachable!("schema string key has string storage"),
        }
    }

    fn toml_value_for_binding(&self, binding: &str) -> toml::Value {
        match binding {
            "ENABLE_AUTOUPDATE" => toml::Value::Boolean(self.config.enable_auto_update),
            "OPEN_DASHBOARD_ON_STARTUP" => {
                toml::Value::Boolean(self.config.open_dashboard_on_startup)
            }
            "START_AT_LOGIN" => toml::Value::Boolean(self.config.start_at_login),
            "DEBUG_LOG" => toml::Value::Boolean(self.config.debug_log),
            "CALCULATE_PP" => toml::Value::Boolean(self.config.calculate_pp),
            "ENABLE_KEY_OVERLAY" => toml::Value::Boolean(self.config.enable_key_overlay),
            "POLL_RATE" => toml::Value::Float(self.config.poll_rate),
            "PRECISE_DATA_POLL_RATE" => toml::Value::Float(self.config.precise_data_poll_rate),
            "V2_SUBSCRIPTION_DEBOUNCE" => toml::Value::Float(self.config.v2_subscription_debounce),
            "SHOW_MP_COMMANDS" => toml::Value::Boolean(self.config.show_mp_commands),
            "READ_MANIA_SCROLL_SPEED" => toml::Value::Boolean(self.config.read_mania_scroll_speed),
            "SERVER_IP" => toml::Value::String(self.config.server_ip.clone()),
            "SERVER_PORT" => toml::Value::Float(self.config.server_port),
            "STATIC_FOLDER_PATH" => toml::Value::String(self.config.static_folder_path.clone()),
            "ALLOWED_IPS" => toml::Value::String(self.config.allowed_ips.clone()),
            _ => unreachable!("binding order contains only schema bindings"),
        }
    }
}

use super::parser::{format_number, parse_env, parse_real_number};
use super::schema::{
    BINDING_ORDER, Config, ConfigDefault, ConfigKey, DEFAULT_SCHEMA, NEWLINE_INSERTIONS,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigDocument {
    config: Config,
}

impl ConfigDocument {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Parses an env document over the source defaults.
    pub fn parse(content: &str) -> Self {
        let mut store = Self::default();
        store.refresh(content);
        store
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
    /// missing bindings exactly like `refreshConfig`.
    ///
    /// Unknown bindings and malformed env lines are ignored. Invalid numbers
    /// use that binding's schema default; booleans are true only for the exact
    /// unquoted value `true`. The returned messages reproduce source clamp
    /// warnings and can later be forwarded to the logging subsystem.
    pub fn refresh(&mut self, content: &str) -> Vec<String> {
        let env = parse_env(content);

        for schema_item in DEFAULT_SCHEMA.items() {
            let Some(raw) = env.get(schema_item.binding) else {
                continue;
            };

            match schema_item.default {
                ConfigDefault::Boolean(_) => {
                    self.set_boolean(schema_item.key, raw == "true");
                }
                ConfigDefault::Number(default) => {
                    let value = parse_real_number(raw).unwrap_or(default);
                    self.set_number(schema_item.key, value);
                }
                ConfigDefault::String(_) => self.set_string(schema_item.key, raw.clone()),
            }
        }

        self.validate()
    }

    /// Clamps schema-defined polling and subscription minima.
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

        if self.config.v2_subscription_debounce < 0.0 {
            warnings.push(format!(
                "Config %V2_SUBSCRIPTION_DEBOUNCE% value %{}% below minimum %0%, clamping.",
                format_number(self.config.v2_subscription_debounce)
            ));
            self.config.v2_subscription_debounce = 0.0;
        }

        warnings
    }

    /// Serializes with the exact source binding order and blank-line groups.
    pub fn serialize(&self) -> String {
        let mut output = String::new();

        for (index, binding) in BINDING_ORDER.iter().enumerate() {
            if index != 0 {
                output.push('\n');
            }
            output.push_str(binding);
            output.push('=');
            output.push_str(&self.value_for_binding(binding));
            if NEWLINE_INSERTIONS.contains(binding) {
                output.push('\n');
            }
        }

        output
    }

    fn set_boolean(&mut self, key: ConfigKey, value: bool) {
        match key {
            ConfigKey::EnableAutoUpdate => self.config.enable_auto_update = value,
            ConfigKey::OpenDashboardOnStartup => self.config.open_dashboard_on_startup = value,
            ConfigKey::DebugLog => self.config.debug_log = value,
            ConfigKey::CalculatePp => self.config.calculate_pp = value,
            ConfigKey::EnableKeyOverlay => self.config.enable_key_overlay = value,
            ConfigKey::ShowMpCommands => self.config.show_mp_commands = value,
            ConfigKey::ReadManiaScrollSpeed => self.config.read_mania_scroll_speed = value,
            ConfigKey::EnableIngameOverlay => self.config.enable_ingame_overlay = value,
            _ => unreachable!("schema boolean key has boolean storage"),
        }
    }

    fn set_number(&mut self, key: ConfigKey, value: f64) {
        match key {
            ConfigKey::PollRate => self.config.poll_rate = value,
            ConfigKey::PreciseDataPollRate => self.config.precise_data_poll_rate = value,
            ConfigKey::V2SubscriptionDebounce => self.config.v2_subscription_debounce = value,
            ConfigKey::ServerPort => self.config.server_port = value,
            ConfigKey::IngameOverlayMaxFps => self.config.ingame_overlay_max_fps = value,
            _ => unreachable!("schema number key has number storage"),
        }
    }

    fn set_string(&mut self, key: ConfigKey, value: String) {
        match key {
            ConfigKey::ServerIp => self.config.server_ip = value,
            ConfigKey::StaticFolderPath => self.config.static_folder_path = value,
            ConfigKey::IngameOverlayKeybind => self.config.ingame_overlay_keybind = value,
            ConfigKey::AllowedIps => self.config.allowed_ips = value,
            _ => unreachable!("schema string key has string storage"),
        }
    }

    pub(super) fn value_for_binding(&self, binding: &str) -> String {
        match binding {
            "ENABLE_AUTOUPDATE" => self.config.enable_auto_update.to_string(),
            "OPEN_DASHBOARD_ON_STARTUP" => self.config.open_dashboard_on_startup.to_string(),
            "DEBUG_LOG" => self.config.debug_log.to_string(),
            "CALCULATE_PP" => self.config.calculate_pp.to_string(),
            "ENABLE_KEY_OVERLAY" => self.config.enable_key_overlay.to_string(),
            "POLL_RATE" => format_number(self.config.poll_rate),
            "PRECISE_DATA_POLL_RATE" => format_number(self.config.precise_data_poll_rate),
            "V2_SUBSCRIPTION_DEBOUNCE" => format_number(self.config.v2_subscription_debounce),
            "SHOW_MP_COMMANDS" => self.config.show_mp_commands.to_string(),
            "READ_MANIA_SCROLL_SPEED" => self.config.read_mania_scroll_speed.to_string(),
            "SERVER_IP" => self.config.server_ip.clone(),
            "SERVER_PORT" => format_number(self.config.server_port),
            "STATIC_FOLDER_PATH" => self.config.static_folder_path.clone(),
            "ENABLE_INGAME_OVERLAY" => self.config.enable_ingame_overlay.to_string(),
            "INGAME_OVERLAY_KEYBIND" => self.config.ingame_overlay_keybind.clone(),
            "INGAME_OVERLAY_MAX_FPS" => format_number(self.config.ingame_overlay_max_fps),
            "ALLOWED_IPS" => self.config.allowed_ips.clone(),
            _ => unreachable!("binding order contains only schema bindings"),
        }
    }
}

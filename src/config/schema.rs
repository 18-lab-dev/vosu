/// The daemon configuration, using the source application's numeric semantics.
///
/// TypeScript stores every numeric binding as a JavaScript `number`, so these
/// fields remain `f64`. Consumers that need an integer (for example, a TCP port)
/// must validate it at that boundary instead of changing config parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub enable_auto_update: bool,
    pub open_dashboard_on_startup: bool,
    pub debug_log: bool,
    pub calculate_pp: bool,
    pub enable_key_overlay: bool,
    pub poll_rate: f64,
    pub precise_data_poll_rate: f64,
    pub v2_subscription_debounce: f64,
    pub show_mp_commands: bool,
    pub read_mania_scroll_speed: bool,
    pub server_ip: String,
    pub server_port: f64,
    pub static_folder_path: String,
    pub allowed_ips: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enable_auto_update: true,
            open_dashboard_on_startup: true,
            debug_log: false,
            calculate_pp: true,
            enable_key_overlay: true,
            poll_rate: 150.0,
            precise_data_poll_rate: 10.0,
            v2_subscription_debounce: 0.0,
            show_mp_commands: false,
            read_mania_scroll_speed: true,
            server_ip: "127.0.0.1".to_owned(),
            server_port: 24_050.0,
            static_folder_path: "./static".to_owned(),
            allowed_ips: "127.0.0.1,localhost,absolute".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigKey {
    EnableAutoUpdate,
    OpenDashboardOnStartup,
    DebugLog,
    CalculatePp,
    EnableKeyOverlay,
    PollRate,
    PreciseDataPollRate,
    V2SubscriptionDebounce,
    ShowMpCommands,
    ReadManiaScrollSpeed,
    ServerIp,
    ServerPort,
    StaticFolderPath,
    AllowedIps,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ConfigDefault {
    Boolean(bool),
    Number(f64),
    String(&'static str),
}

/// One entry in the source `defaultSchema`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfigItem {
    pub key: ConfigKey,
    pub binding: &'static str,
    pub default: ConfigDefault,
    pub min: Option<f64>,
}

/// Schema metadata in the source declaration order.
#[derive(Clone, Copy, Debug)]
pub struct ConfigSchema {
    items: &'static [ConfigItem],
}

impl ConfigSchema {
    pub const fn items(self) -> &'static [ConfigItem] {
        self.items
    }
}

impl Default for ConfigSchema {
    fn default() -> Self {
        DEFAULT_SCHEMA
    }
}

const SCHEMA_ITEMS: [ConfigItem; 14] = [
    item(
        ConfigKey::EnableAutoUpdate,
        "ENABLE_AUTOUPDATE",
        ConfigDefault::Boolean(true),
        None,
    ),
    item(
        ConfigKey::OpenDashboardOnStartup,
        "OPEN_DASHBOARD_ON_STARTUP",
        ConfigDefault::Boolean(true),
        None,
    ),
    item(
        ConfigKey::DebugLog,
        "DEBUG_LOG",
        ConfigDefault::Boolean(false),
        None,
    ),
    item(
        ConfigKey::CalculatePp,
        "CALCULATE_PP",
        ConfigDefault::Boolean(true),
        None,
    ),
    item(
        ConfigKey::EnableKeyOverlay,
        "ENABLE_KEY_OVERLAY",
        ConfigDefault::Boolean(true),
        None,
    ),
    item(
        ConfigKey::PollRate,
        "POLL_RATE",
        ConfigDefault::Number(150.0),
        Some(100.0),
    ),
    item(
        ConfigKey::PreciseDataPollRate,
        "PRECISE_DATA_POLL_RATE",
        ConfigDefault::Number(10.0),
        Some(1.0),
    ),
    item(
        ConfigKey::V2SubscriptionDebounce,
        "V2_SUBSCRIPTION_DEBOUNCE",
        ConfigDefault::Number(0.0),
        Some(0.0),
    ),
    item(
        ConfigKey::ShowMpCommands,
        "SHOW_MP_COMMANDS",
        ConfigDefault::Boolean(false),
        None,
    ),
    item(
        ConfigKey::ReadManiaScrollSpeed,
        "READ_MANIA_SCROLL_SPEED",
        ConfigDefault::Boolean(true),
        None,
    ),
    item(
        ConfigKey::ServerIp,
        "SERVER_IP",
        ConfigDefault::String("127.0.0.1"),
        None,
    ),
    item(
        ConfigKey::ServerPort,
        "SERVER_PORT",
        ConfigDefault::Number(24_050.0),
        None,
    ),
    item(
        ConfigKey::StaticFolderPath,
        "STATIC_FOLDER_PATH",
        ConfigDefault::String("./static"),
        None,
    ),
    item(
        ConfigKey::AllowedIps,
        "ALLOWED_IPS",
        ConfigDefault::String("127.0.0.1,localhost,absolute"),
        None,
    ),
];

const fn item(
    key: ConfigKey,
    binding: &'static str,
    default: ConfigDefault,
    min: Option<f64>,
) -> ConfigItem {
    ConfigItem {
        key,
        binding,
        default,
        min,
    }
}

pub const DEFAULT_SCHEMA: ConfigSchema = ConfigSchema {
    items: &SCHEMA_ITEMS,
};

/// The source's serialization order, which intentionally differs from schema
/// declaration order.
pub const BINDING_ORDER: [&str; 14] = [
    "DEBUG_LOG",
    "ENABLE_AUTOUPDATE",
    "OPEN_DASHBOARD_ON_STARTUP",
    "SHOW_MP_COMMANDS",
    "CALCULATE_PP",
    "READ_MANIA_SCROLL_SPEED",
    "ENABLE_KEY_OVERLAY",
    "POLL_RATE",
    "PRECISE_DATA_POLL_RATE",
    "V2_SUBSCRIPTION_DEBOUNCE",
    "SERVER_IP",
    "SERVER_PORT",
    "ALLOWED_IPS",
    "STATIC_FOLDER_PATH",
];

/// A blank line is inserted after each of these bindings.
pub const NEWLINE_INSERTIONS: [&str; 5] = [
    "OPEN_DASHBOARD_ON_STARTUP",
    "READ_MANIA_SCROLL_SPEED",
    "ENABLE_KEY_OVERLAY",
    "V2_SUBSCRIPTION_DEBOUNCE",
    "ALLOWED_IPS",
];

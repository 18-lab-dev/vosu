/// The daemon configuration, using the source application's numeric semantics.
///
/// TypeScript stores every numeric binding as a JavaScript `number`, so these
/// fields remain `f64`. Consumers that need an integer (for example, a TCP port)
/// must validate it at that boundary instead of changing config parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub enable_auto_update: bool,
    /// A7.3: whether the dashboard listener and host run. Read once at
    /// startup; a live edit needs a restart, and no dashboard route changes it.
    pub enable_dashboard: bool,
    pub open_dashboard_on_startup: bool,
    pub start_at_login: bool,
    pub debug_log: bool,
    pub api_v1_subscription_debounce: f64,
    pub server_ip: String,
    pub server_port: f64,
    pub dashboard_ip: String,
    pub dashboard_port: f64,
    pub allowed_ips: String,
    pub max_package_mib: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enable_auto_update: true,
            enable_dashboard: true,
            open_dashboard_on_startup: true,
            start_at_login: false,
            debug_log: false,
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

impl Config {
    /// Checked local package cap shared by resolver and download preparation.
    /// Invalid programmatic values fail closed rather than saturating a cast.
    pub fn max_package_bytes(&self) -> Option<u64> {
        if !self.max_package_mib.is_finite()
            || self.max_package_mib.fract() != 0.0
            || !(1.0..=4096.0).contains(&self.max_package_mib)
        {
            return None;
        }
        (self.max_package_mib as u64).checked_mul(1_048_576)
    }

    /// Effective API v1 hub debounce after configuration validation.
    pub fn api_v1_subscription_debounce_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.api_v1_subscription_debounce / 1_000.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigKey {
    EnableAutoUpdate,
    EnableDashboard,
    OpenDashboardOnStartup,
    StartAtLogin,
    DebugLog,
    ApiV1SubscriptionDebounce,
    ServerIp,
    ServerPort,
    DashboardIp,
    DashboardPort,
    AllowedIps,
    MaxPackageMib,
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

const SCHEMA_ITEMS: [ConfigItem; 12] = [
    item(
        ConfigKey::EnableAutoUpdate,
        "ENABLE_AUTOUPDATE",
        ConfigDefault::Boolean(true),
        None,
    ),
    item(
        ConfigKey::EnableDashboard,
        "ENABLE_DASHBOARD",
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
        ConfigKey::StartAtLogin,
        "START_AT_LOGIN",
        ConfigDefault::Boolean(false),
        None,
    ),
    item(
        ConfigKey::DebugLog,
        "DEBUG_LOG",
        ConfigDefault::Boolean(false),
        None,
    ),
    item(
        ConfigKey::ApiV1SubscriptionDebounce,
        "API_V1_SUBSCRIPTION_DEBOUNCE",
        ConfigDefault::Number(1.0),
        Some(1.0),
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
        ConfigKey::DashboardIp,
        "DASHBOARD_IP",
        ConfigDefault::String("127.0.0.1"),
        None,
    ),
    item(
        ConfigKey::DashboardPort,
        "DASHBOARD_PORT",
        ConfigDefault::Number(24_051.0),
        None,
    ),
    item(
        ConfigKey::AllowedIps,
        "ALLOWED_IPS",
        ConfigDefault::String("127.0.0.1,localhost,absolute"),
        None,
    ),
    item(
        ConfigKey::MaxPackageMib,
        "MAX_PACKAGE_MIB",
        ConfigDefault::Number(512.0),
        Some(1.0),
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

/// Canonical serialization order for core `vosu.toml` bindings.
pub const BINDING_ORDER: [&str; 12] = [
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

/// A blank line is inserted after each of these bindings.
pub const NEWLINE_INSERTIONS: [&str; 3] = [
    "START_AT_LOGIN",
    "API_V1_SUBSCRIPTION_DEBOUNCE",
    "ALLOWED_IPS",
];

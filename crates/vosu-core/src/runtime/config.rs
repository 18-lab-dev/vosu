use super::*;

pub(super) const DEFAULT_OSU_POLL_RATE_MS: f64 = 150.0;
pub(super) const DEFAULT_OSU_PRECISE_POLL_RATE_MS: f64 = 10.0;
const MIN_OSU_POLL_RATE_MS: f64 = 100.0;
const MIN_OSU_PRECISE_POLL_RATE_MS: f64 = 1.0;

pub(super) fn migration_schemas(bundle: &Bundle) -> io::Result<crate::migration::MigrationSchemas> {
    let parse_schema =
        |plugin_id: &str| -> io::Result<vosu_plugin_api::manifest::settings::SettingsSchema> {
            let index = bundle
                .plugin_ids
                .iter()
                .position(|id| *id == plugin_id)
                .ok_or_else(|| {
                    io::Error::other(format!("required bundled plugin {plugin_id} missing"))
                })?;
            let text = bundle
                .plugins
                .get(index)
                .and_then(|entry| entry.settings_schema)
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "bundled plugin {plugin_id} declares no settings schema"
                    ))
                })?;
            vosu_plugin_api::manifest::settings::SettingsSchema::parse(text).map_err(|error| {
                io::Error::other(format!(
                    "bundled plugin {plugin_id} has invalid settings schema: {error}"
                ))
            })
        };
    Ok(crate::migration::MigrationSchemas {
        osu: parse_schema(crate::migration::OSU_PLUGIN_ID)?,
        overlay_host: parse_schema(crate::migration::OVERLAY_HOST_PLUGIN_ID)?,
    })
}

pub(super) async fn initialize_startup_storage(
    bundle: &Bundle,
    paths: &Paths,
    channel: vosu_sys::platform::BuildChannel,
) -> io::Result<(ConfigStore, Arc<PluginSettingsStore>)> {
    let settings_store = Arc::new(PluginSettingsStore::open(paths.config()));
    let target = paths.config().join("vosu.toml");
    match channel {
        vosu_sys::platform::BuildChannel::Release => {
            let schemas = migration_schemas(bundle)?;
            let config_root = paths.config().to_path_buf();
            let data_root = paths.data().to_path_buf();
            let store = Arc::clone(&settings_store);
            tokio::task::spawn_blocking(move || {
                crate::migration::run_local_migration(
                    &config_root,
                    &data_root,
                    channel,
                    &schemas,
                    &store,
                )
            })
            .await
            .map_err(|error| io::Error::other(format!("migration task failed: {error}")))?
            .map_err(|error| io::Error::other(error.to_string()))?;
            let config = ConfigStore::initialize(target).await?;
            Ok((config, settings_store))
        }
        vosu_sys::platform::BuildChannel::Prerelease => {
            let normal = paths
                .config()
                .parent()
                .ok_or_else(|| {
                    io::Error::other("prerelease configuration root has no normal parent")
                })?
                .to_path_buf();
            let target_check = target.clone();
            let first_run =
                tokio::task::spawn_blocking(move || is_prerelease_first_run(&target_check))
                    .await
                    .map_err(|error| {
                        io::Error::other(format!("prerelease probe task failed: {error}"))
                    })??;
            let config =
                ConfigStore::initialize_prerelease(target, normal.join("vosu.toml")).await?;
            if first_run {
                let osu_schema = migration_schemas(bundle)?.osu;
                let store = Arc::clone(&settings_store);
                tokio::task::spawn_blocking(move || {
                    import_prerelease_osu_settings(&normal, &osu_schema, &store);
                })
                .await
                .map_err(|error| {
                    io::Error::other(format!("prerelease osu settings import failed: {error}"))
                })?;
            }
            Ok((config, settings_store))
        }
    }
}

fn is_prerelease_first_run(target: &std::path::Path) -> io::Result<bool> {
    match vosu_sys::platform::read_published(target) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
    for local in [
        parent.join("vosu.env"),
        parent.join(crate::config::PRE_RENAME_ENV_FILENAME),
    ] {
        match std::fs::symlink_metadata(local) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// Read-only import of the 5 `vosu.osu` keys from the normal root into
/// prerelease `plugins/settings.json` on first-run prerelease bootstrap.
/// Never touches the normal root and leaves `vosu.overlayhost`'s
/// `static_folder_path` defaulted.
fn import_prerelease_osu_settings(
    normal_config: &std::path::Path,
    osu_schema: &vosu_plugin_api::manifest::settings::SettingsSchema,
    prerelease_store: &PluginSettingsStore,
) {
    let mut overrides = serde_json::Map::new();
    for env_name in [crate::config::PRE_RENAME_ENV_FILENAME, "vosu.env"] {
        if let Ok(content) = std::fs::read_to_string(normal_config.join(env_name)) {
            extract_prerelease_env_osu(&content, &mut overrides);
        }
    }
    if let Ok(bytes) = vosu_sys::platform::read_published(&normal_config.join("vosu.toml"))
        && let Ok(text) = String::from_utf8(bytes)
        && let Ok(table) = toml::from_str::<toml::Table>(&text)
    {
        extract_prerelease_toml_osu(&table, &mut overrides);
    }
    if let Ok(existing_normal) =
        PluginSettingsStore::open(normal_config).load(crate::migration::OSU_PLUGIN_ID)
    {
        for key in crate::migration::OSU_MOVED_KEYS {
            if let Some(value) = existing_normal.get(key) {
                let candidate = serde_json::Map::from_iter([(key.to_owned(), value.clone())]);
                if osu_schema.merge(&candidate).is_ok() {
                    overrides.insert(key.to_owned(), value.clone());
                }
            }
        }
    }
    if !overrides.is_empty()
        && let Err(error) = prerelease_store.import_missing_overrides(
            crate::migration::OSU_PLUGIN_ID,
            osu_schema,
            &overrides,
        )
    {
        tracing::warn!(%error, "prerelease osu! settings import skipped");
    }
}

fn extract_prerelease_env_osu(
    content: &str,
    overrides: &mut serde_json::Map<String, serde_json::Value>,
) {
    for line in content.lines() {
        let mut trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("export")
            && rest.starts_with(char::is_whitespace)
        {
            trimmed = rest.trim_start();
        }
        let Some((key, raw_value)) = trimmed.split_once('=').or_else(|| {
            let (key, value) = trimmed.split_once(':')?;
            value
                .starts_with(char::is_whitespace)
                .then_some((key, value.trim_start()))
        }) else {
            continue;
        };
        let key = key.trim();
        let value = raw_value
            .trim()
            .split_once('#')
            .map_or(raw_value.trim(), |(before, _)| before)
            .trim_end()
            .trim_matches(['"', '\'', '`']);
        match key {
            "CALCULATE_PP" | "SHOW_MP_COMMANDS" | "READ_MANIA_SCROLL_SPEED" => {
                overrides.insert(key.to_owned(), serde_json::Value::Bool(value == "true"));
            }
            "POLL_RATE" => {
                if let Some(number) = crate::config::parse_real_number(value)
                    && let Some(json) = serde_json::Number::from_f64(
                        number.clamp(MIN_OSU_POLL_RATE_MS, ConfigDocument::MAX_INTERVAL_MS),
                    )
                {
                    overrides.insert(key.to_owned(), serde_json::Value::Number(json));
                }
            }
            "PRECISE_DATA_POLL_RATE" => {
                if let Some(number) = crate::config::parse_real_number(value)
                    && let Some(json) = serde_json::Number::from_f64(number.clamp(
                        MIN_OSU_PRECISE_POLL_RATE_MS,
                        ConfigDocument::MAX_INTERVAL_MS,
                    ))
                {
                    overrides.insert(key.to_owned(), serde_json::Value::Number(json));
                }
            }
            _ => {}
        }
    }
}

fn extract_prerelease_toml_osu(
    table: &toml::Table,
    overrides: &mut serde_json::Map<String, serde_json::Value>,
) {
    for key in [
        "CALCULATE_PP",
        "SHOW_MP_COMMANDS",
        "READ_MANIA_SCROLL_SPEED",
    ] {
        if let Some(value) = table.get(key).and_then(toml::Value::as_bool) {
            overrides.insert(key.to_owned(), serde_json::Value::Bool(value));
        }
    }
    for (key, min) in [
        ("POLL_RATE", MIN_OSU_POLL_RATE_MS),
        ("PRECISE_DATA_POLL_RATE", MIN_OSU_PRECISE_POLL_RATE_MS),
    ] {
        let number = match table.get(key) {
            Some(toml::Value::Integer(value)) => Some(*value as f64),
            Some(toml::Value::Float(value)) if value.is_finite() => Some(*value),
            _ => None,
        };
        if let Some(number) = number
            && let Some(json) =
                serde_json::Number::from_f64(number.clamp(min, ConfigDocument::MAX_INTERVAL_MS))
        {
            overrides.insert(key.to_owned(), serde_json::Value::Number(json));
        }
    }
}

/// Effective `(POLL_RATE, PRECISE_DATA_POLL_RATE)` read from `vosu.osu`'s
/// plugin settings store, defaulting to `150.0` / `10.0` when unset or
/// unreadable and clamped to the supported interval bounds.
pub(super) fn effective_osu_poll_rates(settings: &PluginSettingsService) -> (f64, f64) {
    let Ok(overrides) = settings.stored_overrides(crate::migration::OSU_PLUGIN_ID) else {
        return (DEFAULT_OSU_POLL_RATE_MS, DEFAULT_OSU_PRECISE_POLL_RATE_MS);
    };
    let read_rate = |key: &str, min: f64, default: f64| {
        overrides
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite())
            .map_or(default, |value| {
                value.clamp(min, ConfigDocument::MAX_INTERVAL_MS)
            })
    };
    (
        read_rate("POLL_RATE", MIN_OSU_POLL_RATE_MS, DEFAULT_OSU_POLL_RATE_MS),
        read_rate(
            "PRECISE_DATA_POLL_RATE",
            MIN_OSU_PRECISE_POLL_RATE_MS,
            DEFAULT_OSU_PRECISE_POLL_RATE_MS,
        ),
    )
}

/// Resolves `vosu.overlayhost`'s stored `static_folder_path` override through
/// `Paths::static_path` before delivering `init.settings` or
/// `SETTINGS_CHANGED` to the overlay host plugin.
pub(super) fn overlay_root_delivery_transform(
    paths: Paths,
    setting: OverlayRootSetting,
) -> SettingsDeliveryTransform {
    Arc::new(move |plugin_id, mut effective| {
        if plugin_id != setting.plugin {
            return effective;
        }
        let configured = effective
            .get(setting.key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let resolved = match paths.static_path(&configured) {
            Ok(root) => root.to_string_lossy().into_owned(),
            Err(error) => {
                tracing::warn!(%error, "could not resolve overlay static_folder_path; delivering empty root");
                String::new()
            }
        };
        effective.insert(setting.key.to_owned(), serde_json::Value::String(resolved));
        effective
    })
}

/// Validates and resolves `vosu.overlayhost`'s `static_folder_path` setting
/// on `POST /api/plugins/vosu.overlayhost/settings` before persisting it.
pub(super) struct OverlayRootHandler {
    pub(super) setting: OverlayRootSetting,
    pub(super) paths: Paths,
    pub(super) overlay: Arc<dyn OverlayComposition>,
}

impl OverlayRootHandler {
    pub(super) fn resolve_submission(
        &self,
        plugin_id: &str,
        inputs: &[(String, serde_json::Value)],
    ) -> Result<Option<PathBuf>, String> {
        if plugin_id != self.setting.plugin {
            return Ok(None);
        }
        let Some((_, value)) = inputs.iter().rfind(|(key, _)| key == self.setting.key) else {
            return Ok(None);
        };
        let Some(raw) = value.as_str() else {
            return Ok(None);
        };
        self.validate_and_resolve(raw).map(Some)
    }

    pub(super) fn validate_and_resolve(&self, raw: &str) -> Result<PathBuf, String> {
        let trimmed = raw.trim();
        // A typo in an absolute path must not silently create an empty folder.
        // Empty or relative paths follow `Paths::static_path` semantics.
        if std::path::Path::new(trimmed).is_absolute() && !std::path::Path::new(trimmed).is_dir() {
            return Err(format!("Overlays folder not found: {trimmed}"));
        }
        self.paths
            .static_path(trimmed)
            .map_err(|error| format!("Overlays folder could not be opened: {error}"))
    }
}

/// Resolves the effective overlay root from `vosu.overlayhost`'s stored
/// `static_folder_path` setting (empty/unset -> `<data>/static`).
pub(super) fn resolved_overlay_root(
    paths: &Paths,
    settings: &PluginSettingsService,
    setting: OverlayRootSetting,
) -> io::Result<PathBuf> {
    let configured = settings
        .stored_overrides(setting.plugin)
        .ok()
        .and_then(|map| {
            map.get(setting.key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    paths.static_path(configured.trim())
}

pub(super) struct NetworkChange {
    generation: Arc<()>,
    deadline: tokio::time::Instant,
}

#[derive(Clone)]
pub(super) struct RuntimeConfig {
    pub(super) config: Arc<ConfigStore>,
    pub(super) login_state: Arc<AsyncMutex<bool>>,
    pub(super) network: Arc<AsyncMutex<Option<NetworkChange>>>,
    pub(super) origins: Option<Arc<crate::server::origins::Origins>>,
}

#[cfg(any(test, feature = "test-harness"))]
impl RuntimeConfig {
    pub(super) fn new(config: Arc<ConfigStore>) -> Self {
        let enabled = config.current().start_at_login;
        Self {
            config,
            login_state: Arc::new(AsyncMutex::new(enabled)),
            network: Arc::new(AsyncMutex::new(None)),
            origins: None,
        }
    }
}

impl ConfigUpdateService for RuntimeConfig {
    fn save(
        &self,
        values: serde_json::Value,
    ) -> crate::server::routes::ProviderFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let object = values
                .as_object()
                .ok_or_else(|| "settings payload must be an object".to_owned())?
                .clone();
            let action = object.get("_network_action").and_then(|v| v.as_str());
            if action == Some("confirm") {
                let current = self.config.current();
                if self.origins.as_ref().is_some_and(|origins| {
                    !origins.dashboard_target_matches(
                        &current.dashboard_ip,
                        current.dashboard_port as u16,
                    ) || !origins
                        .overlay_target_matches(&current.server_ip, current.server_port as u16)
                }) {
                    return Err(
                        "Both new listeners must be ready; network settings will revert".into(),
                    );
                }
                let wildcard = current
                    .dashboard_ip
                    .trim()
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_unspecified());
                let expected = crate::desktop::dashboard_url(
                    &current.dashboard_ip,
                    current.dashboard_port as u16,
                );
                let expected = url::Url::parse(&expected).map_err(|e| e.to_string())?;
                let actual = object
                    .get("_request_origin")
                    .and_then(|v| v.as_str())
                    .and_then(|v| url::Url::parse(v).ok())
                    .ok_or("Confirmation requires the new dashboard origin")?;
                let loopback = |host: &str| {
                    host == "localhost"
                        || host
                            .trim_matches(['[', ']'])
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                };
                if actual.port_or_known_default() != expected.port_or_known_default()
                    || (!wildcard
                        && actual.host_str() != expected.host_str()
                        && !(loopback(actual.host_str().unwrap_or_default())
                            && loopback(expected.host_str().unwrap_or_default())))
                {
                    return Err("Confirm from the new dashboard origin".into());
                }
                let mut pending = self.network.lock().await;
                if pending
                    .as_ref()
                    .is_none_or(|change| tokio::time::Instant::now() >= change.deadline)
                {
                    return Err("No unexpired pending network change".into());
                }
                self.config
                    .confirm_network()
                    .await
                    .map_err(|error| error.to_string())?;
                *pending = None;
                if let Some(origins) = &self.origins {
                    *origins.previous_dashboard.write() = None;
                }
                return Ok(());
            }
            // "Revert now" from the pending-change bar, from either origin;
            // the same rollback the 30-second timer would run.
            if action == Some("revert") {
                let mut pending = self.network.lock().await;
                if pending.is_none() {
                    return Err("No pending network change".into());
                }
                self.config
                    .rollback_pending_network()
                    .await
                    .map_err(|error| error.to_string())?;
                *pending = None;
                if let Some(origins) = &self.origins {
                    *origins.previous_dashboard.write() = None;
                }
                return Ok(());
            }
            let network_keys = [
                "SERVER_IP",
                "SERVER_PORT",
                "DASHBOARD_IP",
                "DASHBOARD_PORT",
                "ALLOWED_IPS",
            ];
            let changes_network = network_keys.iter().any(|key| object.contains_key(*key));
            if changes_network && action != Some("apply") {
                return Err(
                    "Network settings require Apply and confirmation within 30 seconds".into(),
                );
            }
            if let Some(raw) = object.get("MAX_PACKAGE_MIB") {
                let value =
                    json_number(raw).ok_or("MAX_PACKAGE_MIB must be an integer from 1 to 4096")?;
                if !value.is_finite() || value.fract() != 0.0 || !(1.0..=4096.0).contains(&value) {
                    return Err("MAX_PACKAGE_MIB must be an integer from 1 to 4096".into());
                }
            }
            // Reject instead of silently ignoring or clamping, so the dashboard
            // never reports a save that did not persist the submitted value.
            for (key, minimum, maximum) in [
                (
                    "API_V1_SUBSCRIPTION_DEBOUNCE",
                    1.0,
                    ConfigDocument::MAX_INTERVAL_MS,
                ),
                ("SERVER_PORT", 1.0, 65_535.0),
                ("DASHBOARD_PORT", 1.0, 65_535.0),
            ] {
                let Some(raw) = object.get(key) else {
                    continue;
                };
                let Some(value) = json_number(raw).filter(|value| value.is_finite()) else {
                    return Err(format!("{key} must be a number"));
                };
                if value > maximum {
                    return Err(format!("{key} must not exceed {}", format_number(maximum)));
                }
                if value < minimum {
                    return Err(format!("{key} must be at least {}", format_number(minimum)));
                }
                if matches!(key, "SERVER_PORT" | "DASHBOARD_PORT") && value.fract() != 0.0 {
                    return Err(format!("{key} must be a whole number"));
                }
            }
            // Serialize service reconciliation with watcher delivery, so an older
            // notification cannot re-enable a service after a newer settings save.
            #[cfg(target_os = "linux")]
            let mut login_state = self.login_state.lock().await;
            #[cfg(not(target_os = "linux"))]
            let _login_state = self.login_state.lock().await;
            #[cfg(target_os = "linux")]
            if let Some(enabled) = object
                .get("START_AT_LOGIN")
                .and_then(|value| value.as_bool())
                && enabled != self.config.current().start_at_login
            {
                tokio::task::spawn_blocking(move || crate::desktop::login::reconcile(enabled))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| format!("Start at login: {error}"))?;
                *login_state = enabled;
            }
            let mut pending = self.network.lock().await;
            if changes_network && pending.is_some() {
                return Err("A network change is already pending".into());
            }
            let previous = self.config.current();
            if changes_network {
                let rollback_origin = if self.origins.is_some() {
                    Some(
                        object
                            .get("_request_origin")
                            .and_then(|value| value.as_str())
                            .ok_or("Apply requires the verified dashboard request origin")?
                            .to_owned(),
                    )
                } else {
                    None
                };
                let mut candidate = (*previous).clone();
                apply_config_values(&mut candidate, &object);
                self.config
                    .validate_network(&candidate)
                    .await
                    .map_err(|error| error.to_string())?;
                self.config
                    .stage_network(&candidate)
                    .await
                    .map_err(|error| error.to_string())?;
                if let Some(origins) = &self.origins {
                    *origins.previous_dashboard.write() = rollback_origin;
                }
                let generation = Arc::new(());
                *pending = Some(NetworkChange {
                    generation: Arc::clone(&generation),
                    deadline: tokio::time::Instant::now() + Duration::from_secs(30),
                });
                let pending = Arc::clone(&self.network);
                let store = Arc::clone(&self.config);
                let origins = self.origins.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    loop {
                        let mut pending = pending.lock().await;
                        if !pending
                            .as_ref()
                            .is_some_and(|current| Arc::ptr_eq(&current.generation, &generation))
                        {
                            break;
                        }
                        if store.rollback_pending_network().await.is_ok() {
                            *pending = None;
                            if let Some(origins) = &origins {
                                *origins.previous_dashboard.write() = None;
                            }
                            break;
                        }
                        // Retain both the durable journal and the in-memory guard
                        // until restoration has actually reached disk.
                        drop(pending);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                });
            }
            self.config
                .update(|config| apply_config_values(config, &object));
            if let Err(error) = self.config.flush().await {
                if changes_network {
                    self.config
                        .update(|config| restore_network(config, &previous));
                    if self.config.rollback_pending_network().await.is_ok() {
                        *pending = None;
                        if let Some(origins) = &self.origins {
                            *origins.previous_dashboard.write() = None;
                        }
                    }
                }
                return Err(error.to_string());
            }
            Ok(())
        })
    }

    fn view(&self) -> serde_json::Value {
        settings_view(&self.config.current(), crate::desktop::externally_managed())
    }
}

/// `SettingsView` (A7.1 / M8): exactly the 11 core keys the Settings page
/// renders, by their configuration names. `ENABLE_DASHBOARD` is never
/// included (A7.3); osu! and overlay-root keys live in plugin settings.
pub(super) fn settings_view(
    config: &Config,
    updates_externally_managed: bool,
) -> serde_json::Value {
    serde_json::json!({
        "values": {
            "ENABLE_AUTOUPDATE": config.enable_auto_update,
            "OPEN_DASHBOARD_ON_STARTUP": config.open_dashboard_on_startup,
            "START_AT_LOGIN": config.start_at_login,
            "DEBUG_LOG": config.debug_log,
            "API_V1_SUBSCRIPTION_DEBOUNCE": config.api_v1_subscription_debounce,
            "SERVER_IP": config.server_ip,
            "SERVER_PORT": config.server_port,
            "DASHBOARD_IP": config.dashboard_ip,
            "DASHBOARD_PORT": config.dashboard_port,
            "ALLOWED_IPS": config.allowed_ips,
            "MAX_PACKAGE_MIB": config.max_package_mib,
        },
        "updates_externally_managed": updates_externally_managed,
    })
}

fn restore_network(config: &mut Config, previous: &Config) {
    config.server_ip.clone_from(&previous.server_ip);
    config.server_port = previous.server_port;
    config.dashboard_ip.clone_from(&previous.dashboard_ip);
    config.dashboard_port = previous.dashboard_port;
    config.allowed_ips.clone_from(&previous.allowed_ips);
}

pub(super) fn apply_config_values(
    config: &mut Config,
    values: &serde_json::Map<String, serde_json::Value>,
) {
    macro_rules! boolean {
        ($key:literal, $field:ident) => {
            if let Some(value) = values.get($key).and_then(|v| v.as_bool()) {
                config.$field = value;
            }
        };
    }
    macro_rules! number {
        ($key:literal, $field:ident) => {
            if let Some(value) = values
                .get($key)
                .and_then(json_number)
                .filter(|value| value.is_finite())
            {
                config.$field = value;
            }
        };
    }
    macro_rules! string {
        ($key:literal, $field:ident) => {
            if let Some(value) = values.get($key).and_then(|v| v.as_str()) {
                config.$field = value.to_owned();
            }
        };
    }
    boolean!("ENABLE_AUTOUPDATE", enable_auto_update);
    boolean!("OPEN_DASHBOARD_ON_STARTUP", open_dashboard_on_startup);
    boolean!("START_AT_LOGIN", start_at_login);
    boolean!("DEBUG_LOG", debug_log);
    number!("API_V1_SUBSCRIPTION_DEBOUNCE", api_v1_subscription_debounce);
    number!("SERVER_PORT", server_port);
    number!("DASHBOARD_PORT", dashboard_port);
    number!("MAX_PACKAGE_MIB", max_package_mib);
    string!("DASHBOARD_IP", dashboard_ip);
    string!("SERVER_IP", server_ip);
    string!("ALLOWED_IPS", allowed_ips);
}

/// The update the dashboard advertises. A major release is installed only
/// after the dashboard's explicit confirmation, so the page renders it
/// differently.
#[derive(Clone)]
pub(super) enum UpdateNotice {
    Minor(String),
    Major(crate::updater::MajorRelease),
}

impl UpdateNotice {
    /// `DashboardStatus.update`: `{"version","major","release_url"}`.
    pub(super) fn view(&self) -> serde_json::Value {
        match self {
            Self::Minor(version) => {
                serde_json::json!({"version": version, "major": false, "release_url": null})
            }
            Self::Major(release) => serde_json::json!({
                "version": release.version.to_string(),
                "major": true,
                "release_url": release.release_url,
            }),
        }
    }
}

#[derive(Clone)]
pub(super) struct RuntimeReportInput {
    pub(super) config: Arc<ConfigStore>,
    pub(super) plugin_settings: Arc<PluginSettingsService>,
    pub(super) plugins: Arc<PluginHost>,
    pub(super) adapters: Arc<super::adapters::Adapters>,
    pub(super) overlay: Arc<dyn OverlayComposition>,
    pub(super) logs: PathBuf,
    pub(super) update: Option<UpdateNotice>,
}

impl ReportInputProvider for RuntimeReportInput {
    fn dashboard_status(&self) -> serde_json::Value {
        let (poll_rate, precise_data_poll_rate) = effective_osu_poll_rates(&self.plugin_settings);
        let instances: Vec<_> = self
            .plugins
            .diagnostic_sessions()
            .into_iter()
            .map(|entry| {
                serde_json::json!({
                    "pid": entry.pid,
                    "client": entry.client.to_ascii_lowercase(),
                    "version": self.adapters.services.client_version(&entry.session).unwrap_or(entry.version),
                    "state": entry.state,
                    "pollIntervalMs": poll_rate,
                    "precisePollIntervalMs": precise_data_poll_rate,
                })
            })
            .collect();
        serde_json::json!({
            "instances": instances,
            "available": true,
            "update": self.update.as_ref().map(UpdateNotice::view),
        })
    }
    fn capture(&self) -> Result<ReportInput, String> {
        let instances = self
            .plugins
            .diagnostic_sessions()
            .into_iter()
            .map(|entry| ReportInstance {
                pid: entry.pid,
                client_type: entry.client,
                bitness: match entry.bitness {
                    vosu_plugin_api::Bitness::X86 => "x86",
                    vosu_plugin_api::Bitness::X64 => "x64",
                }
                .into(),
                version: self
                    .adapters
                    .services
                    .client_version(&entry.session)
                    .unwrap_or(entry.version),
            })
            .collect();
        let counters = self.overlay.report_counters()?;
        let mut input = ReportInput::with_config(&self.config.current());
        input.instances = instances;
        input.counters = counters;
        input.logs_dir = Some(self.logs.clone());
        Ok(input)
    }
}

pub(super) struct RuntimeUpdater {
    pub(super) program: PathBuf,
    pub(super) program_dir: PathBuf,
    pub(super) shutdown: CancellationToken,
    pub(super) client: vosu_net::streaming::StreamingClient,
    pub(super) release_api: String,
    pub(super) version: semver::Version,
}
impl RuntimeUpdater {
    pub(super) fn production(
        program: PathBuf,
        program_dir: PathBuf,
        shutdown: CancellationToken,
        version: semver::Version,
    ) -> Self {
        Self {
            program,
            program_dir,
            shutdown,
            client: vosu_net::streaming::StreamingClient::production(),
            release_api: LATEST_RELEASE_API.to_owned(),
            version,
        }
    }
}

/// Feature-gated updater route seam. It carries the exact-listener test policy
/// and reserved release endpoint without exposing a CLI/environment bypass.
/// `version` is the running executable's ([`Bundle::version`]).
#[cfg(feature = "test-harness")]
pub fn test_updater_handoff(
    program: PathBuf,
    program_dir: PathBuf,
    shutdown: CancellationToken,
    network: vosu_net::streaming::TestNetworkConfig,
    version: semver::Version,
) -> io::Result<Arc<dyn UpdaterHandoff>> {
    let release_api = network
        .endpoints()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "test network endpoints missing",
            )
        })?
        .release
        .clone();
    let client = vosu_net::streaming::StreamingClient::for_test_network(
        network,
        vosu_net::streaming::Deadlines::default(),
        vosu_net::streaming::DownloadLimits::default(),
    );
    Ok(Arc::new(RuntimeUpdater {
        program,
        program_dir,
        shutdown,
        client,
        release_api,
        version,
    }))
}

impl UpdaterHandoff for RuntimeUpdater {
    fn run_update(
        &self,
        major_consent: bool,
    ) -> crate::server::routes::ProviderFuture<'_, Result<UpdateRouteResult, String>> {
        Box::pin(async move {
            if !crate::desktop::directory_writable(&self.program_dir) {
                return Err(crate::desktop::MANAGED_UPDATE_ERROR.to_owned());
            }
            let client = self.client.clone();
            let cancel = self.shutdown.child_token();
            let platform_name = if cfg!(windows) { "windows" } else { "linux" };
            match check_latest_and_prepare(
                &self.program_dir,
                &self.program,
                &self.version,
                platform_name,
                &self.release_api,
                &NativePlatform,
                &client,
                &cancel,
                major_consent,
            )
            .await
            .map_err(|error| error.to_string())?
            {
                UpdateCheck::Latest(_)
                | UpdateCheck::Available(_)
                | UpdateCheck::SuppressedForced => Ok(UpdateRouteResult::Latest),
                UpdateCheck::NoFiles => Ok(UpdateRouteResult::NoFiles),
                UpdateCheck::MajorAvailable(release) => Ok(UpdateRouteResult::MajorAvailable {
                    version: release.version.to_string(),
                    release_url: release.release_url,
                }),
                UpdateCheck::Prepared(prepared) => {
                    tokio::task::spawn_blocking(move || prepared.spawn_helper())
                        .await
                        .map_err(|error| error.to_string())?
                        .map_err(|error| error.to_string())?;
                    let shutdown = self.shutdown.clone();
                    Ok(UpdateRouteResult::Updated(
                        crate::server::service::DeferredUpdateTrigger::new(move || {
                            shutdown.cancel();
                        }),
                    ))
                }
            }
        })
    }
}
pub(super) fn startup_role_allows_update_check(readiness: Option<&StartupReadiness>) -> bool {
    readiness.is_none()
}
pub(super) fn listen_target(config: &Config) -> io::Result<ListenTarget> {
    if !config.server_port.is_finite()
        || config.server_port.fract() != 0.0
        || config.server_port < 0.0
        || config.server_port > u16::MAX as f64
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SERVER_PORT must be an integral value in the u16 range",
        ));
    }
    Ok(ListenTarget::new(
        config.server_ip.clone(),
        config.server_port as u16,
    ))
}
pub(super) fn dashboard_listen_target(config: &Config) -> io::Result<ListenTarget> {
    let mut dashboard = config.clone();
    dashboard.server_ip = config.dashboard_ip.clone();
    dashboard.server_port = config.dashboard_port;
    listen_target(&dashboard)
}

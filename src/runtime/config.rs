use super::*;

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
            // Reject instead of silently ignoring or clamping, so the dashboard
            // never reports a save that did not persist the submitted value.
            for (key, minimum, maximum) in [
                ("POLL_RATE", 100.0, ConfigDocument::MAX_INTERVAL_MS),
                (
                    "PRECISE_DATA_POLL_RATE",
                    1.0,
                    ConfigDocument::MAX_INTERVAL_MS,
                ),
                (
                    "V2_SUBSCRIPTION_DEBOUNCE",
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
    boolean!("CALCULATE_PP", calculate_pp);
    boolean!("ENABLE_KEY_OVERLAY", enable_key_overlay);
    boolean!("ENABLE_MANIA_KEY_HIT_ERRORS", enable_mania_key_hit_errors);
    boolean!("SHOW_MP_COMMANDS", show_mp_commands);
    boolean!("READ_MANIA_SCROLL_SPEED", read_mania_scroll_speed);
    number!("POLL_RATE", poll_rate);
    number!("PRECISE_DATA_POLL_RATE", precise_data_poll_rate);
    number!("V2_SUBSCRIPTION_DEBOUNCE", v2_subscription_debounce);
    number!("SERVER_PORT", server_port);
    number!("DASHBOARD_PORT", dashboard_port);
    string!("DASHBOARD_IP", dashboard_ip);
    string!("SERVER_IP", server_ip);
    string!("STATIC_FOLDER_PATH", static_folder_path);
    string!("ALLOWED_IPS", allowed_ips);
}

pub(super) type DashboardLifecycle = Arc<
    arc_swap::ArcSwapOption<
        tokio::sync::watch::Receiver<Arc<crate::instance::InstanceLifecycleSnapshot>>,
    >,
>;

#[derive(Clone)]
pub(super) struct RuntimeReportInput {
    pub(super) config: Arc<ConfigStore>,
    pub(super) snapshots: Arc<SnapshotManager>,
    pub(super) counters: Arc<ReloadableCounterService>,
    pub(super) logs: PathBuf,
    pub(super) lifecycle: DashboardLifecycle,
}

impl ReportInputProvider for RuntimeReportInput {
    fn dashboard_status(&self) -> serde_json::Value {
        let combined = self.snapshots.load();
        let config = self.config.current();
        let lifecycle = self
            .lifecycle
            .load()
            .as_ref()
            .map(|receiver| receiver.borrow().clone());
        let render = |pid, client, entry: Option<&crate::api::ManagerInstanceSnapshot>, state| {
            serde_json::json!({
                "pid": pid,
                "client": format!("{client:?}").to_ascii_lowercase(),
                "version": entry.map(|entry| report_instance_version(&entry.state().regular.settings.values)).unwrap_or_default(),
                "state": state,
                "pollIntervalMs": config.poll_rate,
                "precisePollIntervalMs": config.precise_data_poll_rate
            })
        };
        let instances = if let Some(lifecycle) = lifecycle {
            lifecycle
                .instances
                .iter()
                .map(|detected| {
                    let entry = combined
                        .manager()
                        .instances()
                        .get(&detected.pid)
                        .filter(|entry| entry.state().generation == detected.generation);
                    let state = if entry.is_some() {
                        "reading"
                    } else if detected.offset_ready {
                        "starting reader"
                    } else {
                        "resolving offsets"
                    };
                    render(detected.pid, detected.client, entry, state)
                })
                .collect::<Vec<_>>()
        } else {
            combined
                .manager()
                .instances()
                .values()
                .map(|entry| render(entry.pid(), entry.client(), Some(entry), "reading"))
                .collect()
        };
        serde_json::json!({ "instances": instances, "available": true })
    }
    fn capture(&self) -> Result<ReportInput, String> {
        let combined = self.snapshots.load();
        let manager = combined.manager();
        let instances = manager
            .instances()
            .iter()
            .map(|(pid, entry)| ReportInstance {
                pid: *pid,
                client_type: format!("{:?}", entry.client()),
                bitness: if entry.client() == ClientType::Stable {
                    "x86"
                } else {
                    "x64"
                }
                .into(),
                version: report_instance_version(&entry.state().regular.settings.values),
            })
            .collect();
        let counters = self
            .counters
            .discover()
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|counter| ReportCounter {
                author: counter.author,
                name: counter.name,
                version: counter.version,
                folder_name: counter.folder_name,
            })
            .collect();
        let mut input = ReportInput::with_config(&self.config.current());
        input.instances = instances;
        input.counters = counters;
        input.logs_dir = Some(self.logs.clone());
        Ok(input)
    }
}

pub(super) fn report_instance_version(values: &crate::model::states::SettingsObject) -> String {
    values
        .get("client")
        .and_then(serde_json::Value::as_object)
        .and_then(|client| client.get("version"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub(super) struct RuntimeUpdater {
    pub(super) program: PathBuf,
    pub(super) program_dir: PathBuf,
    pub(super) shutdown: CancellationToken,
    pub(super) client: crate::net::streaming::StreamingClient,
    pub(super) release_api: String,
    pub(super) platform: NativePlatform,
}
impl RuntimeUpdater {
    pub(super) fn production(
        program: PathBuf,
        program_dir: PathBuf,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            program,
            program_dir,
            shutdown,
            client: crate::net::streaming::StreamingClient::production(),
            release_api: LATEST_RELEASE_API.to_owned(),
            platform: NativePlatform,
        }
    }
}

/// Feature-gated updater route seam. It carries the exact-listener test policy
/// and reserved release endpoint without exposing a CLI/environment bypass.
#[cfg(feature = "test-harness")]
pub fn test_updater_handoff(
    program: PathBuf,
    program_dir: PathBuf,
    shutdown: CancellationToken,
    network: crate::net::streaming::TestNetworkConfig,
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
    let client = crate::net::streaming::StreamingClient::for_test_network(
        network,
        crate::net::streaming::Deadlines::default(),
        crate::net::streaming::DownloadLimits::default(),
    );
    Ok(Arc::new(RuntimeUpdater {
        program,
        program_dir,
        shutdown,
        client,
        release_api,
        platform: NativePlatform,
    }))
}

impl UpdaterHandoff for RuntimeUpdater {
    fn run_update(
        &self,
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
                &current_version(),
                platform_name,
                &self.release_api,
                &self.platform,
                &client,
                &cancel,
            )
            .await
            .map_err(|error| error.to_string())?
            {
                UpdateCheck::Latest(_)
                | UpdateCheck::Available(_)
                | UpdateCheck::SuppressedForced => Ok(UpdateRouteResult::Latest),
                UpdateCheck::NoFiles => Ok(UpdateRouteResult::NoFiles),
                UpdateCheck::Prepared(prepared) => {
                    prepared.spawn_helper().map_err(|error| error.to_string())?;
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

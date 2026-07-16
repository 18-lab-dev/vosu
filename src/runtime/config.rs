use super::*;

#[derive(Clone)]
pub(super) struct RuntimeConfig {
    pub(super) config: Arc<ConfigStore>,
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
            self.config
                .update(|config| apply_config_values(config, &object));
            self.config.flush().await.map_err(|error| error.to_string())
        })
    }
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
            if let Some(value) = values.get($key).and_then(|v| v.as_f64()) {
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
    boolean!("DEBUG_LOG", debug_log);
    boolean!("CALCULATE_PP", calculate_pp);
    boolean!("ENABLE_KEY_OVERLAY", enable_key_overlay);
    boolean!("SHOW_MP_COMMANDS", show_mp_commands);
    boolean!("READ_MANIA_SCROLL_SPEED", read_mania_scroll_speed);
    boolean!("ENABLE_INGAME_OVERLAY", enable_ingame_overlay);
    number!("POLL_RATE", poll_rate);
    number!("PRECISE_DATA_POLL_RATE", precise_data_poll_rate);
    number!("SERVER_PORT", server_port);
    number!("INGAME_OVERLAY_MAX_FPS", ingame_overlay_max_fps);
    string!("SERVER_IP", server_ip);
    string!("STATIC_FOLDER_PATH", static_folder_path);
    string!("INGAME_OVERLAY_KEYBIND", ingame_overlay_keybind);
    string!("ALLOWED_IPS", allowed_ips);
}

#[derive(Clone)]
pub(super) struct RuntimeReportInput {
    pub(super) config: Arc<ConfigStore>,
    pub(super) snapshots: Arc<SnapshotManager>,
    pub(super) counters: Arc<ReloadableCounterService>,
    pub(super) logs: PathBuf,
}

impl ReportInputProvider for RuntimeReportInput {
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

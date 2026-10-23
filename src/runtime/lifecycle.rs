use super::*;

/// Process-wide resource owner. Every long-lived production service is retained
/// here and joined in reverse dependency order.
pub struct ProductionRuntime {
    config: Arc<ConfigStore>,
    changes: broadcast::Receiver<ConfigChanged>,
    logger: Logger,
    paths: Paths,
    snapshots: Arc<SnapshotProviders>,
    shutdown_token: CancellationToken,
    signal_task: AsyncMutex<Option<JoinHandle<io::Result<()>>>>,
    startup_readiness: Option<StartupReadiness>,
    authorizer: Arc<ReloadableAuthorizer>,
    update_handed_off: AtomicBool,
    available_update: Option<String>,
    counters: AsyncMutex<Option<Arc<ReloadableCounterService>>>,
    tools: AsyncMutex<Option<Arc<crate::tools::ToolHost>>>,
    dev_tools: Vec<PathBuf>,
    listener: AsyncMutex<Option<ListenerOwner>>,
    dashboard_listener: AsyncMutex<Option<ListenerOwner>>,
    origins: Arc<crate::server::origins::Origins>,
    hubs: AsyncMutex<Option<WebSocketHubs>>,
    processor: AsyncMutex<Option<Arc<CommandProcessor>>>,
    relays: AsyncMutex<Option<Arc<WebSocketRelaySink>>>,
    supervisor: AsyncMutex<Option<ProductionStateSupervisor>>,
    instances: AsyncMutex<Option<InstanceRuntime<NativeProcessProvider, EmbeddedOffsetGenerator>>>,
    ordinary_role: bool,
    tray: AsyncMutex<Option<crate::desktop::tray::TrayHandle>>,
    #[cfg(target_os = "linux")]
    login_state: Arc<AsyncMutex<bool>>,
}

impl ProductionRuntime {
    async fn shutdown(&self) -> io::Result<()> {
        self.shutdown_token.cancel();
        if let Some(tools) = self.tools.lock().await.take() {
            tools.shutdown().await;
        }
        if let Some(mut tray) = self.tray.lock().await.take() {
            tray.shutdown().await;
        }
        let dashboard_listener = self.dashboard_listener.lock().await.take();
        if let Some(listener) = dashboard_listener.as_ref() {
            listener.stop_accepting();
        }
        let listener = self.listener.lock().await.take();
        if let Some(listener) = listener.as_ref() {
            listener.stop_accepting();
        }
        if let Some(hubs) = self.hubs.lock().await.take() {
            hubs.shutdown().await;
        }
        if let Some(listener) = listener {
            listener.join().await;
        }
        if let Some(listener) = dashboard_listener {
            listener.join().await;
        }
        if let Some(processor) = self.processor.lock().await.take() {
            processor.shutdown().await;
        }
        if let Some(relays) = self.relays.lock().await.take() {
            relays.shutdown().await;
        }
        if let Some(supervisor) = self.supervisor.lock().await.take() {
            supervisor.shutdown().await;
        }
        if let Some(instances) = self.instances.lock().await.take() {
            instances.shutdown().await;
        }
        if let Some(counters) = self.counters.lock().await.take() {
            counters.shutdown().await;
        }
        let signal_result = match self.signal_task.lock().await.take() {
            Some(task) => task
                .await
                .map_err(|error| io::Error::other(format!("signal task failed: {error}")))?,
            None => Ok(()),
        };
        let config_result = self.config.shutdown().await;
        let logger_result = self.logger.shutdown().await;
        signal_result.and(config_result).and(logger_result)
    }

    async fn compose(&mut self) -> io::Result<()> {
        if self.listener.get_mut().is_some() {
            return Ok(());
        }
        let config = self.config.current();
        self.config.validate_network(&config).await?;
        let dashboard_target = dashboard_listen_target(&config)?;
        // Resolve every fallible construction input before starting any
        // long-lived task.
        let target = listen_target(&config)?;
        let program = env::current_exe()?;
        let asset = DaemonAssets::get("offsets.json").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "embedded offsets.json is missing")
        })?;
        let generator = EmbeddedOffsetGenerator::new(self.paths.cache().join("offset-generator"));
        let loader = LazerOffsetLoader::new(
            generator,
            self.paths.cache().join("offsets"),
            Bytes::copy_from_slice(&asset.data),
        );
        let static_root = resolved_static_root(&self.paths, &config)?;
        let counters = Arc::new(ReloadableCounterService::production(
            static_root,
            self.paths.config().join("settings"),
            self.paths.cache().join("downloads"),
        ));
        {
            let counters = Arc::clone(&counters);
            let paths = self.paths.clone();
            let logger = self.logger.clone();
            self.config.register_before_publish(move |next| {
                if let Err(error) = reload_counter_static_root(&paths, next, &counters) {
                    logger.warn(format!(
                        "Failed to resolve or create requested static root; retaining previous counter root: {error}"
                    ));
                }
            });
        }
        let beatmap = Arc::new(RuntimeBeatmap {
            snapshots: self.snapshots.snapshot_manager(),
            counters: Arc::clone(&counters),
        });
        let counter_facade = Arc::new(RuntimeCounterFacade {
            counters: Arc::clone(&counters),
        });
        let relays = Arc::new(WebSocketRelaySink::new());
        let processor = Arc::new(CommandProcessor::new(counter_facade, relays.clone()));
        let hubs = WebSocketHubs::start(
            self.snapshots.clone(),
            self.snapshots.clone(),
            duration_from_millis_saturating(config.v2_subscription_debounce),
        );
        let dashboard_lifecycle = Arc::new(arc_swap::ArcSwapOption::empty());
        let services = ProductionServices::with_native_report_probe(
            PageService,
            Arc::new(RuntimePages {
                pages: PageService,
                counters: Arc::clone(&counters),
                catalog: CatalogService::production(&current_version().to_string()),
                config: Arc::clone(&self.config),
                shutdown: self.shutdown_token.clone(),
                update_version: self.available_update.clone(),
            }),
            Arc::clone(&counters),
            beatmap.clone(),
            Arc::new(RuntimeConfig {
                config: Arc::clone(&self.config),
                network: Arc::new(AsyncMutex::new(None)),
                origins: Some(Arc::clone(&self.origins)),
                #[cfg(target_os = "linux")]
                login_state: Arc::clone(&self.login_state),
                #[cfg(not(target_os = "linux"))]
                login_state: Arc::new(AsyncMutex::new(config.start_at_login)),
            }),
            // The receiver is bound once native discovery starts below.
            Arc::new(RuntimeReportInput {
                config: Arc::clone(&self.config),
                snapshots: self.snapshots.snapshot_manager(),
                counters: Arc::clone(&counters),
                logs: self.paths.data().join("logs"),
                lifecycle: Arc::clone(&dashboard_lifecycle),
            }),
            Arc::new(RuntimeUpdater::production(
                program,
                self.paths.program().to_path_buf(),
                self.shutdown_token.clone(),
            )),
            WebSocketEndpoints {
                v1: hubs.v1.clone(),
                sc: hubs.stream_companion.clone(),
                v2: hubs.v2.clone(),
                precise: hubs.v2_precise.clone(),
                commands: hubs.v1.clone(),
                subscriptions: hubs.v2_subscription.clone(),
                processor: processor.clone(),
                relays: relays.clone(),
            },
        );
        let context_beatmap = beatmap.clone();
        let tools = crate::tools::ToolHost::new(
            self.paths.config().join("tools"),
            self.paths.data().join("tools"),
            self.paths.cache().join("tools"),
            crate::tools::ExtraRoots {
                dev: self.dev_tools.clone(),
            },
            self.origins.overlay(),
            self.origins.dashboard(),
            Arc::new(move || context_beatmap.tool_context()),
        )
        .await?;
        tools.set_counters(Arc::clone(&counters));
        *self.tools.get_mut() = Some(tools.clone());
        let providers = RouteProviders {
            authorization: Arc::new(crate::server::origins::OriginAuthorizer {
                inner: self.authorizer.clone(),
                origins: Arc::clone(&self.origins),
                dashboard: false,
            }),
            state: Arc::new(SnapshotStateProvider::new(
                self.snapshots.snapshot_manager(),
            )),
            calculation: Arc::new(LegacyCalculateProvider::new(beatmap)),
            delegate: Arc::new(
                ProductionRouteDelegate::new(services)
                    .with_api_keys(Arc::new(crate::server::api_keys::ApiKeys::load(
                        self.paths.config().join("api-keys.toml"),
                        Some(self.logger.clone()),
                    )))
                    .with_tools(tools.clone()),
            ),
        };
        let mut dashboard_providers = providers.clone();
        dashboard_providers.authorization = Arc::new(crate::server::origins::OriginAuthorizer {
            inner: self.authorizer.clone(),
            origins: Arc::clone(&self.origins),
            dashboard: true,
        });
        let listener = match ListenerOwner::start_with_broker(
            providers,
            target,
            self.paths.broker_path().map(Some),
            Some(self.logger.clone()),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) => {
                hubs.shutdown().await;
                processor.shutdown().await;
                relays.shutdown().await;
                counters.shutdown().await;
                return Err(error);
            }
        };
        let Some(effective_listener) = listener.state().effective else {
            listener.stop_accepting();
            hubs.shutdown().await;
            listener.join().await;
            processor.shutdown().await;
            relays.shutdown().await;
            counters.shutdown().await;
            return Err(io::Error::other(
                "listener start returned without effective address",
            ));
        };
        self.logger.info(format!(
            "Overlays and API: {}",
            crate::desktop::dashboard_url(
                &effective_listener.target.host,
                effective_listener.address.port()
            )
        ));
        let dashboard_listener =
            match ListenerOwner::start(dashboard_providers, dashboard_target).await {
                Ok(listener) => listener,
                Err(error) => {
                    listener.stop_accepting();
                    hubs.shutdown().await;
                    listener.join().await;
                    processor.shutdown().await;
                    relays.shutdown().await;
                    counters.shutdown().await;
                    return Err(error);
                }
            };
        let dashboard_effective = dashboard_listener
            .state()
            .effective
            .expect("started dashboard listener has an effective address");
        self.origins
            .observe_listeners(listener.state.clone(), dashboard_listener.state.clone());
        tools.activate(Some(self.origins.clone()));
        self.logger.info(format!(
            "Dashboard: {}",
            crate::desktop::dashboard_url(
                &dashboard_effective.target.host,
                dashboard_effective.address.port()
            )
        ));
        *self.dashboard_listener.get_mut() = Some(dashboard_listener);
        // Bind the existing discovery stream; do not maintain a second roster.
        let process_provider = NativeProcessProvider::new();
        self.logger.info(format!(
            "Process events backend: {}",
            process_provider.process_event_backend_name()
        ));
        let instances =
            InstanceRuntime::start_with_logger(process_provider, loader, self.logger.clone());
        dashboard_lifecycle.store(Some(Arc::new(instances.subscribe_current())));
        let supervisor = ProductionStateSupervisor::start(
            instances.subscribe_current(),
            self.config.subscribe_current(),
            hubs.v2_subscription.demand(),
            self.snapshots.clone(),
            self.logger.clone(),
        );
        self.snapshots.set_poll_rates(
            poll_duration(config.poll_rate),
            poll_duration(config.precise_data_poll_rate),
        );
        *self.counters.get_mut() = Some(counters);
        *self.listener.get_mut() = Some(listener);
        *self.hubs.get_mut() = Some(hubs);
        *self.processor.get_mut() = Some(processor);
        *self.relays.get_mut() = Some(relays);
        *self.supervisor.get_mut() = Some(supervisor);
        *self.instances.get_mut() = Some(instances);
        if self.ordinary_role {
            let origins = Arc::clone(&self.origins);
            *self.tray.get_mut() = Some(crate::desktop::tray::start(
                Arc::new(move || format!("{}/", origins.dashboard())),
                self.shutdown_token.clone(),
            ));
        }
        if self.ordinary_role && config.open_dashboard_on_startup {
            let url = format!("{}/", self.origins.dashboard());
            if let Err(error) = tokio::task::spawn_blocking(move || open::that_detached(url))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()))
            {
                self.logger
                    .warn(format!("Could not open dashboard: {error}"));
            }
        }
        // Publish readiness only after every owned task is reachable through
        // `self`, so any signal/cleanup failure is followed by the normal
        // guaranteed shutdown path rather than stranding local resources.
        if let Some(readiness) = self.startup_readiness.take() {
            readiness
                .signal_ready()
                .map_err(|error| io::Error::other(error.to_string()))?;
            readiness
                .finish_cleanup(&NativePlatform)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        self.logger.info("Searching for %osu!% process...");
        Ok(())
    }
}

impl RuntimeHooks for ProductionRuntime {
    fn warn<'a>(&'a mut self, message: &'a str) -> HookFuture<'a, ()> {
        Box::pin(async move {
            self.logger.warn(message);
            Ok(())
        })
    }

    fn apply_update_decision(&mut self, enabled: bool) -> HookFuture<'_, ()> {
        Box::pin(async move {
            // Candidate and rollback processes are launched by the update helper
            // while it owns the live install record. They may bind and report
            // readiness, but must never start a second update transaction.
            if !startup_role_allows_update_check(self.startup_readiness.as_ref()) {
                return Ok(());
            }
            if !crate::desktop::directory_writable(self.paths.program()) {
                self.logger
                    .info("Updates are managed externally; update vosu with your package manager");
                return Ok(());
            }
            let client = crate::net::streaming::StreamingClient::production();
            let cancel = self.shutdown_token.child_token();
            let executable = env::current_exe()?;
            let check = check_startup_and_prepare(
                enabled,
                self.paths.program(),
                &executable,
                &current_version(),
                RELEASE_PLATFORM,
                LATEST_RELEASE_API,
                &NativePlatform,
                &client,
                &cancel,
            )
            .await;
            let check = match check {
                Ok(check) => check,
                Err(
                    crate::updater::UpdateError::AlreadyRunning
                    | crate::updater::UpdateError::LiveOwner,
                ) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ResourceBusy,
                        StartupOwnerConflict,
                    ));
                }
                Err(error) => {
                    self.logger.warn(format!(
                        "Startup update check failed; continuing without update: {error}"
                    ));
                    return Ok(());
                }
            };
            match check {
                UpdateCheck::Prepared(prepared) => match prepared.spawn_helper() {
                    Ok(_) => self.update_handed_off.store(true, Ordering::Release),
                    Err(error) => self.logger.warn(format!(
                        "Failed to hand off startup update; continuing ordinary startup: {error}"
                    )),
                },
                UpdateCheck::Available(version) => {
                    self.logger.warn(format!(
                        "Update available: v{} => v{version}",
                        current_version()
                    ));
                    self.available_update = Some(version.to_string());
                }
                UpdateCheck::Latest(version) => {
                    self.logger
                        .info(format!("vosu {version} is already latest"));
                }
                UpdateCheck::NoFiles => self.logger.warn("No matching update asset was found"),
                UpdateCheck::SuppressedForced => self
                    .logger
                    .info("Startup update suppressed for forced build"),
            }
            Ok(())
        })
    }

    fn start_runtime(&mut self) -> HookFuture<'_, ()> {
        Box::pin(async move {
            if self.update_handed_off.load(Ordering::Acquire) {
                Ok(())
            } else {
                self.config.claim_network_ownership().await?;
                let current = self.config.current();
                self.authorizer
                    .reload(&current.allowed_ips, &current.server_ip);
                *self.origins.overlay.write() = crate::desktop::dashboard_url(
                    &current.server_ip,
                    listen_target(&current)?.port,
                )
                .trim_end_matches('/')
                .to_owned();
                *self.origins.dashboard.write() = crate::desktop::dashboard_url(
                    &current.dashboard_ip,
                    dashboard_listen_target(&current)?.port,
                )
                .trim_end_matches('/')
                .to_owned();
                self.compose().await
            }
        })
    }

    fn next_config_change(&mut self) -> HookFuture<'_, Option<ConfigChanged>> {
        Box::pin(async move {
            if self.update_handed_off.load(Ordering::Acquire) {
                return Ok(None);
            }
            loop {
                tokio::select! {
                    result = self.changes.recv() => match result {
                        Ok(change) => {
                            // Broadcast delivery is asynchronous: reconcile queued
                            // non-root effects against the authoritative snapshot.
                            // Static-root cutover already happened synchronously in
                            // ConfigStore's before-publication barrier.
                            let effective = self.config.current();
                            #[cfg(target_os = "linux")]
                            if self.ordinary_role && change.old.start_at_login != change.new.start_at_login {
                                let mut login_state = self.login_state.lock().await;
                                let enabled = self.config.current().start_at_login;
                                if enabled != *login_state {
                                    match tokio::task::spawn_blocking(move || crate::desktop::login::reconcile(enabled)).await {
                                        Ok(Ok(())) => *login_state = enabled,
                                        Ok(Err(error)) => self.logger.warn(format!("Start at login could not be reconciled: {error}")),
                                        Err(error) => self.logger.warn(format!("Start at login task failed: {error}")),
                                    }
                                }
                            }
                            self.logger.update_debug(effective.debug_log);
                            self.authorizer.reload(&effective.allowed_ips, &effective.server_ip);
                            self.snapshots.set_poll_rates(
                                poll_duration(effective.poll_rate),
                                poll_duration(effective.precise_data_poll_rate),
                            );
                            if let Some(hubs) = self.hubs.lock().await.as_ref() {
                                hubs.v2_subscription.set_debounce(
                                    duration_from_millis_saturating(
                                        effective.v2_subscription_debounce,
                                    ),
                                );
                            }
                            match listen_target(&effective) {
                                Ok(target) => {
                                    let mut listener = self.listener.lock().await;
                                    if let Some(listener) = listener.as_mut() {
                                        let previous_address = listener
                                            .state()
                                            .effective
                                            .map(|effective| effective.address);
                                        match listener.stage(target).await {
                                            Ok(()) => {
                                                let effective_address = listener
                                                    .state()
                                                    .effective
                                                    .map(|effective| effective.address);
                                                if effective_address != previous_address
                                                    && let Some(address) = effective_address
                                                {
                                                    self.logger.info(format!(
                                                        "Overlays and API: {}", crate::desktop::dashboard_url(&effective.server_ip, address.port())
                                                    ));
                                                }
                                            }
                                            Err(error) => self.logger.warn(format!(
                                                "Failed to bind requested listener; retaining previous listener: {error}"
                                            )),
                                        }
                                    }
                                }
                                Err(error) => self.logger.warn(format!(
                                    "Invalid requested listener; retaining previous listener: {error}"
                                )),
                            }
                            match dashboard_listen_target(&effective) {
                                Ok(target) => {
                                    let mut listener = self.dashboard_listener.lock().await;
                                    if let Some(listener) = listener.as_mut() {
                                        let previous_address = listener.state().effective.map(|effective| effective.address);
                                        match listener.stage(target).await {
                                            Ok(()) => {
                                                if let Some(bound) = listener.state().effective
                                                    && Some(bound.address) != previous_address
                                                {
                                                    self.logger.info(format!("Dashboard: {}", crate::desktop::dashboard_url(&bound.target.host, bound.address.port())));
                                                }
                                            },
                                            Err(error) => self.logger.warn(format!("Failed to bind dashboard; retaining previous listener: {error}")),
                                        }
                                    }
                                }
                                Err(error) => self.logger.warn(format!("Invalid dashboard listener: {error}")),
                            }
                            return Ok(Some(ConfigChanged {
                                old: change.old,
                                new: effective,
                            }));
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return Ok(None),
                    },
                    _ = self.shutdown_token.cancelled() => return Ok(None),
                }
            }
        })
    }
}

pub(super) async fn cancel_on_signal(
    shutdown: CancellationToken,
    signal: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let result = tokio::select! {
        () = shutdown.cancelled() => Ok(()),
        result = signal => result,
    };
    shutdown.cancel();
    result
}

pub(super) async fn run_signal_cancellation(shutdown: CancellationToken) -> io::Result<()> {
    cancel_on_signal(shutdown, shutdown_signal()).await
}

#[cfg(unix)]
pub(super) async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        received = interrupt.recv() => {
            received.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SIGINT stream closed"))
        }
        received = terminate.recv() => {
            received.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SIGTERM stream closed"))
        }
    }
}

#[cfg(windows)]
pub(super) async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::windows::ctrl_break;

    let mut break_signal = ctrl_break()?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        received = break_signal.recv() => {
            received.ok_or_else(|| io::Error::new(
                io::ErrorKind::BrokenPipe,
                "CTRL_BREAK stream closed",
            ))
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) async fn shutdown_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
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

pub(super) fn resolved_static_root(paths: &Paths, config: &Config) -> io::Result<PathBuf> {
    paths.static_path(&config.static_folder_path)
}

pub(super) fn reload_counter_static_root(
    paths: &Paths,
    config: &Config,
    counters: &ReloadableCounterService,
) -> io::Result<()> {
    let static_root = resolved_static_root(paths, config)?;
    counters.reload_static_root(static_root);
    Ok(())
}
#[derive(Debug)]
pub(super) struct StartupOwnerConflict;

impl std::fmt::Display for StartupOwnerConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("another updater owner is still alive")
    }
}

impl std::error::Error for StartupOwnerConflict {}

/// Identifies the typed startup ownership conflict mapped by the binary to the
/// updater contract's dedicated exit status 75.
pub fn is_startup_owner_conflict(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.downcast_ref::<StartupOwnerConflict>().is_some())
}

pub(super) fn recovery_error_is_nonfatal(error: &crate::updater::UpdateError) -> bool {
    matches!(error, crate::updater::UpdateError::Quarantined(_))
}

pub(super) async fn finish_runtime_run(
    run_result: io::Result<StartupOutcome>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let shutdown_result = shutdown.await;
    match (run_result, shutdown_result) {
        (Err(run_error), _) => Err(run_error),
        (Ok(_), Err(shutdown_error)) => Err(shutdown_error),
        (Ok(_), Ok(())) => Ok(()),
    }
}

/// Runs the daemon with the real process environment and command line.
pub async fn run_cli() -> io::Result<()> {
    let raw_args: Vec<OsString> = env::args_os().collect();
    match super::cli::classify_command_invocation(&raw_args) {
        super::cli::CommandInvocation::RunDaemon => {}
        super::cli::CommandInvocation::Command(command) => {
            return super::cli::run_command(command).await;
        }
        super::cli::CommandInvocation::InvalidInput => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "vosu command flags must be standalone arguments without values",
            ));
        }
    }
    let args = StartupArgs::parse(raw_args.clone());
    let environment = EnvironmentSnapshot::capture();
    let resolver = SnapshotPathResolver {
        executable: env::current_exe()?,
        launch: env::current_dir()?,
        environment: &environment,
    };
    let paths = Paths::resolve(&resolver)?;
    let role = dispatch_startup_role(raw_args, &NativePlatform)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let ordinary_role = matches!(role, StartupDispatch::Ordinary { .. });
    let startup_readiness = match role {
        StartupDispatch::HelperCompleted => return Ok(()),
        StartupDispatch::Ordinary { .. } => None,
        StartupDispatch::Candidate(readiness) | StartupDispatch::Rollback(readiness) => {
            Some(readiness)
        }
    };
    let config = ConfigStore::initialize(paths.config().join("vosu.toml")).await?;
    if ordinary_role
        && let Ok(target) = dashboard_listen_target(&config.current())
        && let Some(url) = crate::desktop::existing_dashboard(&target.host, target.port).await
    {
        println!("vosu is already running; opening {url}");
        if let Err(error) = open::that_detached(&url) {
            eprintln!("Could not open dashboard: {error}");
        }
        config.shutdown().await?;
        return Ok(());
    }
    let mut recovery_warning = None;
    if ordinary_role
        && crate::desktop::directory_writable(paths.program())
        && matches!(
            StartupDecision::evaluate(
                &args,
                &config.current(),
                &environment,
                StartupPlatform::CURRENT,
                paths.program(),
            ),
            StartupDecision::Run { .. }
        )
    {
        match recover_startup_update(paths.program(), &NativePlatform) {
            Ok(Some(RecoveryAction::ResumeCandidate | RecoveryAction::RestoreRollback)) => {
                config.shutdown().await?;
                return Ok(());
            }
            Ok(_) => {}
            Err(
                crate::updater::UpdateError::AlreadyRunning
                | crate::updater::UpdateError::LiveOwner,
            ) => {
                config.shutdown().await?;
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    StartupOwnerConflict,
                ));
            }
            Err(error) if recovery_error_is_nonfatal(&error) => {
                recovery_warning = Some(error.to_string());
            }
            Err(error) => {
                config.shutdown().await?;
                return Err(io::Error::other(error.to_string()));
            }
        }
    }
    let logger = Logger::new(
        paths.data().join("logs"),
        LoggerOptions {
            debug_log: config.current().debug_log,
            ..LoggerOptions::default()
        },
        Arc::new(StdoutConsole),
    )?;
    if let Some(warning) = recovery_warning {
        logger.warn(format!(
            "Update recovery was quarantined; continuing for diagnostics: {warning}"
        ));
    }
    logger.info("Starting %vosu%");
    #[cfg(target_os = "linux")]
    if ordinary_role
        && let Some(warning) = crate::platform::cap_sys_ptrace_warning(&resolver.executable)
    {
        logger.warn(warning);
    }
    for warning in config.warnings() {
        logger.warn(warning.clone());
    }

    #[cfg(target_os = "linux")]
    if ordinary_role {
        let enabled = config.current().start_at_login;
        match tokio::task::spawn_blocking(move || crate::desktop::login::reconcile(enabled)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                logger.warn(format!("Start at login could not be reconciled: {error}"))
            }
            Err(error) => logger.warn(format!("Start at login task failed: {error}")),
        }
    }
    let current = config.current();
    let snapshots = Arc::new(SnapshotProviders::new(
        Arc::new(ManagerSnapshot::not_ready(
            0,
            ApiRuntimeSettings {
                enable_key_overlay: current.enable_key_overlay,
                enable_mania_key_hit_errors: current.enable_mania_key_hit_errors,
                enable_hit_offsets: current.enable_hit_offsets,
            },
        )),
        Arc::new(ProductionSnapshotEncoder),
    ));
    let changes = config.subscribe();
    let shutdown_token = CancellationToken::new();
    let signal_task = tokio::spawn(run_signal_cancellation(shutdown_token.clone()));
    let hooks = ProductionRuntime {
        config: Arc::new(config),
        changes,
        logger,
        paths: paths.clone(),
        snapshots,
        shutdown_token,
        signal_task: AsyncMutex::new(Some(signal_task)),
        startup_readiness,
        authorizer: Arc::new(ReloadableAuthorizer::new(
            &current.allowed_ips,
            &current.server_ip,
        )),
        update_handed_off: AtomicBool::new(false),
        available_update: None,
        counters: AsyncMutex::new(None),
        tools: AsyncMutex::new(None),
        dev_tools: crate::tools::development_roots(args.raw())?,
        listener: AsyncMutex::new(None),
        dashboard_listener: AsyncMutex::new(None),
        origins: Arc::new(crate::server::origins::Origins::new(
            crate::desktop::dashboard_url(&current.server_ip, listen_target(&current)?.port),
            crate::desktop::dashboard_url(
                &current.dashboard_ip,
                dashboard_listen_target(&current)?.port,
            ),
        )?),
        hubs: AsyncMutex::new(None),
        processor: AsyncMutex::new(None),
        supervisor: AsyncMutex::new(None),
        relays: AsyncMutex::new(None),
        instances: AsyncMutex::new(None),
        ordinary_role,
        tray: AsyncMutex::new(None),
        #[cfg(target_os = "linux")]
        login_state: Arc::new(AsyncMutex::new(current.start_at_login)),
    };
    let mut app = App::new(
        args,
        environment,
        paths.program().to_path_buf(),
        StartupPlatform::CURRENT,
        current,
        hooks,
    );

    let run_result = app.run().await;
    if matches!(run_result, Ok(StartupOutcome::Started)) {
        app.hooks().logger.info("Shutting down %vosu%");
    }
    finish_runtime_run(run_result, app.hooks().shutdown()).await
}

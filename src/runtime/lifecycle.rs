use super::*;

#[path = "api_v1.rs"]
pub(super) mod api_v1;

/// Process-wide resource owner. Every long-lived production service is retained
/// here and joined in reverse dependency order.
pub struct ProductionRuntime {
    config: Arc<ConfigStore>,
    changes: broadcast::Receiver<ConfigChanged>,
    logger: Logger,
    paths: Paths,
    shutdown_token: CancellationToken,
    signal_task: AsyncMutex<Option<JoinHandle<io::Result<()>>>>,
    startup_readiness: Option<StartupReadiness>,
    authorizer: Arc<ReloadableAuthorizer>,
    update_handed_off: AtomicBool,
    update_notice: Option<UpdateNotice>,
    counters: AsyncMutex<Option<Arc<ReloadableCounterService>>>,
    tools: AsyncMutex<Option<Arc<crate::tools::ToolHost>>>,
    dev_tools: Vec<PathBuf>,
    dev_plugins: Vec<PathBuf>,
    plugins: Arc<PluginHost>,
    api_v1: api_v1::ApiV1Service,
    state_host: AsyncMutex<Option<vosu_surface::state::StateHostInstalled>>,
    /// Bridge B2's `ConfigStore` watch task (§6.9): has no other way to
    /// stop, since `ConfigStore::shutdown` never drops the `changed`
    /// sender this task's `RecvError::Closed` path waits on. Aborted
    /// explicitly in `shutdown` instead of being left to outlive the
    /// Tokio runtime.
    plugin_settings_watch: AsyncMutex<Option<JoinHandle<()>>>,
    listener: AsyncMutex<Option<ListenerOwner>>,
    dashboard_listener: AsyncMutex<Option<ListenerOwner>>,
    origins: Arc<crate::server::origins::Origins>,
    hubs: AsyncMutex<Option<WebSocketHubs>>,
    adapters: AsyncMutex<Option<Arc<adapters::Adapters>>>,
    processor: AsyncMutex<Option<Arc<CommandProcessor>>>,
    relays: AsyncMutex<Option<Arc<WebSocketRelaySink>>>,
    ordinary_role: bool,
    tray: AsyncMutex<Option<vosu_core::desktop::tray::TrayHandle>>,
    discovery: AsyncMutex<Option<vosu_core::plugin::DiscoveryService>>,
    #[cfg(windows)]
    foreground: AsyncMutex<Option<vosu_core::plugin::ForegroundPoller>>,
    #[cfg(target_os = "linux")]
    login_state: Arc<AsyncMutex<bool>>,
}

impl ProductionRuntime {
    async fn shutdown(&self) -> io::Result<()> {
        let plugins = Arc::clone(&self.plugins);
        self.shutdown_with_plugin_destroy(async move {
            tokio::task::spawn_blocking(move || {
                plugins.destroy_all(dev_plugins::PLUGIN_DESTROY_GRACE)
            })
            .await
            .map_err(|error| io::Error::other(format!("plugin shutdown task failed: {error}")))
        })
        .await
    }

    async fn shutdown_with_plugin_destroy(
        &self,
        destroy: impl Future<Output = io::Result<()>>,
    ) -> io::Result<()> {
        self.shutdown_token.cancel();
        self.api_v1.shutdown();
        if let Some(watch) = self.plugin_settings_watch.lock().await.take() {
            watch.abort();
        }
        if let Some(tools) = self.tools.lock().await.take() {
            tools.shutdown().await;
        }
        // M1 §4 phase d2b unit d: discovery (and, on Windows, the
        // foreground poller) must stop before `destroy_all` — both call
        // back into `self.plugins` (`process_appeared`/`process_exited`/
        // `foreground_changed`), which must not race a plugin instance
        // tearing down under it.
        if let Some(discovery) = self.discovery.lock().await.take() {
            tokio::task::spawn_blocking(move || discovery.stop())
                .await
                .map_err(|error| {
                    io::Error::other(format!("discovery shutdown task failed: {error}"))
                })?;
        }
        #[cfg(windows)]
        if let Some(foreground) = self.foreground.lock().await.take() {
            tokio::task::spawn_blocking(move || foreground.stop())
                .await
                .map_err(|error| {
                    io::Error::other(format!("foreground poller shutdown task failed: {error}"))
                })?;
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
        if let Some(adapters) = self.adapters.lock().await.take() {
            adapters.shutdown().await?;
        }
        destroy.await?;
        self.state_host.lock().await.take();
        if let Some(processor) = self.processor.lock().await.take() {
            processor.shutdown().await;
        }
        if let Some(relays) = self.relays.lock().await.take() {
            relays.shutdown().await;
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
        let host = Arc::clone(&self.plugins);
        let paths = self.paths.clone();
        let logger = self.logger.clone();
        let dev_dirs = std::mem::take(&mut self.dev_plugins);
        tokio::task::spawn_blocking(move || {
            dev_plugins::load_all(&host, dev_dirs, &paths, &logger)
        })
        .await
        .map_err(io::Error::other)
        .and_then(|result| result)
        .inspect_err(|error| self.logger.error(format!("Plugin startup failed: {error}")))?;
        let config = self.config.current();
        self.config.validate_network(&config).await?;
        let dashboard_target = dashboard_listen_target(&config)?;
        // Resolve every fallible construction input before starting any
        // long-lived task.
        let target = listen_target(&config)?;
        let program = env::current_exe()?;
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
            counters: Arc::clone(&counters),
        });
        let counter_facade = Arc::new(RuntimeCounterFacade {
            counters: Arc::clone(&counters),
        });
        let api_keys_path = self.paths.config().join("api-keys.toml");
        let logger = Some(self.logger.clone());
        // `ApiKeys::load` now retries a transient missing read of
        // `api-keys.toml` (Windows, bounded ~50ms) through
        // `vosu_sys::platform::read_published`; run it in
        // `spawn_blocking` rather than on this async worker.
        let api_keys = tokio::task::spawn_blocking(move || {
            crate::server::api_keys::ApiKeys::load(api_keys_path, logger)
        })
        .await
        .map_err(io::Error::other)?;
        let (adapters, hubs) =
            adapters::Adapters::start(&self.plugins, Arc::clone(&self.config)).await?;
        *self.adapters.get_mut() = Some(adapters.clone());
        let context_files = adapters.files.clone();
        let context_host = self.plugins.clone();
        let tools = crate::tools::ToolHost::new(
            self.paths.config().join("tools"),
            self.paths.data().join("tools"),
            self.paths.cache().join("tools"),
            crate::tools::ExtraRoots {
                dev: self.dev_tools.clone(),
            },
            self.origins.overlay(),
            self.origins.dashboard(),
            Arc::new(move || tool_context::tool_context(&context_files, &context_host)),
        )
        .await;
        let tools = match tools {
            Ok(tools) => tools,
            Err(error) => {
                hubs.shutdown().await;
                return Err(error);
            }
        };
        tools.set_counters(Arc::clone(&counters));
        *self.tools.get_mut() = Some(tools.clone());
        let relays = Arc::new(WebSocketRelaySink::new());
        let processor = Arc::new(CommandProcessor::new(counter_facade, relays.clone()));
        let services = ProductionServices::with_native_report_probe(
            PageService,
            Arc::new(RuntimePages {
                pages: PageService,
                counters: Arc::clone(&counters),
                catalog: CatalogService::production(&current_version().to_string()),
                config: Arc::clone(&self.config),
                shutdown: self.shutdown_token.clone(),
                update: self.update_notice.clone(),
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
            Arc::new(RuntimeReportInput {
                config: Arc::clone(&self.config),
                plugins: Arc::clone(&self.plugins),
                counters: Arc::clone(&counters),
                files: adapters.files.clone(),
                logs: self.paths.data().join("logs"),
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
                processor: processor.clone(),
                relays: relays.clone(),
            },
        );
        let providers = RouteProviders {
            authorization: Arc::new(crate::server::origins::OriginAuthorizer {
                inner: self.authorizer.clone(),
                origins: Arc::clone(&self.origins),
                dashboard: false,
            }),
            state: adapters.clone(),
            calculation: Arc::new(DeclaredCalculateProvider(adapters.files.clone())),
            delegate: Arc::new(
                ProductionRouteDelegate::new(services)
                    .with_surface(adapters.router(self.plugins.clone()))
                    .with_api_keys(Arc::new(api_keys))
                    .with_tools(tools.clone())
                    .with_api_v1(self.api_v1.action_router(&self.plugins, tools.clone())),
            ),
        };
        let mut dashboard_providers = providers.clone();
        dashboard_providers.authorization = Arc::new(crate::server::origins::OriginAuthorizer {
            inner: self.authorizer.clone(),
            origins: Arc::clone(&self.origins),
            dashboard: true,
        });
        let mut listener = match ListenerOwner::start_with_broker(
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
        listener.watch_cli_actions(&self.plugins).await;
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
            vosu_core::desktop::dashboard_url(
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
            vosu_core::desktop::dashboard_url(
                &dashboard_effective.target.host,
                dashboard_effective.address.port()
            )
        ));
        *self.dashboard_listener.get_mut() = Some(dashboard_listener);
        *self.counters.get_mut() = Some(counters);
        *self.listener.get_mut() = Some(listener);
        *self.hubs.get_mut() = Some(hubs);
        *self.processor.get_mut() = Some(processor);
        *self.relays.get_mut() = Some(relays);
        let discovery = discovery::start_discovery(&self.plugins, Handle::current(), &self.logger)
            .ok_or_else(|| {
                let message = "required osu! discovery did not start";
                self.logger.error(message);
                io::Error::other(message)
            })?;
        #[cfg(windows)]
        let foreground =
            vosu_core::plugin::ForegroundPoller::start(&self.plugins, Handle::current());
        *self.discovery.get_mut() = Some(discovery);
        #[cfg(windows)]
        {
            *self.foreground.get_mut() = Some(foreground);
        }
        if self.ordinary_role {
            let origins = Arc::clone(&self.origins);
            *self.tray.get_mut() = Some(vosu_core::desktop::tray::start(
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
            // Cleanup waits for the helper to exit and retries scanner locks for
            // up to ten seconds each; keep that off the async workers.
            tokio::task::spawn_blocking(move || {
                readiness.signal_ready()?;
                readiness.finish_cleanup(&NativePlatform)
            })
            .await
            .map_err(io::Error::other)?
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
            if !vosu_core::desktop::directory_writable(self.paths.program()) {
                self.logger
                    .info("Updates are managed externally; update vosu with your package manager");
                return Ok(());
            }
            let client = vosu_net::streaming::StreamingClient::production();
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
                    vosu_core::updater::UpdateError::AlreadyRunning
                    | vosu_core::updater::UpdateError::LiveOwner,
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
                UpdateCheck::Prepared(prepared) => {
                    let handoff = tokio::task::spawn_blocking(move || prepared.spawn_helper())
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result.map_err(|error| error.to_string()));
                    match handoff {
                        Ok(_) => self.update_handed_off.store(true, Ordering::Release),
                        Err(error) => self.logger.warn(format!(
                            "Failed to hand off startup update; continuing ordinary startup: {error}"
                        )),
                    }
                }
                UpdateCheck::Available(version) => {
                    self.logger.warn(format!(
                        "Update available: v{} => v{version}",
                        current_version()
                    ));
                    self.update_notice = Some(UpdateNotice::Minor(version.to_string()));
                }
                UpdateCheck::MajorAvailable(release) => {
                    self.logger.warn(format!(
                        "Major update available: v{} => v{} (requires dashboard confirmation): {}",
                        current_version(),
                        release.version,
                        release.release_url
                    ));
                    self.update_notice = Some(UpdateNotice::Major(release));
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
                *self.origins.overlay.write() = vosu_core::desktop::dashboard_url(
                    &current.server_ip,
                    listen_target(&current)?.port,
                )
                .trim_end_matches('/')
                .to_owned();
                *self.origins.dashboard.write() = vosu_core::desktop::dashboard_url(
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
                                    match tokio::task::spawn_blocking(move || vosu_core::desktop::login::reconcile(enabled)).await {
                                        Ok(Ok(())) => *login_state = enabled,
                                        Ok(Err(error)) => self.logger.warn(format!("Start at login could not be reconciled: {error}")),
                                        Err(error) => self.logger.warn(format!("Start at login task failed: {error}")),
                                    }
                                }
                            }
                            self.logger.update_debug(effective.debug_log);
                            self.api_v1.reload(&effective);
                            self.authorizer.reload(&effective.allowed_ips, &effective.server_ip);
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
                                                        "Overlays and API: {}", vosu_core::desktop::dashboard_url(&effective.server_ip, address.port())
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
                                                    self.logger.info(format!("Dashboard: {}", vosu_core::desktop::dashboard_url(&bound.target.host, bound.address.port())));
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

pub(super) fn recovery_error_is_nonfatal(error: &vosu_core::updater::UpdateError) -> bool {
    matches!(error, vosu_core::updater::UpdateError::Quarantined(_))
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
    let startup_flags = StartupFlags {
        update: args.get("update").and_then(StartupValue::as_boolean),
        onedrive_bypass: args.get("onedrive").and_then(StartupValue::as_boolean) == Some(true),
    };
    let environment = EnvironmentSnapshot::capture();
    let resolver = SnapshotPathResolver {
        executable: env::current_exe()?,
        launch: env::current_dir()?,
        environment: &environment,
    };
    let channel = vosu_sys::platform::BuildChannel::from_version(env!("CARGO_PKG_VERSION"))
        .map_err(io::Error::other)?;
    let paths = Paths::resolve(&resolver, channel)?;
    paths.initialize()?;
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
    let config = Arc::new(super::config::initialize_startup_config(&paths, channel).await?);
    if ordinary_role
        && let Ok(target) = dashboard_listen_target(&config.current())
        && let Some(url) = vosu_core::desktop::existing_dashboard(&target.host, target.port).await
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
        && vosu_core::desktop::directory_writable(paths.program())
        && matches!(
            StartupDecision::evaluate(
                &startup_flags,
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
                vosu_core::updater::UpdateError::AlreadyRunning
                | vosu_core::updater::UpdateError::LiveOwner,
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
    super::tracing_log::install(logger.clone());
    if let Some(warning) = recovery_warning {
        logger.warn(format!(
            "Update recovery was quarantined; continuing for diagnostics: {warning}"
        ));
    }
    logger.info("Starting %vosu%");
    #[cfg(target_os = "linux")]
    if ordinary_role
        && let Some(warning) = vosu_sys::platform::cap_sys_ptrace_warning(&resolver.executable)
    {
        logger.warn(warning);
    }
    for warning in config.warnings() {
        logger.warn(warning.clone());
    }

    #[cfg(target_os = "linux")]
    if ordinary_role {
        let enabled = config.current().start_at_login;
        match tokio::task::spawn_blocking(move || vosu_core::desktop::login::reconcile(enabled))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                logger.warn(format!("Start at login could not be reconciled: {error}"))
            }
            Err(error) => logger.warn(format!("Start at login task failed: {error}")),
        }
    }
    let current = config.current();
    let changes = config.subscribe();
    let shutdown_token = CancellationToken::new();
    let signal_task = tokio::spawn(run_signal_cancellation(shutdown_token.clone()));
    let dev_plugin_dirs = vosu_core::plugin::development_roots(args.raw())?;
    // Resolve development roots before constructing owned runtime resources.
    let dev_tools = crate::tools::development_roots(args.raw())?;
    let origins = Arc::new(crate::server::origins::Origins::new(
        vosu_core::desktop::dashboard_url(&current.server_ip, listen_target(&current)?.port),
        vosu_core::desktop::dashboard_url(
            &current.dashboard_ip,
            dashboard_listen_target(&current)?.port,
        ),
    )?);
    let settings_bridge = Arc::new(ConfigSettingsBridgeB2::new(Arc::clone(&config)));
    let settings_storage: Arc<dyn SettingsStorage> = settings_bridge.clone();
    let plugin_settings = Arc::new(PluginSettingsService::new(settings_storage));
    let plugin_host = api_v1::resource_host(logger.clone(), plugin_settings, paths.data()).await?;
    let state_host = vosu_surface::state::install_state_host(plugin_host.surface_host())
        .map_err(io::Error::other)?;
    let plugin_settings_watch = plugin_host.watch_bridge_b2(&settings_bridge, &Handle::current());
    // Discovery/foreground poller start in `compose()`, not here (P3: only after `App::run`'s guards).
    let hooks = ProductionRuntime {
        config,
        changes,
        logger,
        paths: paths.clone(),
        shutdown_token,
        signal_task: AsyncMutex::new(Some(signal_task)),
        startup_readiness,
        authorizer: Arc::new(ReloadableAuthorizer::new(
            &current.allowed_ips,
            &current.server_ip,
        )),
        update_handed_off: AtomicBool::new(false),
        update_notice: None,
        counters: AsyncMutex::new(None),
        tools: AsyncMutex::new(None),
        dev_tools,
        dev_plugins: dev_plugin_dirs,
        api_v1: api_v1::ApiV1Service::new(&plugin_host, &current),
        plugins: plugin_host,
        state_host: AsyncMutex::new(Some(state_host)),
        adapters: AsyncMutex::new(None),
        plugin_settings_watch: AsyncMutex::new(Some(plugin_settings_watch)),
        listener: AsyncMutex::new(None),
        dashboard_listener: AsyncMutex::new(None),
        origins,
        hubs: AsyncMutex::new(None),
        processor: AsyncMutex::new(None),
        relays: AsyncMutex::new(None),
        ordinary_role,
        tray: AsyncMutex::new(None),
        discovery: AsyncMutex::new(None),
        #[cfg(windows)]
        foreground: AsyncMutex::new(None),
        #[cfg(target_os = "linux")]
        login_state: Arc::new(AsyncMutex::new(current.start_at_login)),
    };
    let mut app = App::new(
        startup_flags,
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

#[cfg(test)]
#[path = "tests/lifecycle_owner.rs"]
mod owner_tests;

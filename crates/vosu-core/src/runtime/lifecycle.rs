use super::*;

#[path = "api_v1.rs"]
pub(super) mod api_v1;
use super::signals::run_signal_cancellation;

/// Process-wide resource owner. Every long-lived production service is retained
/// here and joined in reverse dependency order.
pub struct ProductionRuntime {
    /// The executable's bundled set (`run_cli`'s argument).
    bundle: &'static Bundle,
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
    /// Core's D20 recorder (A7.4/A7.5): one per process, outliving any route composition.
    removed_routes: Arc<crate::removed_routes::RemovedRoutes>,
    /// A7.1-OR and the A7 overlay host: the counter service, command
    /// processing and relays, the root seed's `SETTINGS_CHANGED` delivery
    /// and the read-only lease generations, composed by the bundle.
    overlay: AsyncMutex<Option<Arc<dyn OverlayComposition>>>,
    settings_storage: Arc<super::overlay_root::RuntimeSettingsStorage>,
    plugin_settings: Arc<PluginSettingsService>,
    tools: AsyncMutex<Option<Arc<crate::tools::ToolRegistry>>>,
    /// The bundled tool host's routes; dropped before the registry stops.
    tool_host: AsyncMutex<Option<Box<dyn Send + Sync>>>,
    /// K0 catalog owner: started before tools/discovery/routes, stopped first.
    catalog: AsyncMutex<Option<super::CatalogComposition>>,
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
    ordinary_role: bool,
    /// The A7.3 dashboard switch, resolved once by `run_cli`; a live edit of
    /// `ENABLE_DASHBOARD` is logged as needing a restart and never applied.
    dashboard: bool,
    tray: AsyncMutex<Option<crate::desktop::tray::TrayHandle>>,
    discovery: AsyncMutex<Option<crate::plugin::DiscoveryService>>,
    #[cfg(windows)]
    foreground: AsyncMutex<Option<crate::plugin::ForegroundPoller>>,
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
        let mut result = Ok(());
        self.shutdown_token.cancel();
        self.api_v1.shutdown();
        if let Some(watch) = self.plugin_settings_watch.lock().await.take() {
            watch.abort();
            if let Err(error) = watch.await
                && !error.is_cancelled()
            {
                result = Err(io::Error::other(format!(
                    "plugin settings watch failed: {error}"
                )));
            }
        }
        if let Some(catalog) = self.catalog.lock().await.take() {
            catalog.shutdown().await;
        }
        self.tool_host.lock().await.take();
        if let Some(tools) = self.tools.lock().await.take() {
            tools.shutdown().await;
        }
        // M1 §4 phase d2b unit d: discovery (and, on Windows, the
        // foreground poller) must stop before `destroy_all` — both call
        // back into `self.plugins` (`process_appeared`/`process_exited`/
        // `foreground_changed`), which must not race a plugin instance
        // tearing down under it.
        if let Some(discovery) = self.discovery.lock().await.take() {
            result = result.and(
                tokio::task::spawn_blocking(move || discovery.stop())
                    .await
                    .map_err(|error| {
                        io::Error::other(format!("discovery shutdown task failed: {error}"))
                    }),
            );
        }
        #[cfg(windows)]
        if let Some(foreground) = self.foreground.lock().await.take() {
            result = result.and(
                tokio::task::spawn_blocking(move || foreground.stop())
                    .await
                    .map_err(|error| {
                        io::Error::other(format!("foreground poller shutdown task failed: {error}"))
                    }),
            );
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
        self.api_v1.join().await;
        // Adapter consumers stop before plugin destroy; neither failure may
        // skip the remaining joins.
        if let Some(adapters) = self.adapters.lock().await.take() {
            result = result.and(adapters.shutdown().await);
        }
        result = result.and(destroy.await);
        self.state_host.lock().await.take();
        if let Some(overlay) = self.overlay.lock().await.take() {
            overlay.shutdown().await;
        }
        let signal_result = match self.signal_task.lock().await.take() {
            Some(task) => task.await.unwrap_or_else(|error| {
                Err(io::Error::other(format!("signal task failed: {error}")))
            }),
            None => Ok(()),
        };
        // Do not short-circuit cleanup after a task panic: even retained
        // config Arcs must have their blocking worker joined before runtime drop.
        result = result.and(signal_result);
        result = result.and(self.config.shutdown().await);
        result.and(self.logger.shutdown().await)
    }

    async fn compose(&mut self) -> io::Result<()> {
        let broker_path = self.paths.broker_path().map(Some);
        self.compose_with_discovery(broker_path, |host, logger| {
            discovery::start_discovery(host, Handle::current(), logger)
        })
        .await
    }

    async fn compose_with_discovery(
        &mut self,
        broker_path: io::Result<Option<PathBuf>>,
        start_discovery: impl FnOnce(
            &Arc<PluginHost>,
            &Logger,
        ) -> Option<crate::plugin::DiscoveryService>,
    ) -> io::Result<()> {
        if self.listener.get_mut().is_some() {
            return Ok(());
        }
        let bundle = self.bundle;
        let host = Arc::clone(&self.plugins);
        let paths = self.paths.clone();
        let logger = self.logger.clone();
        let dev_dirs = std::mem::take(&mut self.dev_plugins);
        let dashboard = self.dashboard;
        let management = tokio::task::spawn_blocking(move || {
            dev_plugins::load_all(bundle, &host, dev_dirs, &paths, &logger, dashboard)
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
        #[cfg(feature = "test-harness")]
        let adapter_capture = crate::server::adapter_capture::endpoint::Capability::from_env()?;
        let static_root = resolved_static_root(&self.paths, &config)?;
        let overlay_instance = bundle
            .instance(bundle.overlay_root.plugin)
            .ok_or_else(|| io::Error::other("the overlay host is not a required bundled plugin"))?;
        // K0: catalog recovery and committed-graph load precede the tool
        // host's first scan, discovery and every route.
        let catalog = super::CatalogComposition::start(
            Arc::clone(&self.plugins),
            super::CatalogRoots::production(&self.paths, static_root.clone()),
            super::CatalogComposition::production_transport(),
            bundle,
        );
        *self.catalog.get_mut() = Some(catalog.await?);
        // Owned from here on: every later failure leaves it to `shutdown`.
        let overlay = bundle.hosts.overlay(OverlayInputs {
            host: Arc::clone(&self.plugins),
            instance: overlay_instance,
            data_dir: self
                .paths
                .bundled_plugin_data_dir(bundle.overlay_root.plugin),
            storage: Arc::clone(&self.settings_storage),
            settings: Arc::clone(&self.plugin_settings),
            static_root,
            settings_root: self.paths.config().join("settings"),
            cache_root: self.paths.cache().join("downloads"),
            version: bundle.version(),
            catalog: OverlayCatalog::Production,
            shutdown: self.shutdown_token.clone(),
            origins: Arc::clone(&self.origins),
        });
        *self.overlay.get_mut() = Some(Arc::clone(&overlay));
        {
            let overlay = Arc::clone(&overlay);
            let paths = self.paths.clone();
            let logger = self.logger.clone();
            self.config.register_before_publish(move |next| {
                if let Err(error) = reload_overlay_root(&paths, next, overlay.as_ref()) {
                    logger.warn(format!(
                        "Failed to resolve or create requested static root; retaining previous counter root: {error}"
                    ));
                }
            });
        }
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
        let (adapters, hubs) = adapters::Adapters::start(
            bundle,
            &self.plugins,
            Arc::clone(&self.config),
            self.dashboard,
        )
        .await?;
        *self.adapters.get_mut() = Some(adapters.clone());
        let tools = crate::tools::ToolRegistry::new(
            self.paths.config().join("tools"),
            self.paths.data().join("tools"),
            self.paths.cache().join("tools"),
            crate::tools::ExtraRoots {
                dev: self.dev_tools.clone(),
            },
            self.origins.overlay(),
            self.dashboard.then(|| self.origins.dashboard()),
            bundle.version(),
        )
        .await;
        let tools = match tools {
            Ok(tools) => tools,
            Err(error) => {
                hubs.shutdown().await;
                return Err(error);
            }
        };
        let tool_host = bundle
            .instance(bundle.tool_host)
            .ok_or_else(|| io::Error::other("the tool host is not a required bundled plugin"))
            .and_then(|instance| {
                self.plugins
                    .surface_requester(instance)
                    .map_err(io::Error::other)
            })
            .and_then(|requester| {
                // A7.3: with the dashboard off nothing registers on its role.
                use vosu_surface::listener::ListenerRole::{Dashboard, Overlay};
                let roles: &[_] = if self.dashboard {
                    &[Overlay, Dashboard]
                } else {
                    &[Overlay]
                };
                bundle
                    .hosts
                    .mount_tool_host(requester, tools.tool_host_views(), roles)
                    .map_err(io::Error::other)
            });
        match tool_host {
            Ok(tool_host) => *self.tool_host.get_mut() = Some(tool_host),
            Err(error) => {
                tools.shutdown().await;
                hubs.shutdown().await;
                return Err(error);
            }
        }
        if let Some(catalog) = self.catalog.get_mut() {
            catalog.attach_tools(tools.clone());
            catalog.attach_overlay(Arc::clone(&overlay));
        }
        *self.tools.get_mut() = Some(tools.clone());
        if let Err(error) = overlay.mount(hubs.v1.clone()) {
            hubs.shutdown().await;
            return Err(error);
        }
        let services = ProductionServices::with_native_report_probe(
            Arc::clone(&overlay),
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
                adapters: Arc::clone(&adapters),
                overlay: Arc::clone(&overlay),
                logs: crate::logging::logs_dir(&self.paths),
                update: self.update_notice.clone(),
            }),
            Arc::new(RuntimeUpdater::production(
                program,
                self.paths.program().to_path_buf(),
                self.shutdown_token.clone(),
                bundle.version(),
            )),
            WebSocketEndpoints {
                v1: hubs.v1.clone(),
                sc: hubs.stream_companion.clone(),
                v2: hubs.v2.clone(),
                precise: hubs.v2_precise.clone(),
                commands: hubs.v1.clone(),
            },
        );
        let delegate = ProductionRouteDelegate::new(services)
            .with_surface(adapters.router(self.plugins.clone()))
            .with_api_keys(Arc::new(api_keys))
            .with_tools(tools.clone())
            .with_plugins(Arc::new(management))
            .with_api_v1(self.api_v1.action_router(&self.plugins, tools.clone()))
            .with_removed_routes(Arc::clone(&self.removed_routes));
        let live_config = Arc::clone(&self.config);
        let delegate = match self.catalog.get_mut() {
            Some(catalog) => delegate.with_catalog(catalog.api(
                Arc::new(move || live_config.current().max_package_bytes()),
                crate::server::catalog_api::network(),
            )),
            None => delegate,
        };
        #[cfg(feature = "test-harness")]
        let delegate = match adapter_capture {
            Some(capability) => delegate.with_adapter_capture(
                crate::server::adapter_capture::endpoint::AdapterCaptureEndpoint::new(
                    capability,
                    adapters.clone(),
                ),
            ),
            None => delegate,
        };
        let providers = RouteProviders {
            authorization: Arc::new(crate::server::origins::OriginAuthorizer {
                inner: self.authorizer.clone(),
                origins: Arc::clone(&self.origins),
                dashboard: false,
            }),
            delegate: Arc::new(delegate),
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
            broker_path,
            Some(self.logger.clone()),
        )
        .await
        {
            Ok(listener) => listener,
            Err(error) => {
                hubs.shutdown().await;
                return Err(error);
            }
        };
        listener.watch_cli_actions(&self.plugins).await;
        let Some(effective_listener) = listener.state().effective else {
            listener.stop_accepting();
            hubs.shutdown().await;
            listener.join().await;
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
        // A7.3: off binds no dashboard listener, and the origins observe an
        // empty dashboard state.
        let dashboard_listener = if self.dashboard {
            match ListenerOwner::start(dashboard_providers, dashboard_target).await {
                Ok(listener) => Some(listener),
                Err(error) => {
                    listener.stop_accepting();
                    hubs.shutdown().await;
                    listener.join().await;
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.origins.observe_listeners(
            listener.state.clone(),
            dashboard_listener.as_ref().map_or_else(
                || tokio::sync::watch::channel(Default::default()).1,
                |dashboard| dashboard.state.clone(),
            ),
        );
        tools.activate(Some(self.origins.clone()));
        if let Some(dashboard) = &dashboard_listener {
            let effective = dashboard
                .state()
                .effective
                .expect("started dashboard listener has an effective address");
            self.logger.info(format!(
                "Dashboard: {}",
                crate::desktop::dashboard_url(&effective.target.host, effective.address.port())
            ));
        }
        *self.dashboard_listener.get_mut() = dashboard_listener;
        *self.listener.get_mut() = Some(listener);
        *self.hubs.get_mut() = Some(hubs);
        let discovery = start_discovery(&self.plugins, &self.logger).ok_or_else(|| {
            let message = "required osu! discovery did not start";
            self.logger.error(message);
            io::Error::other(message)
        })?;
        #[cfg(windows)]
        let foreground = crate::plugin::ForegroundPoller::start(&self.plugins, Handle::current());
        *self.discovery.get_mut() = Some(discovery);
        #[cfg(windows)]
        {
            *self.foreground.get_mut() = Some(foreground);
        }
        if self.ordinary_role {
            let origins = Arc::clone(&self.origins);
            let dashboard_url: crate::desktop::tray::DashboardUrl =
                Arc::new(move || format!("{}/", origins.dashboard()));
            *self.tray.get_mut() = Some(crate::desktop::tray::start(
                self.dashboard.then_some(dashboard_url),
                crate::logging::logs_dir(&self.paths),
                self.shutdown_token.clone(),
            ));
        }
        if self.ordinary_role && self.dashboard && config.open_dashboard_on_startup {
            let url = format!("{}/", self.origins.dashboard());
            if let Err(error) =
                tokio::task::spawn_blocking(move || vosu_sys::process_spawn::open_detached(url))
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
            if !crate::desktop::directory_writable(self.paths.program()) {
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
                &self.bundle.version(),
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
                        self.bundle.version
                    ));
                    self.update_notice = Some(UpdateNotice::Minor(version.to_string()));
                }
                UpdateCheck::MajorAvailable(release) => {
                    self.logger.warn(format!(
                        "Major update available: v{} => v{} (requires dashboard confirmation): {}",
                        self.bundle.version, release.version, release.release_url
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
                            if change.old.enable_dashboard != change.new.enable_dashboard {
                                self.logger.warn("ENABLE_DASHBOARD changed in vosu.toml; restart vosu to apply it");
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

/// Runs the daemon for `bundle` with the real process environment and
/// command line.
pub async fn run_cli(bundle: &'static Bundle) -> io::Result<()> {
    let raw_args: Vec<OsString> = env::args_os().collect();
    match super::cli::classify_command_invocation(&raw_args) {
        super::cli::CommandInvocation::RunDaemon => {}
        super::cli::CommandInvocation::Command(command) => {
            return super::cli::run_command(command, bundle).await;
        }
        super::cli::CommandInvocation::InvalidInput => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid command arguments; see vosu --help",
            ));
        }
    }
    #[cfg(windows)]
    vosu_sys::platform::detach_private_console();
    // A7.3: an invalid VOSU_DASHBOARD fails startup before any probe or bind.
    let dashboard_forced_off =
        super::dashboard::forced_off(env::var_os(super::dashboard::ENV).as_deref())?;
    let startup_flags = bundle.hosts.startup_flags(&raw_args);
    // `argv` after the executable: the `--plugin`/`--tool` roots' input.
    let args = raw_args.get(1..).unwrap_or_default().to_vec();
    let environment = EnvironmentSnapshot::capture();
    let resolver = SnapshotPathResolver {
        executable: env::current_exe()?,
        launch: env::current_dir()?,
        environment: &environment,
    };
    let channel =
        vosu_sys::platform::BuildChannel::from_version(bundle.version).map_err(io::Error::other)?;
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
    // Read once: a live edit of ENABLE_DASHBOARD needs a restart.
    let dashboard = !dashboard_forced_off && config.current().enable_dashboard;
    if ordinary_role
        && dashboard
        && let Ok(target) = dashboard_listen_target(&config.current())
        && let Some(url) = crate::desktop::existing_dashboard(&target.host, target.port).await
    {
        println!("vosu is already running; opening {url}");
        if let Err(error) = vosu_sys::process_spawn::open_detached(&url) {
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
        crate::logging::logs_dir(&paths),
        daemon_logger_options(bundle, config.current().debug_log),
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
        match tokio::task::spawn_blocking(move || crate::desktop::login::reconcile(enabled)).await {
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
    let dev_plugin_dirs = crate::plugin::development_roots(&args)?;
    // Resolve development roots before constructing owned runtime resources.
    let dev_tools = crate::tools::development_roots(&args)?;
    let origins = crate::server::origins::Origins::new(
        crate::desktop::dashboard_url(&current.server_ip, listen_target(&current)?.port),
        crate::desktop::dashboard_url(
            &current.dashboard_ip,
            dashboard_listen_target(&current)?.port,
        ),
    )?;
    let origins = Arc::new(if dashboard {
        origins
    } else {
        origins.without_dashboard()
    });
    let settings_bridge = Arc::new(ConfigSettingsBridgeB2::new(Arc::clone(&config)));
    let settings_storage = Arc::new(super::overlay_root::RuntimeSettingsStorage::new(
        settings_bridge.clone(),
        Arc::new(crate::plugin::settings::PluginSettingsStore::open(
            paths.config(),
        )),
        bundle.overlay_root,
    ));
    // The overlay host is created with the effective root already seeded.
    if let Ok(root) = resolved_static_root(&paths, &current) {
        settings_storage.seed_overlay_root(&root);
    }
    let plugin_settings = Arc::new(PluginSettingsService::new(
        settings_storage.clone() as Arc<dyn SettingsStorage>
    ));
    let plugin_host =
        api_v1::resource_host(logger.clone(), plugin_settings.clone(), paths.data()).await?;
    let state_host = vosu_surface::state::install_state_host(plugin_host.surface_host())
        .map_err(io::Error::other)?;
    let plugin_settings_watch = plugin_host.watch_bridge_b2(&settings_bridge, &Handle::current());
    // Discovery/foreground poller start in `compose()`, not here (P3: only after `App::run`'s guards).
    let hooks = ProductionRuntime {
        bundle,
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
        removed_routes: Arc::default(),
        overlay: AsyncMutex::new(None),
        settings_storage,
        plugin_settings,
        tools: AsyncMutex::new(None),
        tool_host: AsyncMutex::new(None),
        catalog: AsyncMutex::new(None),
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
        ordinary_role,
        dashboard,
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

#[cfg(feature = "test-harness")]
#[path = "test_daemon.rs"]
mod test_daemon;
#[cfg(feature = "test-harness")]
pub use test_daemon::{TestAdapters, TestDaemon};

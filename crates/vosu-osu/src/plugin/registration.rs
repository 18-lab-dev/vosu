//! Identified-session setup, including frozen selected-client launch facts.
use super::*;
/// Prepare the action target before making it public through SESSION_IDENTIFY.
/// B2 settings/control delivery remains serialized by PluginState; no host call
/// runs under that lock. Capture/B1 and lazer resolution start only after the
/// host accepts identification. Rejection rolls back the prepared session.
#[expect(
    clippy::too_many_arguments,
    reason = "registration requires the frozen attach facts and resolved public version"
)]
pub(super) fn register_session<H, G>(
    host: &Arc<H>,
    state: &Arc<Mutex<PluginState>>,
    scan_waiters: &Arc<ScanWaiters>,
    loader: &Arc<LazerOffsetLoader<G>>,
    runtime_handle: &Handle,
    handle: u64,
    identified: &Identified,
    version: String,
) -> Result<(), PluginError>
where
    H: HostServices + Send + Sync + Clone + 'static,
    G: OffsetGenerator + 'static,
{
    if !state
        .lock()
        .expect("plugin state mutex poisoned")
        .live_handles
        .contains(&handle)
    {
        return Ok(());
    }
    let bitness = to_process_bitness(identified.bitness);
    let memory = Arc::new(PlanProcessMemory::new(
        (**host).clone(),
        handle,
        identified.pid,
        bitness,
    ));
    let alive = memory.alive_flag();
    let scans: Arc<dyn ScanBackend> = Arc::new(PlanScanBackend::with_shared(
        (**host).clone(),
        handle,
        Arc::clone(scan_waiters),
        Arc::clone(&alive),
    ));
    let process = ProcessHandle::with_scan_backend(memory, bitness, scans);
    let session = OsuSession::new(
        handle,
        identified.pid,
        identified.generation,
        identified.client,
        process,
        identified.root.clone(),
        identified.executable_root.clone(),
        identified.settings.clone(),
        Some(SessionScanLifecycle {
            waiters: Arc::clone(scan_waiters),
            alive,
        }),
    );
    let tournament = {
        let state = state.lock().expect("plugin state mutex poisoned");
        let manager = state.attached_facts.get(&handle).is_some_and(|attached| {
            let args = attached
                .argv
                .as_ref()
                .map(|argv| {
                    vosu_osu_reader::discovery::StartupArgs::parse(
                        argv.iter().map(std::ffi::OsString::from),
                    )
                })
                .or_else(|| {
                    attached
                        .command_line
                        .as_deref()
                        .map(vosu_osu_reader::discovery::StartupArgs::parse_command_line)
                });
            args.is_some_and(|args| args.get("tournament").is_some())
        });
        session.set_tournament_manager(manager);
        Arc::clone(&state.tournament)
    };
    let _ = session.tournament.set(Arc::clone(&tournament));
    if identified.client == ClientType::Lazer {
        let attached = state
            .lock()
            .expect("plugin state mutex poisoned")
            .attached_facts
            .get(&handle)
            .cloned();
        if let Some(attached) = attached
            && let Ok(context) = vosu_sys::process::launch_context(attached.pid)
        {
            let _ = session.import_context.set((attached, context));
        }
    }
    session.configure_publication(Arc::clone(host), runtime_handle);
    let stream_ids = state
        .lock()
        .expect("plugin state mutex poisoned")
        .stream_ids
        .clone();
    resources::configure(&session, host, stream_ids);
    let registered = {
        let mut state = state.lock().expect("plugin state mutex poisoned");
        if state.live_handles.contains(&handle) {
            session.osu_settings.send_replace(state.settings);
            if let Some(controls) = state.pending_controls.remove(&handle) {
                let publication = session.publication.get().expect("publication configured");
                publication.bus.update_foreground(controls.foreground);
                publication.demand.set_bus_collection(
                    controls.regular,
                    controls.precise,
                    controls.collect_errors,
                );
                publication
                    .demand
                    .tournament
                    .request(controls.tourney.0, controls.tourney.1);
            }
            state.sessions.insert(handle, Arc::clone(&session));
            true
        } else {
            false
        }
    };
    let identification = if registered {
        let (variant, name) = match identified.client {
            ClientType::Stable => ("stable", "osu!"),
            ClientType::Lazer => ("lazer", "osu!(lazer)"),
        };
        finish_identify(
            host.as_ref(),
            state,
            handle,
            variant,
            name,
            version,
            identified.named,
        )
    } else {
        Ok(false)
    };
    if !matches!(identification, Ok(true)) {
        {
            let mut state = state.lock().expect("plugin state mutex poisoned");
            if state
                .sessions
                .get(&handle)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                state.sessions.remove(&handle);
            }
        }
        session.cancel();
        runtime_handle.spawn(async move {
            session.join_resolver().await;
        });
        return identification.map(|_| ());
    }
    let _ = session
        .publication
        .get()
        .expect("publication configured")
        .bus
        .tournament
        .set((Arc::clone(&tournament), handle));
    if session.client == ClientType::Stable {
        tournament.attach(
            &session
                .publication
                .get()
                .expect("publication configured")
                .bus,
            handle,
            session.is_tournament_manager(),
            session.settings.clone(),
            crate::publish::tournament::Controls {
                selected: Arc::clone(&session.selected),
                demand: session
                    .publication
                    .get()
                    .expect("publication configured")
                    .demand
                    .tournament
                    .clone(),
            },
        );
    }
    if identified.client == ClientType::Lazer {
        let loader = Arc::clone(loader);
        let warn = log_warn(host);
        runtime_handle.spawn(async move {
            session.start_lazer_resolver(loader, warn).await;
        });
    }
    if registered {
        let sink = vosu_surface::current::<B1Event, B1Inputs>();
        let warn = log_warn(host);
        let error = log_error(host);
        let session = {
            let state = state.lock().expect("plugin state mutex poisoned");
            state.sessions.get(&handle).cloned()
        };
        if let Some(session) = session {
            runtime_handle.spawn(async move {
                session.start_capture(sink, warn, error).await;
            });
        }
    }
    Ok(())
}

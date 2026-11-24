//! Identified-session setup, including frozen selected-client launch facts.
use super::*;
/// Builds this handle's `ProcessHandle` (P3's `PlanProcessMemory` +
/// `PlanScanBackend`, sharing this plugin instance's `ScanWaiters`) and
/// registers its [`OsuSession`] (P4) — but only if the handle is still
/// live: a detach racing this identify, landing between `finish_identify`
/// succeeding and the lock this function takes, must never leave a
/// session registered that nothing will ever detach again (the module
/// doc comment's "P4's analogous race"). A lazer session's resolver is
/// started on `runtime_handle` only once registration itself succeeded.
/// Free (not a method), for the same reason as `finish_identify`: the
/// lazer identify thread calls this with only the `Arc`s it captured.
pub(super) fn register_session<H, G>(
    host: &Arc<H>,
    state: &Arc<Mutex<PluginState>>,
    scan_waiters: &Arc<ScanWaiters>,
    loader: &Arc<LazerOffsetLoader<G>>,
    runtime_handle: &Handle,
    handle: u64,
    identified: &Identified,
) where
    H: HostServices + Send + Sync + Clone + 'static,
    G: OffsetGenerator + 'static,
{
    if !state
        .lock()
        .expect("plugin state mutex poisoned")
        .live_handles
        .contains(&handle)
    {
        return;
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
            }
            state.sessions.insert(handle, Arc::clone(&session));
            true
        } else {
            false
        }
    };
    if !registered {
        runtime_handle.spawn(async move {
            session.join_resolver().await;
        });
        return;
    }
    if registered && identified.client == ClientType::Lazer {
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
}

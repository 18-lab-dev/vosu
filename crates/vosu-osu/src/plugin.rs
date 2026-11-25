//! `OsuPlugin`: the `ConcurrentPlugin` entry point (D22, abi:408 §10). P2
//! wires attach/detach/settle-timer handling through
//! [`crate::classify::decide`] into `SESSION_DECLINE`/`SESSION_IDENTIFY`,
//! and routes `VOSU_OP_SCAN_RESULT` into P3's [`crate::scan::ScanWaiters`].
//! P4 adds the session map ([`crate::session::OsuSession`]) and this
//! plugin's own single-worker Tokio runtime: every identified session gets
//! a `ProcessHandle` backed by P3's `PlanProcessMemory`/`PlanScanBackend`,
//! and a lazer session's offset resolver is started on that runtime, never
//! inline on a dispatch thread. P5 adds session-owned capture workers and
//! state-bus publication and selector-driven sampling.
//!
//! Generic over `H: HostServices` (default [`VtableHostServices`]) and `G:
//! OffsetGenerator` (default [`EmbeddedOffsetGenerator`]), exactly like
//! `PlanProcessMemory` (P3): the exported entry point only ever
//! instantiates `OsuPlugin<VtableHostServices>`, but a test supplies any
//! other `HostServices`/`OffsetGenerator` implementation (a recording fake
//! host, an offline generator) directly, without going through the real
//! ABI vtable or a real lazer install. `host` and `state` are `Arc`-wrapped
//! (not bare fields) so a lazer version lookup can run on its own thread
//! (see `OsuPlugin::commit_identify`) while still sharing them with the
//! instance that spawned it; `H: Clone` lets `register_session` hand
//! P3's backends their own owned copy, exactly as `PlanProcessMemory`/
//! `PlanScanBackend` already require.
//!
//! Lock order: a single `Mutex<PluginState>` — which, since P4, also owns
//! the session map, so an identify that only now notices a race already
//! lost (review #3) and a detach that races it (P4's analogous race, one
//! step later: between `finish_identify` succeeding and
//! `register_session`'s own insert) serialize on the very same lock
//! rather than needing a second one. Never held across a call into
//! `self.host` (`identify_session`/`send`/`set_timer`/`log`) or into
//! `self.runtime_handle`/`self.runtime` (spawning, joining, or shutting
//! down): every method reads or mutates the state in its own short-lived
//! guard, drops it, and only then calls out.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::runtime::{Handle, Runtime};
use vosu_osu_model::enums::ClientType;
use vosu_osu_reader::assets::DaemonAssets;
use vosu_osu_reader::memory::lazer_loader::{LazerOffsetLoader, LogWarn, OffsetGenerator};
use vosu_osu_reader::offset_generator::EmbeddedOffsetGenerator;
use vosu_osu_reader::process::{ProcessHandle, ScanBackend};
use vosu_plugin_api::{
    CborValue, ConcurrentPlugin, HostServices, Init, LogLevel, Message, PluginError,
    SessionAttached, SessionDecline, SessionDetached, TimerFired, VtableHostServices, vosu_status,
};

use crate::classify::{
    self, AttachDecision, AttachFacts, Identified, PendingIdentify, Platform, RootResolver,
    to_process_bitness,
};
use crate::memory::PlanProcessMemory;
use crate::scan::{PlanScanBackend, ScanWaiters};
use crate::session::{OsuSession, SessionScanLifecycle};
use crate::settings::OsuSettings;
mod registration;
mod resources;
use registration::register_session;

/// abi:82's destroy grace: `destroy` shuts every still-live session down
/// (cancel, then join) within this budget before shutting the plugin's own
/// runtime down (DG), rather than waiting out a hung resolver forever.
const DESTROY_GRACE: Duration = Duration::from_secs(5);

#[derive(Default)]
struct PendingControls {
    foreground: Option<bool>,
    regular: bool,
    precise: bool,
    collect_errors: bool,
    tourney: (bool, bool),
}

#[derive(Default)]
struct PluginState {
    /// Handles currently attached (inserted at `SESSION_ATTACHED`, removed
    /// at `SESSION_DETACHED`): consulted so a detach racing a concurrent
    /// identify never leaves a stale entry in `named_stable_handles`
    /// (review #3), and so [`register_session`] never registers a session
    /// for a handle that already detached (P4's analogous race).
    live_handles: HashSet<u64>,
    attached_facts: HashMap<u64, SessionAttached>,
    /// Handles identified through the Linux "named" rule specifically
    /// (`Identified::named`/review #2), consulted so a later fallback
    /// attach's settle sees a named sibling (DF/R3) — never a settled
    /// fallback, a Windows client, or a nameless Linux attach, none of
    /// which are "named" even when they resolved to stable.
    named_stable_handles: HashSet<u64>,
    /// Linux fallback candidates waiting for `TIMER_FIRED`, keyed by this
    /// plugin's own timer id; the stored handle lets `SESSION_DETACHED`
    /// cancel one before its timer fires.
    pending_settle: HashMap<u64, (u64, PendingIdentify)>,
    next_timer: u64,
    /// Lazer version lookups handed off to their own thread (§4; review
    /// #6), joined on `Drop` so none outlives this plugin instance's
    /// borrowed `VtableHostServices`, whose raw pointers the ABI
    /// documents as valid only until `destroy` returns (abi:82).
    lazer_lookups: Vec<JoinHandle<()>>,
    /// Handles whose `SESSION_DETACHED` arrived before their own
    /// `SESSION_ATTACHED` (P3 review #4): `WorkerPool`'s concurrent
    /// dispatch does not preserve per-handle order, so this reordering is
    /// possible even though a host never actually detaches a handle it
    /// has not yet attached. Recorded here so `handle_attached` skips
    /// inserting the handle into `live_handles` — nothing would ever
    /// remove it otherwise — while still classifying and replying to the
    /// attach as usual.
    detached_before_attached: HashSet<u64>,
    /// Every session a successful identify registered (P4), keyed by
    /// handle, removed by `SESSION_DETACHED` (or `destroy`). Lives under
    /// this same lock specifically to close the identify/detach race
    /// described on the module doc comment.
    sessions: HashMap<u64, Arc<OsuSession>>,
    stream_ids: Arc<std::sync::atomic::AtomicU64>,
    /// A detach's off-thread resolver join (never awaited on the dispatch
    /// thread that handled the detach itself — see `handle_detached`),
    /// joined by `destroy` within the grace period, or by a test's
    /// [`OsuPlugin::join_background_session_detaches`].
    session_detach_joins: Vec<tokio::task::JoinHandle<()>>,
    settings: OsuSettings,
    // Only live, not-yet-registered handles. IDENTIFY may synchronously cause
    // focus/demand deliveries before register_session has inserted the session.
    pending_controls: HashMap<u64, PendingControls>,
    tournament: Arc<crate::publish::tournament::Tournament>,
    #[cfg(test)]
    settings_before_send: Option<std::sync::mpsc::SyncSender<()>>,
}

pub struct OsuPlugin<
    H: HostServices + Send + Sync + Clone + 'static = VtableHostServices,
    G: OffsetGenerator + 'static = EmbeddedOffsetGenerator,
> {
    host: Arc<H>,
    platform: Platform,
    state: Arc<Mutex<PluginState>>,
    root_resolver: RootResolver,
    scan_waiters: Arc<ScanWaiters>,
    /// DH: resolves offsets under `<data_dir>/offsets`, generating with
    /// `<data_dir>/offset-generator` as scratch space.
    loader: Arc<LazerOffsetLoader<G>>,
    /// This plugin's own single-worker Tokio runtime, created in `create`
    /// and shut down (bounded, abi:82) in `destroy`. A cheap `Handle`
    /// clone lets a dispatch thread (which has no runtime context of its
    /// own) or the lazer-version-lookup OS thread enqueue work on it
    /// without ever locking `runtime` itself; only `Drop` takes the owned
    /// `Runtime` back out, to shut it down.
    runtime_handle: Handle,
    runtime: Mutex<Option<Runtime>>,
    imports: crate::actions::ImportActions,
}

impl<H: HostServices + Send + Sync + Clone + 'static> OsuPlugin<H, EmbeddedOffsetGenerator> {
    pub fn new(host: H, platform: Platform, data_dir: PathBuf) -> Self {
        Self::with_root_resolver(host, platform, classify::resolve_root, data_dir)
    }

    /// Like [`Self::new`], with an injectable root resolver (P3 review
    /// #7): a test uses this to avoid the real `/proc`/env-var reads
    /// `classify::resolve_root` performs; production (`ConcurrentPlugin::
    /// create`) always uses [`Self::new`].
    pub fn with_root_resolver(
        host: H,
        platform: Platform,
        root_resolver: RootResolver,
        data_dir: PathBuf,
    ) -> Self {
        // The reader has no vosu-sys edge (M5 R1); this composer injects the
        // process-wide creation gate (plugin-abi.md §7.4) the generator's
        // children must go through.
        let generator = EmbeddedOffsetGenerator::new(
            data_dir.join("offset-generator"),
            vosu_sys::process_spawn::spawn_tokio,
        );
        Self::with_generator(host, platform, root_resolver, data_dir, generator)
    }
}

impl<H: HostServices + Send + Sync + Clone + 'static, G: OffsetGenerator + 'static>
    OsuPlugin<H, G>
{
    /// Like [`OsuPlugin::<H, EmbeddedOffsetGenerator>::with_root_resolver`],
    /// with an injectable offset generator: a test supplies one that never
    /// touches the network or a real osu!lazer install (P4 acceptance:
    /// "use an offline generator").
    pub fn with_generator(
        host: H,
        platform: Platform,
        root_resolver: RootResolver,
        data_dir: PathBuf,
        generator: G,
    ) -> Self {
        let asset =
            DaemonAssets::get("offsets.json").expect("embedded assets/offsets.json is missing");
        let loader = Arc::new(LazerOffsetLoader::new(
            generator,
            data_dir.join("offsets"),
            Bytes::copy_from_slice(&asset.data),
        ));
        Self::with_loader(host, platform, root_resolver, loader)
    }

    pub fn with_loader(
        host: H,
        platform: Platform,
        root_resolver: RootResolver,
        loader: Arc<LazerOffsetLoader<G>>,
    ) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            // Readers and blocking initialization/join work run off-runtime.
            // A second scheduler worker adds a park/unpark handoff per sample.
            .worker_threads(1)
            .thread_name("vosu-osu")
            .enable_all()
            .build()
            .expect("failed to start the vosu-osu plugin's own runtime");
        let runtime_handle = runtime.handle().clone();
        Self {
            host: Arc::new(host),
            platform,
            state: Arc::new(Mutex::new(PluginState {
                stream_ids: Arc::new(std::sync::atomic::AtomicU64::new(1)),
                ..PluginState::default()
            })),
            root_resolver,
            scan_waiters: ScanWaiters::new(),
            loader,
            runtime_handle,
            runtime: Mutex::new(Some(runtime)),
            imports: crate::actions::ImportActions::default(),
        }
    }

    /// The scan-result waiter map this instance's [`crate::scan::PlanScanBackend`]s
    /// share (wiring item 8): every session's backend is built from this
    /// same instance (see `register_session`).
    #[must_use]
    pub fn scan_waiters(&self) -> &Arc<ScanWaiters> {
        &self.scan_waiters
    }

    /// The real `on_message` logic, independent of `ConcurrentPlugin` so
    /// tests can drive it directly against a fake `H`.
    pub fn dispatch(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::ActionInvoke(action) => {
                let session = action.session.and_then(|handle| {
                    self.state
                        .lock()
                        .expect("plugin state mutex poisoned")
                        .sessions
                        .get(&handle)
                        .cloned()
                });
                self.imports.invoke(
                    &self.host,
                    self.runtime_handle.clone(),
                    action,
                    session.as_ref(),
                )
            }
            Message::ActionCancel(cancel) => self.imports.cancel(cancel.invocation),
            message @ (Message::ResourceResolve(_)
            | Message::TreeList(_)
            | Message::TreeLookup(_)
            | Message::StreamRead(_)
            | Message::StreamClose(_)) => self.dispatch_resource(message, correlation_id),
            Message::SessionAttached(attached) => self.handle_attached(&attached),
            Message::SessionDetached(detached) => {
                self.handle_detached(&detached);
                Ok(())
            }
            Message::TimerFired(fired) => self.handle_timer_fired(&fired),
            Message::ScanResult(result) => {
                self.scan_waiters.deliver(correlation_id, result);
                Ok(())
            }
            Message::SettingsChanged(changed) => self.update_settings(&changed.values),
            Message::DemandChanged(changed) => {
                let collect_errors = changed.selectors.iter().any(|selector| {
                    selector.ns == "vosu.osu"
                        && ["/play/hitOffsets", "/play/keyHitErrors"]
                            .iter()
                            .any(|path| {
                                selector.pointer.is_empty()
                                    || *path == selector.pointer
                                    || path
                                        .strip_prefix(&selector.pointer)
                                        .is_some_and(|rest| rest.starts_with('/'))
                                    || selector
                                        .pointer
                                        .strip_prefix(path)
                                        .is_some_and(|rest| rest.starts_with('/'))
                            })
                });
                let tourney = crate::demand::tournament_lanes(
                    &changed.selectors,
                    changed.regular,
                    changed.precise,
                );
                let mut state = self.state.lock().expect("plugin state mutex poisoned");
                if let Some(session) = state.sessions.get(&changed.handle)
                    && let Some(publication) = session.publication.get()
                {
                    publication.demand.set_bus_collection(
                        changed.regular,
                        changed.precise,
                        collect_errors,
                    );
                    publication.demand.tournament.request(tourney.0, tourney.1);
                } else if state.live_handles.contains(&changed.handle) {
                    let controls = state.pending_controls.entry(changed.handle).or_default();
                    controls.regular = changed.regular;
                    controls.precise = changed.precise;
                    controls.collect_errors = collect_errors;
                    controls.tourney = tourney;
                }
                let tournament = state.tournament.clone();
                drop(state);
                tournament.demand_changed();
                Ok(())
            }
            Message::FocusChanged(changed) => {
                let bus = {
                    let mut state = self.state.lock().expect("plugin state mutex poisoned");
                    if let Some(session) = state.sessions.get(&changed.handle)
                        && let Some(publication) = session.publication.get()
                    {
                        session
                            .selected
                            .store(changed.focused, std::sync::atomic::Ordering::Release);
                        // Only atomics under PluginState; republish can re-enter
                        // the host and therefore runs after releasing the guard.
                        publication
                            .bus
                            .update_foreground(changed.foreground)
                            .then(|| Arc::clone(&publication.bus))
                    } else {
                        if state.live_handles.contains(&changed.handle) {
                            state
                                .pending_controls
                                .entry(changed.handle)
                                .or_default()
                                .foreground = changed.foreground;
                        }
                        None
                    }
                };
                if let Some(bus) = bus {
                    bus.republish_session_fields();
                }
                let tournament = self
                    .state
                    .lock()
                    .expect("plugin state mutex poisoned")
                    .tournament
                    .clone();
                tournament.focus_changed();
                Ok(())
            }
            // Every other opcode this concurrent dispatch call could see has
            // has no plugin-side action in the current milestone.
            _ => Ok(()),
        }
    }

    fn update_settings(&self, values: &CborValue) -> Result<(), PluginError> {
        let settings =
            OsuSettings::from_cbor(values).map_err(|_| vosu_status::VOSU_ERR_INVALID_ARGUMENT)?;
        let mut state = self.state.lock().expect("plugin state mutex poisoned");
        state.settings = settings;
        // Serialize the effective value and its watch publication. Otherwise a
        // concurrent older update can publish after the newer one. Lock order:
        // PluginState -> watch value; receivers copy settings before calling
        // out, and send_replace invokes no plugin or host callbacks.
        for session in state.sessions.values() {
            #[cfg(test)]
            if let Some(observer) = &state.settings_before_send {
                let _ = observer.try_send(());
            }
            session.osu_settings.send_replace(settings);
        }
        Ok(())
    }

    fn handle_attached(&self, message: &SessionAttached) -> Result<(), PluginError> {
        let facts = AttachFacts::from_attached_with(message, self.root_resolver);
        let named_sibling_attached = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            // A `SESSION_DETACHED` that raced this very attach and arrived
            // first (P3 review #4): this handle already detached, so it
            // must never be inserted into `live_handles` — no later
            // `SESSION_DETACHED` is still coming to remove it. Still
            // classified and replied to below, exactly as usual.
            if !state.detached_before_attached.remove(&facts.handle) {
                state.live_handles.insert(facts.handle);
                state.attached_facts.insert(facts.handle, message.clone());
            }
            !state.named_stable_handles.is_empty()
        };
        match classify::decide(&facts, self.platform, named_sibling_attached) {
            AttachDecision::Decline => self.send_decline(facts.handle),
            AttachDecision::Identify(identified) => self.commit_identify(facts.handle, identified),
            AttachDecision::Settle(pending) => self.schedule_settle(facts.handle, pending),
        }
    }

    fn handle_detached(&self, detached: &SessionDetached) {
        let session = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            let was_live = state.live_handles.remove(&detached.handle);
            state.named_stable_handles.remove(&detached.handle);
            state.pending_controls.remove(&detached.handle);
            state.attached_facts.remove(&detached.handle);
            state
                .pending_settle
                .retain(|_, (handle, _)| *handle != detached.handle);
            if !was_live {
                state.detached_before_attached.insert(detached.handle);
            }
            state.sessions.remove(&detached.handle)
        };
        self.scan_waiters.detach(detached.handle);
        if let Some(session) = session {
            // Synchronous and non-blocking (`CancellationToken::cancel`):
            // safe on this dispatch thread. Actually reclaiming the
            // resolver task is a separate step, on this plugin's own
            // runtime, never awaited here.
            session.cancel();
            let join = self
                .runtime_handle
                .spawn(async move { session.join_resolver().await });
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            state
                .session_detach_joins
                .retain(|lookup| !lookup.is_finished());
            state.session_detach_joins.push(join);
        }
    }

    fn handle_timer_fired(&self, fired: &TimerFired) -> Result<(), PluginError> {
        let pending = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            state.pending_settle.remove(&fired.timer)
        };
        let Some((handle, pending)) = pending else {
            // Already detached (or a stale/foreign timer id): nothing to do.
            return Ok(());
        };
        self.resolve_settled(handle, pending)
    }

    /// The final client for a settled Linux fallback, against the sibling
    /// state known right now: used both when `TIMER_FIRED` actually fires
    /// and when `set_timer` itself was refused (review #5) and this handle
    /// must still be identified immediately instead of waiting forever.
    fn resolve_settled(&self, handle: u64, pending: PendingIdentify) -> Result<(), PluginError> {
        let named_sibling_attached = {
            let state = self.state.lock().expect("plugin state mutex poisoned");
            !state.named_stable_handles.is_empty()
        };
        let client = if named_sibling_attached {
            ClientType::Stable
        } else {
            ClientType::Lazer
        };
        self.commit_identify(
            handle,
            Identified {
                client,
                settings: pending.settings,
                root: pending.root,
                executable_root: pending.executable_root,
                pid: pending.pid,
                generation: pending.generation,
                bitness: pending.bitness,
                // A settled fallback is never "named" (review #2), whether
                // it resolved to stable or lazer.
                named: false,
            },
        )
    }

    fn send_decline(&self, handle: u64) -> Result<(), PluginError> {
        match self
            .host
            .send(&Message::SessionDecline(SessionDecline { handle }), 0)
        {
            // Discovery can retire the handle before this concurrent attach
            // callback runs. No decline or session work remains in that case.
            vosu_status::VOSU_ERR_TARGET_GONE => Ok(()),
            status => status_to_result(status),
        }
    }

    fn schedule_settle(&self, handle: u64, pending: PendingIdentify) -> Result<(), PluginError> {
        let timer = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            let timer = state.next_timer;
            state.next_timer += 1;
            state.pending_settle.insert(timer, (handle, pending));
            timer
        };
        if self.host.set_timer(timer, None) == vosu_status::VOSU_OK {
            return Ok(());
        }
        // `set_timer` was refused (e.g. the 64-timer cap): remove the entry
        // just inserted and resolve immediately with the sibling state
        // known right now, rather than leaving this handle unidentified
        // forever (§6.2; review #5).
        let removed = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            state.pending_settle.remove(&timer)
        };
        match removed {
            Some((handle, pending)) => self.resolve_settled(handle, pending),
            // Raced with a detach between the insert above and here:
            // nothing left to resolve.
            None => Ok(()),
        }
    }

    /// Sends `SESSION_IDENTIFY` and, once it succeeds, records this handle
    /// as a named sibling if (and only if) `identified.named` and the
    /// handle is still live (review #2, #3), then registers this handle's
    /// session (P4; see [`register_session`]) — again only if the handle
    /// is still live, closing the same race one step later.
    ///
    /// Stable resolves and sends inline — no lazer version to look up.
    /// Lazer hands off to its own thread (see `resolve_lazer_version`):
    /// §4 says a `TIMER_FIRED`/`SESSION_ATTACHED` dispatch must hand off
    /// blocking work rather than do it inline on the host's worker pool,
    /// and the up-to-4 MiB `osu!.deps.json` read/parse, while bounded,
    /// still is blocking I/O (review #6). The spawned thread calls
    /// `finish_identify`/`register_session` itself, preserving per-handle
    /// identify ordering (nothing else ever identifies this handle again).
    fn commit_identify(&self, handle: u64, identified: Identified) -> Result<(), PluginError> {
        match identified.client {
            ClientType::Stable => register_session(
                &self.host,
                &self.state,
                &self.scan_waiters,
                &self.loader,
                &self.runtime_handle,
                handle,
                &identified,
                String::new(),
            ),
            ClientType::Lazer => {
                let host = Arc::clone(&self.host);
                let state = Arc::clone(&self.state);
                let scan_waiters = Arc::clone(&self.scan_waiters);
                let loader = Arc::clone(&self.loader);
                let runtime_handle = self.runtime_handle.clone();
                let executable_root = identified.executable_root.clone();
                let pid = identified.pid;
                // The session is installed before the host publishes identify.
                let identified_for_session = identified.clone();
                let spawned = thread::Builder::new()
                    .name("vosu-osu-lazer-version".to_owned())
                    .spawn(move || {
                        let version = resolve_lazer_version(host.as_ref(), pid, &executable_root);
                        let _ = register_session(
                            &host,
                            &state,
                            &scan_waiters,
                            &loader,
                            &runtime_handle,
                            handle,
                            &identified_for_session,
                            version,
                        );
                    });
                match spawned {
                    Ok(join_handle) => {
                        let mut state = self.state.lock().expect("plugin state mutex poisoned");
                        state.lazer_lookups.retain(|lookup| !lookup.is_finished());
                        state.lazer_lookups.push(join_handle);
                        Ok(())
                    }
                    Err(_) => {
                        // Spawning the OS thread itself failed (resource
                        // exhaustion): fall back to resolving inline rather
                        // than dropping the identify entirely.
                        let version = resolve_lazer_version(
                            self.host.as_ref(),
                            identified.pid,
                            &identified.executable_root,
                        );
                        register_session(
                            &self.host,
                            &self.state,
                            &self.scan_waiters,
                            &self.loader,
                            &self.runtime_handle,
                            handle,
                            &identified,
                            version,
                        )
                    }
                }
            }
        }
    }

    /// Test-only: blocks until every lazer-version lookup spawned so far
    /// has sent its identify and returned, so a test can assert on a fake
    /// host's recorded messages deterministically instead of racing a
    /// background thread.
    #[cfg(test)]
    fn join_background_lazer_lookups(&self) {
        let lookups = std::mem::take(
            &mut self
                .state
                .lock()
                .expect("plugin state mutex poisoned")
                .lazer_lookups,
        );
        for lookup in lookups {
            let _ = lookup.join();
        }
    }

    /// Test-only: blocks until every detach-triggered resolver join
    /// spawned so far (see `handle_detached`) has actually completed, so a
    /// test can assert a session's resolver was joined rather than merely
    /// cancelled.
    #[cfg(test)]
    fn join_background_session_detaches(&self) {
        let joins = std::mem::take(
            &mut self
                .state
                .lock()
                .expect("plugin state mutex poisoned")
                .session_detach_joins,
        );
        let runtime = self.runtime.lock().expect("plugin runtime mutex poisoned");
        if let Some(runtime) = runtime.as_ref() {
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(5), async {
                    for join in joins {
                        let _ = join.await;
                    }
                })
                .await
                .expect("session detach join deadline");
            });
        }
    }
}

impl<H: HostServices + Send + Sync + Clone + 'static, G: OffsetGenerator + 'static> Drop
    for OsuPlugin<H, G>
{
    fn drop(&mut self) {
        // Every slot `ScanWaiters` handed out is released here if a
        // session detach did not already release it (wiring item 8).
        self.scan_waiters.shutdown();

        // Reserve 500 ms of the host's grace for dispatch draining and
        // destroy-thread scheduling. Session joins and runtime shutdown
        // share only the remaining budget.
        let deadline = Instant::now() + (DESTROY_GRACE - Duration::from_millis(500));

        // `live_handles` is cleared in the very same critical section
        // that takes `sessions` (review #2): a lazer-version-lookup
        // thread that only finishes later (joined below, before anything
        // else) calls `register_session`, whose own live check now sees
        // an empty `live_handles` and registers nothing — closing the
        // race where such a lookup would otherwise hand a fresh,
        // never-cancelled session to nobody.
        let (sessions, detach_joins, lazer_lookups) = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            state.live_handles.clear();
            (
                std::mem::take(&mut state.sessions),
                std::mem::take(&mut state.session_detach_joins),
                std::mem::take(&mut state.lazer_lookups),
            )
        };
        for session in sessions.values() {
            session.cancel();
        }
        self.imports.shutdown();
        // Resource callbacks borrow the host context. Like version lookups,
        // never detach a blocked resource worker when our local grace expires;
        // the host's DestroyTimedOut path retains the whole context instead.
        for session in sessions.values() {
            if let Some(resources) = session
                .publication
                .get()
                .and_then(|p| p.bus.resources.get())
            {
                resources.join();
            }
        }

        // Join lookups first, without a timeout: they retain callable host
        // pointers, so returning while one runs could use freed host state.
        // Reads are size-capped (and O_NONBLOCK on Linux), but a stalled
        // filesystem can still hang. In that case the host's DestroyTimedOut
        // path retains the context rather than freeing it; we must not
        // bypass that protection by detaching the lookup here.
        for lookup in lazer_lookups {
            let _ = lookup.join();
        }

        // Shut every still-live session down (cancel already ran above;
        // this joins) within whatever remains of the grace, then shut
        // the runtime itself down (DG, abi:82) with whatever remains
        // after that — exactly what `destroy_concurrent` runs, via this
        // `Drop`.
        if let Some(runtime) = self
            .runtime
            .lock()
            .expect("plugin runtime mutex poisoned")
            .take()
        {
            runtime.block_on(async {
                for join in detach_joins {
                    let _ = join.await;
                }
                let _ = tokio::time::timeout(
                    deadline.saturating_duration_since(Instant::now()),
                    async {
                        for session in sessions.values() {
                            session.join_resolver().await;
                        }
                    },
                )
                .await;
            });
            runtime.shutdown_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

/// Sends `SESSION_IDENTIFY` and records a named sibling, shared by the
/// inline (stable) and spawned-thread (lazer) paths of
/// [`OsuPlugin::commit_identify`] so both apply review #2/#3's rules
/// identically. Free (not a method) so the spawned thread can call it with
/// only the `Arc`s it captured, without borrowing `OsuPlugin` itself.
fn finish_identify<H: HostServices>(
    host: &H,
    state: &Mutex<PluginState>,
    handle: u64,
    variant: &str,
    name: &str,
    version: String,
    named: bool,
) -> Result<bool, PluginError> {
    let priority = state
        .lock()
        .expect("plugin state mutex poisoned")
        .sessions
        .get(&handle)
        .is_some_and(|session| session.is_tournament_manager());
    match host.identify_session(handle, "osu", variant, name, &version, priority) {
        // The host may have removed the handle before its queued detach
        // reaches us. This is not a malformed attach, and must not register
        // a reader for an identification the host never accepted.
        vosu_status::VOSU_ERR_TARGET_GONE => return Ok(false),
        status => status_to_result(status)?,
    }
    if named {
        let mut state = state.lock().expect("plugin state mutex poisoned");
        if state.live_handles.contains(&handle) {
            state.named_stable_handles.insert(handle);
        }
    }
    Ok(true)
}

/// A [`LogWarn`] that reports through `host.log` (`VOSU_OP_LOG_RECORD`),
/// for a lazer session's resolver to report a failed attempt without
/// `vosu-osu-reader` depending on `vosu-core`'s `Logger`.
fn log_warn<H: HostServices + Send + Sync + 'static>(host: &Arc<H>) -> LogWarn {
    let host = Arc::clone(host);
    Arc::new(move |message: String| {
        let _ = host.log(LogLevel::Warn, &message, CborValue::Map(Vec::new()));
    })
}

fn log_error<H: HostServices + Send + Sync + 'static>(host: &Arc<H>) -> LogWarn {
    let host = Arc::clone(host);
    Arc::new(move |message: String| {
        let _ = host.log(LogLevel::Error, &message, CborValue::Map(Vec::new()));
    })
}

/// DF: lazer's version comes from `osu!.deps.json`. On Linux this first
/// tries the target's own filesystem view
/// (`vosu_osu_reader::memory::lazer_version::discover_for_process`, which
/// reads the attach `path` fact's directory through `/proc/<pid>/root/...`)
/// so a sandboxed/Flatpak lazer is still found exactly as 1.x did (review
/// #4); it falls back to the daemon-view `executable_root` fact itself
/// only when that lookup failed and `executable_root` is non-empty — an
/// empty `executable_root` never falls back to a relative, cwd-dependent
/// read (review #1). A read failure (missing/corrupt file, unknown root)
/// identifies with `""` rather than failing the attach (R10). Both steps
/// use only core's attach facts; neither queries the target process.
type LazerVersionError = vosu_osu_reader::memory::lazer_version::LazerVersionError;
/// Injectable process-view lazer version lookup (P3 review #1's test
/// seam): production always passes `discover_for_process`.
type DiscoverForProcess = fn(u32, &Path) -> Result<String, LazerVersionError>;
/// Injectable root-based lazer version lookup, the same seam's other
/// half: production always passes `discover_from_root`.
type DiscoverFromRoot = fn(&Path) -> Result<String, LazerVersionError>;

fn resolve_lazer_version(host: &impl HostServices, pid: u32, executable_root: &Path) -> String {
    use vosu_osu_reader::memory::lazer_version::{discover_for_process, discover_from_root};
    resolve_lazer_version_with(
        host,
        pid,
        executable_root,
        discover_for_process,
        discover_from_root,
    )
}

/// Like [`resolve_lazer_version`], with both discovery steps injectable
/// (P3 review #1): a test proves the empty-`executable_root` guard itself
/// — not just that the (also empty, cwd-dependent) real result happens to
/// match — by injecting a process lookup that always fails and a root
/// lookup that panics if it is ever called with an empty path.
fn resolve_lazer_version_with(
    host: &impl HostServices,
    pid: u32,
    executable_root: &Path,
    discover_for_process: DiscoverForProcess,
    discover_from_root: DiscoverFromRoot,
) -> String {
    let result = discover_for_process(pid, executable_root).or_else(|process_error| {
        if executable_root.as_os_str().is_empty() {
            Err(process_error)
        } else {
            discover_from_root(executable_root)
        }
    });
    match result {
        Ok(version) => version,
        Err(error) => {
            let _ = host.log(
                LogLevel::Warn,
                "osu!lazer version discovery failed; identifying without a version",
                CborValue::Map(vec![(
                    CborValue::Text("error".to_owned()),
                    CborValue::Text(error.to_string()),
                )]),
            );
            String::new()
        }
    }
}

impl ConcurrentPlugin for OsuPlugin<VtableHostServices> {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let plugin = Self::new(host, current_platform(), init.data_dir);
        plugin.update_settings(&init.settings)?;
        Ok(plugin)
    }

    fn on_message(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        self.dispatch(message, correlation_id)
    }
}

fn status_to_result(status: vosu_status) -> Result<(), PluginError> {
    match status {
        vosu_status::VOSU_OK => Ok(()),
        status => Err(status.into()),
    }
}

#[cfg(windows)]
fn current_platform() -> Platform {
    Platform::Windows
}

#[cfg(target_os = "linux")]
fn current_platform() -> Platform {
    Platform::Linux
}

#[cfg(test)]
mod tests;

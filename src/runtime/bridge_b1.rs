//! Interim root-side B1 bridge; removed with the 1.x surfaces in M4a.
use super::*;
use vosu_osu::bridge_b1::{B1Event, B1Inputs, B1Phase};
use vosu_osu::capture::equivalent_state_snapshots;
use vosu_osu_model::launch::InstanceSettings;
use vosu_surface::LegacySink;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusState {
    pub focused_client: ClientType,
    pub game_focused: bool,
}

impl Default for FocusState {
    fn default() -> Self {
        Self {
            focused_client: ClientType::Stable,
            game_focused: false,
        }
    }
}

/// One worker's API-visible and live state. Both are carried on every
/// publication so the latest-only queue never drops an API update.
pub(super) struct StatePublication {
    pub(super) pid: u32,
    pub(super) generation: u64,
    pub(super) api: Arc<StateBundleSnapshot>,
    pub(super) live: Arc<StateBundleSnapshot>,
}

#[derive(Default)]
struct StatePublicationQueueState {
    closed: bool,
    pending: BTreeMap<u32, StatePublication>,
}

#[derive(Default)]
pub(super) struct StatePublicationQueue {
    state: Mutex<StatePublicationQueueState>,
    notified: Arc<tokio::sync::Notify>,
}

impl StatePublicationQueue {
    pub(super) fn publish(&self, publication: StatePublication) -> Result<(), StateWorkerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err(StateWorkerError::Publish("state supervisor stopped".into()));
        }

        let should_replace = state
            .pending
            .get(&publication.pid)
            .is_none_or(|pending| pending.generation <= publication.generation);
        if should_replace {
            let should_notify = state.pending.is_empty();
            state.pending.insert(publication.pid, publication);
            drop(state);
            if should_notify {
                self.notified.notify_one();
            }
        }
        Ok(())
    }

    pub(super) fn drain(&self) -> BTreeMap<u32, StatePublication> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut state.pending)
    }

    pub(super) fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.pending.clear();
    }
}

/// One manager publication target: API consumers or the live selected-field
/// stream. Each keeps its own per-instance states and semantic revisions.
#[derive(Default)]
pub(super) struct ManagerView {
    states: BTreeMap<u32, Arc<StateBundleSnapshot>>,
    state_revisions: BTreeMap<u32, u64>,
    revisions: SemanticRevision,
}

impl ManagerView {
    pub(super) fn retain(&mut self, desired: &BTreeMap<u32, u64>) {
        self.states
            .retain(|pid, state| desired.get(pid).copied() == Some(state.generation));
        self.state_revisions
            .retain(|pid, _| desired.contains_key(pid));
    }

    pub(super) fn update(&mut self, pid: u32, snapshot: Arc<StateBundleSnapshot>) {
        update_state_publication(&mut self.states, &mut self.state_revisions, pid, snapshot);
    }

    pub(super) fn manager(
        &mut self,
        lifecycle: &B1Lifecycle,
        config: &vosu_core::config::Config,
    ) -> Option<Arc<ManagerSnapshot>> {
        build_manager_snapshot(
            &mut self.revisions,
            lifecycle,
            config,
            &self.states,
            &self.state_revisions,
        )
    }
}

pub(super) fn update_state_publication(
    states: &mut BTreeMap<u32, Arc<StateBundleSnapshot>>,
    state_revisions: &mut BTreeMap<u32, u64>,
    pid: u32,
    snapshot: Arc<StateBundleSnapshot>,
) {
    let unchanged = states
        .get(&pid)
        .is_some_and(|current| equivalent_state_snapshots(current, &snapshot));
    if unchanged {
        return;
    }
    states.insert(pid, snapshot);
    let revision = state_revisions.entry(pid).or_default();
    *revision = revision.saturating_add(1);
}

pub(super) fn build_manager_snapshot(
    revisions: &mut SemanticRevision,
    lifecycle: &B1Lifecycle,
    config: &vosu_core::config::Config,
    states: &BTreeMap<u32, Arc<StateBundleSnapshot>>,
    state_revisions: &BTreeMap<u32, u64>,
) -> Option<Arc<ManagerSnapshot>> {
    let ready = lifecycle
        .sessions
        .iter()
        .filter_map(|entry| {
            states
                .get(&entry.pid)
                .filter(|state| state.generation == entry.generation)
                .map(|state| (entry, state))
        })
        .collect::<Vec<_>>();
    let focused_pid = ready
        .iter()
        .find(|(entry, _)| entry.is_tournament_manager)
        .or_else(|| {
            ready
                .iter()
                .find(|(entry, _)| entry.client == lifecycle.focus.focused_client)
        })
        .map(|(entry, _)| entry.pid);
    let semantics = ManagerSemantics {
        focused_pid,
        game_focused: lifecycle.focus.game_focused,
        enable_key_overlay: config.enable_key_overlay,
        enable_mania_key_hit_errors: config.enable_mania_key_hit_errors,
        enable_hit_offsets: config.enable_hit_offsets,
        instances: ready
            .iter()
            .map(|(entry, _)| {
                (
                    entry.pid,
                    state_revisions.get(&entry.pid).copied().unwrap_or_default(),
                )
            })
            .collect(),
    };
    let (revision, changed) = revisions.observe_with_change(semantics);
    if !changed {
        return None;
    }
    let entries = ready.into_iter().map(|(entry, state)| {
        ManagerInstanceSnapshot::new(
            entry.pid,
            entry.settings.clone(),
            entry.is_tournament_manager,
            Arc::clone(state),
        )
    });
    Some(Arc::new(ManagerSnapshot::new(
        revision,
        focused_pid,
        lifecycle.focus.game_focused,
        ApiRuntimeSettings {
            enable_key_overlay: config.enable_key_overlay,
            enable_mania_key_hit_errors: config.enable_mania_key_hit_errors,
            enable_hit_offsets: config.enable_hit_offsets,
        },
        entries,
    )))
}
/// Advances only when API-observable manager semantics change. In particular,
/// every instance generation participates, not only the focused PID.
#[derive(Default)]
pub(super) struct SemanticRevision {
    pub(super) revision: u64,
    pub(super) previous: Option<ManagerSemantics>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ManagerSemantics {
    pub(super) focused_pid: Option<u32>,
    pub(super) game_focused: bool,
    pub(super) enable_key_overlay: bool,
    pub(super) enable_mania_key_hit_errors: bool,
    pub(super) enable_hit_offsets: bool,
    pub(super) instances: BTreeMap<u32, u64>,
}

impl SemanticRevision {
    fn observe_with_change(&mut self, semantics: ManagerSemantics) -> (u64, bool) {
        let changed = self.previous.as_ref() != Some(&semantics);
        if changed {
            self.revision = self.revision.saturating_add(1);
            self.previous = Some(semantics);
        }
        (self.revision, changed)
    }

    #[cfg(test)]
    pub(super) fn observe(&mut self, semantics: ManagerSemantics) -> u64 {
        self.observe_with_change(semantics).0
    }
}

/// Identical 1.x focus reduction over PID-ordered session facts.
pub(crate) fn reduce_foreground(
    instances: impl IntoIterator<Item = (u32, ClientType, bool)>,
    foreground_pid: Option<u32>,
) -> FocusState {
    let mut first = None;
    let mut manager = None;
    for (pid, client, is_manager) in instances {
        if Some(pid) == foreground_pid {
            return FocusState {
                focused_client: client,
                game_focused: true,
            };
        }
        first.get_or_insert(client);
        if is_manager {
            manager.get_or_insert(client);
        }
    }
    FocusState {
        focused_client: manager.or(first).unwrap_or(ClientType::Stable),
        game_focused: false,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct B1SessionView {
    pub pid: u32,
    pub generation: u64,
    pub client: ClientType,
    pub settings: InstanceSettings,
    pub phase: B1Phase,
    pub is_tournament_manager: bool,
}
#[derive(Debug, Clone, Default)]
pub(super) struct B1Lifecycle {
    pub revision: u64,
    pub sessions: Arc<[B1SessionView]>,
    pub focus: FocusState,
}

#[derive(Default)]
struct BridgeState {
    closed: bool,
    sessions: BTreeMap<u32, B1SessionView>,
    foreground: Option<u32>,
}

pub(super) struct LegacyBridge {
    // Lock order: state -> publications; no plugin callbacks under either lock.
    state: Mutex<BridgeState>,
    publications: StatePublicationQueue,
    lifecycle: watch::Sender<Arc<B1Lifecycle>>,
    inputs: watch::Sender<B1Inputs>,
    config: watch::Receiver<Arc<Config>>,
    demand: watch::Receiver<vosu_osu_state::SubscriptionDemand>,
    snapshots: Arc<SnapshotProviders>,
    logger: Logger,
}

impl LegacyBridge {
    pub(super) fn new(
        config: watch::Receiver<Arc<Config>>,
        demand: watch::Receiver<vosu_osu_state::SubscriptionDemand>,
        snapshots: Arc<SnapshotProviders>,
        logger: Logger,
    ) -> Arc<Self> {
        let (inputs, _) = watch::channel(bridge_inputs(&config.borrow(), *demand.borrow()));
        Arc::new(Self {
            state: Mutex::default(),
            publications: StatePublicationQueue::default(),
            lifecycle: watch::channel(Arc::default()).0,
            inputs,
            config,
            demand,
            snapshots,
            logger,
        })
    }
    pub(super) fn lifecycle(&self) -> watch::Receiver<Arc<B1Lifecycle>> {
        self.lifecycle.subscribe()
    }
    fn publish_lifecycle(&self, state: &BridgeState) {
        let focus = reduce_foreground(
            state
                .sessions
                .values()
                .map(|s| (s.pid, s.client, s.is_tournament_manager)),
            state.foreground,
        );
        self.lifecycle.send_if_modified(|current| {
            if current.focus == focus && current.sessions.iter().eq(state.sessions.values()) {
                return false;
            }
            *current = Arc::new(B1Lifecycle {
                revision: current.revision.saturating_add(1),
                sessions: state.sessions.values().cloned().collect(),
                focus,
            });
            true
        });
    }
    pub(super) fn start(
        self: &Arc<Self>,
        mut foreground: watch::Receiver<Option<u32>>,
    ) -> LegacyBridgeTask {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let bridge = Arc::downgrade(self);
        let mut config = self.config.clone();
        let mut demand = self.demand.clone();
        let mut lifecycle = self.lifecycle();
        let task = tokio::spawn(async move {
            let mut api = ManagerView::default();
            let mut live = ManagerView::default();
            loop {
                let Some(owner) = bridge.upgrade() else {
                    break;
                };
                // All manager inputs are observed under the lifecycle lock, so detach
                // cannot race a stale queued publication back into the manager.
                {
                    let mut state = owner
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let pid = *foreground.borrow_and_update();
                    if state.foreground != pid {
                        state.foreground = pid;
                        owner.publish_lifecycle(&state);
                    }
                    let current = owner.lifecycle.borrow();
                    let desired = state
                        .sessions
                        .values()
                        .map(|s| (s.pid, s.generation))
                        .collect();
                    api.retain(&desired);
                    live.retain(&desired);
                    for (pid, publication) in owner.publications.drain() {
                        if desired.get(&pid) == Some(&publication.generation) {
                            api.update(pid, publication.api);
                            live.update(pid, publication.live);
                        }
                    }
                    let config = config.borrow();
                    if let Some(manager) = api.manager(&current, &config) {
                        owner.snapshots.publish_api(manager);
                    }
                    if let Some(manager) = live.manager(&current, &config) {
                        owner.snapshots.publish_live(manager);
                    }
                }
                owner.inputs.send_if_modified(|inputs| {
                    let next = bridge_inputs(&config.borrow(), *demand.borrow());
                    if *inputs == next {
                        false
                    } else {
                        *inputs = next;
                        true
                    }
                });
                owner.snapshots.set_poll_rates(
                    poll_duration(config.borrow().poll_rate),
                    poll_duration(config.borrow().precise_data_poll_rate),
                );
                // Waiting owns only the notifier; lifecycle closure also wakes
                // the task when the last bridge owner is dropped.
                let notification = Arc::clone(&owner.publications.notified);
                let notified = notification.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                drop(owner);
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    changed = config.changed() => if changed.is_err() { break; },
                    changed = demand.changed() => if changed.is_err() { break; },
                    changed = lifecycle.changed() => if changed.is_err() { break; },
                    _ = &mut notified => {},
                    changed = foreground.changed() => if changed.is_err() { break; },
                }
            }
            if let Some(owner) = bridge.upgrade() {
                owner.close();
            }
        });
        LegacyBridgeTask { cancel, task }
    }
    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closed = true;
        state.sessions.clear();
        self.publications.close();
        self.publish_lifecycle(&state);
    }
}
fn bridge_inputs(config: &Config, demand: vosu_osu_state::SubscriptionDemand) -> B1Inputs {
    B1Inputs {
        enable_key_overlay: config.enable_key_overlay,
        enable_mania_key_hit_errors: config.enable_mania_key_hit_errors,
        enable_hit_offsets: config.enable_hit_offsets,
        v2_subscription_debounce: config.v2_subscription_debounce,
        demand,
    }
}
impl LegacySink<B1Event, B1Inputs> for LegacyBridge {
    fn inputs(&self) -> watch::Receiver<B1Inputs> {
        self.inputs.subscribe()
    }
    fn send(&self, event: B1Event) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return;
        }
        match event {
            B1Event::Attached {
                key,
                client,
                settings,
                is_tournament_manager,
            } => {
                if state
                    .sessions
                    .get(&key.pid)
                    .is_some_and(|s| s.generation >= key.generation)
                {
                    return;
                }
                state.sessions.insert(
                    key.pid,
                    B1SessionView {
                        pid: key.pid,
                        generation: key.generation,
                        client,
                        settings,
                        is_tournament_manager,
                        phase: B1Phase::ResolvingOffsets,
                    },
                );
                self.logger.info(format!(
                    "Found osu! process PID {} ({client:?}); probing memory",
                    key.pid
                ));
            }
            B1Event::Phase { key, phase } => {
                let Some(session) = state
                    .sessions
                    .get_mut(&key.pid)
                    .filter(|s| s.generation == key.generation)
                else {
                    return;
                };
                session.phase = phase;
            }
            B1Event::Publish { key, api, live } => {
                if state
                    .sessions
                    .get(&key.pid)
                    .is_none_or(|s| s.generation != key.generation)
                    || api.generation != key.generation
                    || live.generation != key.generation
                {
                    return;
                }
                let _ = self.publications.publish(StatePublication {
                    pid: key.pid,
                    generation: key.generation,
                    api,
                    live,
                });
                return;
            }
            B1Event::Detached { key } => {
                if state
                    .sessions
                    .get(&key.pid)
                    .is_none_or(|s| s.generation != key.generation)
                {
                    return;
                }
                state.sessions.remove(&key.pid);
                self.publications
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pending
                    .remove(&key.pid);
            }
        }
        self.publish_lifecycle(&state);
    }
}
pub(super) struct LegacyBridgeTask {
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
impl LegacyBridgeTask {
    pub(super) async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.task.await;
    }
}

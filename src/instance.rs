//! Process discovery, client classification, and deterministic instance selection.

use arc_swap::ArcSwapOption;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use crate::app::StartupArgs;
use crate::memory::lazer_loader::{
    LazerLoadError, LazerOffsetLoader, OffsetFetcher, resolve_for_instance,
};
use crate::memory::lazer_offsets::LazerOffsets;
use crate::model::enums::ClientType;
use crate::process::{MemoryError, ProcessHandle};

pub type Pid = u32;
type LazerResolverHandle = JoinHandle<Result<Arc<LazerOffsets>, LazerLoadError>>;

pub const EMPTY_DISCOVERY_INTERVAL: Duration = Duration::from_millis(100);
pub const ACTIVE_DISCOVERY_INTERVAL: Duration = Duration::from_secs(1);
pub const WINDOWS_FOCUS_INTERVAL: Duration = Duration::from_millis(100);

mod discovery;

#[cfg(any(target_os = "linux", windows))]
pub use discovery::NativeProcessProvider;
pub use discovery::{
    ClassifiedCandidate, DiscoveryPlatform, ProcessDescriptor, ProcessProvider,
    classify_candidates, should_skip,
};

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSettings {
    pub tournament_spectator: bool,
    pub tournament_ipc_id: Option<f64>,
    pub devserver_endpoint: Option<OsString>,
}

impl InstanceSettings {
    fn from_arguments(arguments: &StartupArgs) -> Self {
        let tournament_ipc_id = arguments
            .get("spectateclient")
            .and_then(|value| value.as_number());
        let devserver_endpoint = arguments
            .get("devserver")
            .filter(|value| value.is_truthy())
            .and_then(|value| value.as_os_str())
            .filter(|value| !value.is_empty())
            .map(OsString::from);
        Self {
            tournament_spectator: tournament_ipc_id.is_some(),
            tournament_ipc_id,
            devserver_endpoint,
        }
    }
}

#[derive(Debug)]
pub struct Instance {
    pid: Pid,
    generation: u64,
    client: ClientType,
    process: ProcessHandle,
    root: PathBuf,
    settings: InstanceSettings,
    tournament_manager: AtomicBool,
    lazer_offsets: ArcSwapOption<LazerOffsets>,
    lifecycle_cancel: CancellationToken,
    resolver_task: tokio::sync::Mutex<Option<LazerResolverHandle>>,
    lifecycle_changed: Arc<Notify>,
}

impl Instance {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn pid(&self) -> Pid {
        self.pid
    }

    #[must_use]
    pub fn client(&self) -> ClientType {
        self.client
    }

    #[must_use]
    pub fn process(&self) -> &ProcessHandle {
        &self.process
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn settings(&self) -> &InstanceSettings {
        &self.settings
    }

    #[must_use]
    pub fn is_tournament_manager(&self) -> bool {
        self.tournament_manager.load(Ordering::Acquire)
    }

    pub fn set_tournament_manager(&self, value: bool) {
        self.tournament_manager.store(value, Ordering::Release);
    }

    #[must_use]
    pub fn lazer_offsets(&self) -> Option<Arc<LazerOffsets>> {
        self.lazer_offsets.load_full()
    }

    /// Publishes validated immutable offsets exactly once for this instance.
    pub fn install_lazer_offsets(&self, offsets: Arc<LazerOffsets>) -> bool {
        if self.client != ClientType::Lazer
            || self.lifecycle_cancel.is_cancelled()
            || !self.process.is_alive()
        {
            return false;
        }
        let installed = self
            .lazer_offsets
            .compare_and_swap(&None::<Arc<LazerOffsets>>, Some(offsets))
            .is_none();
        if installed {
            self.lifecycle_changed.notify_one();
        }
        installed
    }

    #[must_use]
    pub fn lifecycle_token(&self) -> CancellationToken {
        self.lifecycle_cancel.child_token()
    }

    /// Starts at most one pending offset resolver for this process generation.
    pub async fn start_lazer_resolver<F>(
        self: &Arc<Self>,
        loader: Arc<LazerOffsetLoader<F>>,
    ) -> bool
    where
        F: OffsetFetcher + 'static,
    {
        if self.client != ClientType::Lazer || self.lifecycle_cancel.is_cancelled() {
            return false;
        }
        let mut task = self.resolver_task.lock().await;
        if task.is_some() || self.lazer_offsets().is_some() {
            return false;
        }
        let instance = self.clone();
        *task = Some(tokio::spawn(async move {
            resolve_for_instance(loader, instance).await
        }));
        true
    }

    /// Cancels and joins the resolver before the process generation is released.
    pub async fn shutdown_lazer_resolver(&self) {
        self.lifecycle_cancel.cancel();
        let task = self.resolver_task.lock().await.take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    pub fn cancel(&self) {
        self.lifecycle_cancel.cancel();
        if let Ok(mut task) = self.resolver_task.try_lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

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

#[derive(Debug, Clone)]
pub struct InstanceGeneration {
    pub generation: u64,
    pub pid: Pid,
    pub client: ClientType,
    pub settings: InstanceSettings,
    pub offset_ready: bool,
    pub instance: Arc<Instance>,
}

impl InstanceGeneration {
    fn semantically_eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.pid == other.pid
            && self.client == other.client
            && self.settings == other.settings
            && self.offset_ready == other.offset_ready
    }
}

#[derive(Debug, Clone)]
pub struct InstanceLifecycleSnapshot {
    pub revision: u64,
    pub instances: Arc<[InstanceGeneration]>,
    pub focus: FocusState,
}

impl InstanceLifecycleSnapshot {
    fn semantically_eq(&self, other: &Self) -> bool {
        self.focus == other.focus
            && self.instances.len() == other.instances.len()
            && self
                .instances
                .iter()
                .zip(other.instances.iter())
                .all(|(left, right)| left.semantically_eq(right))
    }
}

impl Default for InstanceLifecycleSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            instances: Arc::from([]),
            focus: FocusState::default(),
        }
    }
}

#[derive(Debug)]
pub struct InstanceManager {
    instances: BTreeMap<Pid, Arc<Instance>>,
    focus: FocusState,
    next_generation: u64,
    lifecycle_changed: Arc<Notify>,
}

impl Default for InstanceManager {
    fn default() -> Self {
        Self {
            instances: BTreeMap::new(),
            focus: FocusState::default(),
            next_generation: 0,
            lifecycle_changed: Arc::new(Notify::new()),
        }
    }
}

impl InstanceManager {
    #[must_use]
    pub fn instances(&self) -> &BTreeMap<Pid, Arc<Instance>> {
        &self.instances
    }

    #[must_use]
    pub fn focus(&self) -> FocusState {
        self.focus
    }

    #[must_use]
    pub fn discovery_interval(&self) -> Duration {
        if self.instances.is_empty() {
            EMPTY_DISCOVERY_INTERVAL
        } else {
            ACTIVE_DISCOVERY_INTERVAL
        }
    }

    /// Discovers candidates once. A successfully validated native handle is moved into
    /// the instance; failed/skipped probes are dropped immediately.
    pub fn discover(&mut self, provider: &impl ProcessProvider) -> Result<usize, MemoryError> {
        let processes = provider.enumerate()?;
        let mut candidates = classify_candidates(provider.platform(), &processes);
        candidates.sort_unstable_by_key(|candidate| candidate.pid);
        let mut inserted = 0;
        for candidate in candidates {
            if self.instances.contains_key(&candidate.pid) {
                continue;
            }
            let descriptor = &processes[candidate.index];
            if should_skip(&descriptor.arguments) {
                continue;
            }
            let process = match provider.open(candidate.pid) {
                Ok(process) => process,
                Err(_) => continue,
            };
            if !process.is_alive() {
                continue;
            }
            if process.process_id().is_some_and(|pid| pid != candidate.pid) {
                continue;
            }
            self.next_generation = self
                .next_generation
                .checked_add(1)
                .expect("instance generation exhausted");
            let instance = Arc::new(Instance {
                pid: candidate.pid,
                generation: self.next_generation,
                client: candidate.client,
                process,
                root: descriptor.root.clone(),
                settings: InstanceSettings::from_arguments(&descriptor.arguments),
                tournament_manager: AtomicBool::new(false),
                lazer_offsets: ArcSwapOption::empty(),
                lifecycle_cancel: CancellationToken::new(),
                resolver_task: tokio::sync::Mutex::new(None),
                lifecycle_changed: self.lifecycle_changed.clone(),
            });
            self.instances.insert(candidate.pid, instance);
            inserted += 1;
        }
        Ok(inserted)
    }

    /// Production discovery path: every newly observed lazer generation gets
    /// exactly one lifecycle-owned resolver task.
    pub async fn discover_with_lazer_loader<F>(
        &mut self,
        provider: &impl ProcessProvider,
        loader: Arc<LazerOffsetLoader<F>>,
    ) -> Result<usize, MemoryError>
    where
        F: OffsetFetcher + 'static,
    {
        let inserted = self.discover(provider)?;
        for instance in self.instances.values() {
            if instance.client() == ClientType::Lazer && instance.lazer_offsets().is_none() {
                instance.start_lazer_resolver(loader.clone()).await;
            }
        }
        Ok(inserted)
    }

    pub fn insert(&mut self, instance: Arc<Instance>) -> bool {
        if self.instances.contains_key(&instance.pid) {
            return false;
        }
        self.instances.insert(instance.pid, instance);
        true
    }

    pub fn remove(&mut self, pid: Pid) -> Option<Arc<Instance>> {
        let instance = self.instances.remove(&pid)?;
        instance.cancel();
        Some(instance)
    }

    pub async fn remove_and_join(&mut self, pid: Pid) -> Option<Arc<Instance>> {
        let instance = self.instances.remove(&pid)?;
        instance.shutdown_lazer_resolver().await;
        Some(instance)
    }

    /// Removes dead instances exactly once and returns their PIDs in ascending order.
    pub fn remove_dead(&mut self) -> Vec<Pid> {
        let dead: Vec<_> = self
            .instances
            .iter()
            .filter_map(|(pid, instance)| (!instance.process.is_alive()).then_some(*pid))
            .collect();
        for pid in &dead {
            if let Some(instance) = self.instances.remove(pid) {
                instance.cancel();
            }
        }
        dead
    }

    pub async fn remove_dead_and_join(&mut self) -> Vec<Pid> {
        let dead: Vec<_> = self
            .instances
            .iter()
            .filter_map(|(pid, instance)| (!instance.process.is_alive()).then_some(*pid))
            .collect();
        for pid in &dead {
            if let Some(instance) = self.instances.remove(pid) {
                instance.shutdown_lazer_resolver().await;
            }
        }
        dead
    }

    pub async fn shutdown(&mut self) {
        let instances = std::mem::take(&mut self.instances);
        for instance in instances.into_values() {
            instance.shutdown_lazer_resolver().await;
        }
    }

    /// Tournament manager wins; otherwise the lowest matching PID wins. Without
    /// a client filter, the first PID is the source fallback.
    #[must_use]
    pub fn select(&self, client: Option<ClientType>) -> Option<Arc<Instance>> {
        self.instances
            .values()
            .find(|instance| {
                instance.is_tournament_manager()
                    && client.is_none_or(|client| instance.client == client)
            })
            .cloned()
            .or_else(|| {
                self.instances
                    .values()
                    .find(|instance| client.is_none_or(|client| instance.client == client))
                    .cloned()
            })
    }

    pub fn poll_focus(&mut self, provider: &impl ProcessProvider) -> FocusState {
        let foreground_pid = if provider.supports_focus() {
            provider.foreground_pid()
        } else {
            None
        };
        self.focus = reduce_foreground(&self.instances, foreground_pid);
        self.focus
    }
    fn take_dead(&mut self) -> Vec<Arc<Instance>> {
        let dead: Vec<_> = self
            .instances
            .iter()
            .filter_map(|(pid, instance)| (!instance.process.is_alive()).then_some(*pid))
            .collect();
        dead.into_iter()
            .filter_map(|pid| self.instances.remove(&pid))
            .collect()
    }

    fn take_all(&mut self) -> Vec<Arc<Instance>> {
        self.focus = FocusState::default();
        std::mem::take(&mut self.instances).into_values().collect()
    }
}

async fn publish_lifecycle(
    manager: &tokio::sync::RwLock<InstanceManager>,
    sender: &watch::Sender<Arc<InstanceLifecycleSnapshot>>,
) {
    let mut next = {
        let manager = manager.read().await;
        InstanceLifecycleSnapshot {
            revision: 0,
            instances: manager
                .instances
                .values()
                .map(|instance| InstanceGeneration {
                    generation: instance.generation(),
                    pid: instance.pid(),
                    client: instance.client(),
                    settings: instance.settings().clone(),
                    offset_ready: instance.lazer_offsets().is_some(),
                    instance: instance.clone(),
                })
                .collect::<Vec<_>>()
                .into(),
            focus: manager.focus,
        }
    };
    sender.send_if_modified(|current| {
        if current.semantically_eq(&next) {
            return false;
        }
        next.revision = current
            .revision
            .checked_add(1)
            .expect("instance lifecycle revision exhausted");
        *current = Arc::new(next);
        true
    });
}

/// Owns native discovery and every per-instance resolver for the daemon runtime.
pub struct InstanceRuntime<P, F> {
    manager: Arc<tokio::sync::RwLock<InstanceManager>>,
    lifecycle: watch::Receiver<Arc<InstanceLifecycleSnapshot>>,
    cancel: CancellationToken,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    _provider: Arc<P>,
    _loader: Arc<LazerOffsetLoader<F>>,
}

impl<P, F> InstanceRuntime<P, F>
where
    P: ProcessProvider + 'static,
    F: OffsetFetcher + 'static,
{
    pub fn start(provider: P, loader: LazerOffsetLoader<F>) -> Self {
        let manager_value = InstanceManager::default();
        let lifecycle_changed = manager_value.lifecycle_changed.clone();
        let manager = Arc::new(tokio::sync::RwLock::new(manager_value));
        let (lifecycle_sender, lifecycle) =
            watch::channel(Arc::new(InstanceLifecycleSnapshot::default()));
        let cancel = CancellationToken::new();
        let provider = Arc::new(provider);
        let loader = Arc::new(loader);
        let loop_manager = manager.clone();
        let loop_cancel = cancel.clone();
        let loop_provider = provider.clone();
        let loop_loader = loader.clone();
        let task = tokio::spawn(async move {
            loop {
                if loop_cancel.is_cancelled() {
                    break;
                }
                let (interval, removed, pending_resolvers) = {
                    let mut manager = loop_manager.write().await;
                    let _ = manager.discover(loop_provider.as_ref());
                    let removed = manager.take_dead();
                    manager.poll_focus(loop_provider.as_ref());
                    let pending_resolvers = manager
                        .instances
                        .values()
                        .filter(|instance| {
                            instance.client() == ClientType::Lazer
                                && instance.lazer_offsets().is_none()
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    (manager.discovery_interval(), removed, pending_resolvers)
                };
                for instance in removed {
                    instance.shutdown_lazer_resolver().await;
                }
                // Publish Add before a fast resolver can publish Ready.
                publish_lifecycle(&loop_manager, &lifecycle_sender).await;
                for instance in pending_resolvers {
                    instance.start_lazer_resolver(loop_loader.clone()).await;
                }
                tokio::select! {
                    () = loop_cancel.cancelled() => break,
                    () = lifecycle_changed.notified() => {}
                    () = tokio::time::sleep(interval) => {}
                }
            }

            let removed = {
                let mut manager = loop_manager.write().await;
                manager.take_all()
            };
            for instance in removed {
                instance.shutdown_lazer_resolver().await;
            }
            publish_lifecycle(&loop_manager, &lifecycle_sender).await;
        });
        Self {
            manager,
            lifecycle,
            cancel,
            task: tokio::sync::Mutex::new(Some(task)),
            _provider: provider,
            _loader: loader,
        }
    }

    #[must_use]
    pub fn manager(&self) -> Arc<tokio::sync::RwLock<InstanceManager>> {
        self.manager.clone()
    }

    /// Atomically subscribes to the current immutable lifecycle snapshot.
    #[must_use]
    pub fn subscribe_current(&self) -> watch::Receiver<Arc<InstanceLifecycleSnapshot>> {
        self.lifecycle.clone()
    }

    /// Cancels and joins discovery and all generation-owned resolver tasks.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(task) = self.task.lock().await.take() {
            let _ = task.await;
        }
    }
}

#[must_use]
pub fn reduce_foreground(
    instances: &BTreeMap<Pid, Arc<Instance>>,
    foreground_pid: Option<Pid>,
) -> FocusState {
    if let Some(instance) = foreground_pid.and_then(|pid| instances.get(&pid)) {
        return FocusState {
            focused_client: instance.client,
            game_focused: true,
        };
    }
    FocusState {
        focused_client: instances
            .values()
            .find(|instance| instance.is_tournament_manager())
            .or_else(|| instances.first_key_value().map(|(_, instance)| instance))
            .map_or(ClientType::Stable, |instance| instance.client),
        game_focused: false,
    }
}

#[cfg(test)]
mod tests;

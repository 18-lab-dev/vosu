//! Immutable publication and encoding-cache boundary for all public state APIs.
//!
//! Wire builders receive one [`CombinedSnapshot`]. Focus, lifecycle metadata,
//! all instance states, and the focused regular/precise components therefore
//! always come from one atomic publication point.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex, OnceLock};

use arc_swap::ArcSwap;
use bytes::Bytes;

use crate::instance::{InstanceSettings, Pid};
use crate::model::enums::ClientType;
use crate::state::{PreciseState, RegularState, StateBundleSnapshot};

/// A concrete, non-stringly-typed public wire schema.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ApiSchema {
    /// The legacy gosuMemory `/json` schema.
    V1,
    /// The vosu `/json/v2` schema.
    V2,
    /// The streamCompanion `/json/sc` schema.
    StreamCompanion,
    /// The low-latency `/json/v2/precise` schema.
    V2Precise,
}

impl ApiSchema {
    /// Every schema, in stable endpoint order.
    pub const ALL: [Self; 4] = [Self::V1, Self::V2, Self::StreamCompanion, Self::V2Precise];

    /// State components which may affect this schema's unfiltered bytes.
    pub const fn dependency(self) -> SchemaDependency {
        match self {
            // Every builder needs manager focus/readiness and tournament data.
            // Regular schemas expose precise keys/hit errors; V2Precise derives
            // tournament clients from the regular all-instance snapshots.
            Self::V1 | Self::V2 | Self::StreamCompanion | Self::V2Precise => SchemaDependency::ALL,
        }
    }
}

/// Immutable publication components consulted by a schema builder.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SchemaDependency {
    manager: bool,
    regular: bool,
    precise: bool,
}

impl SchemaDependency {
    /// Manager metadata plus focused regular and precise state.
    pub const ALL: Self = Self {
        manager: true,
        regular: true,
        precise: true,
    };

    /// Whether manager focus/lifecycle/all-instance changes invalidate a cache.
    pub const fn includes_manager(self) -> bool {
        self.manager
    }

    /// Whether a new focused regular component invalidates a cache.
    pub const fn includes_regular(self) -> bool {
        self.regular
    }

    /// Whether a new focused precise component invalidates a cache.
    pub const fn includes_precise(self) -> bool {
        self.precise
    }
}

/// A deterministic API build or encoding failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiError {
    /// No focused/runnable osu! instance exists at this publication point.
    NotReady,
    /// A strongly typed wire value could not be serialized.
    Serialization(Arc<str>),
}

impl ApiError {
    /// Creates a serialization error with cheaply cloneable shared context.
    pub fn serialization(message: impl Into<Arc<str>>) -> Self {
        Self::Serialization(message.into())
    }
}

impl Display for ApiError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => formatter.write_str("osu is not ready/running"),
            Self::Serialization(message) => {
                write!(formatter, "API serialization failed: {message}")
            }
        }
    }
}

impl Error for ApiError {}

/// Immutable lifecycle/role metadata paired with one instance state bundle.
#[derive(Clone, Debug, PartialEq)]
pub struct ManagerInstanceSnapshot {
    pid: Pid,
    settings: InstanceSettings,
    tournament_manager: bool,
    state: Arc<StateBundleSnapshot>,
}

impl ManagerInstanceSnapshot {
    /// Captures the API-visible metadata of one discovered instance.
    pub fn new(
        pid: Pid,
        settings: InstanceSettings,
        tournament_manager: bool,
        state: Arc<StateBundleSnapshot>,
    ) -> Self {
        Self {
            pid,
            settings,
            tournament_manager,
            state,
        }
    }

    /// Native process identifier used for deterministic ordering.
    pub const fn pid(&self) -> Pid {
        self.pid
    }

    /// Stable or lazer classification derived from the state source.
    pub fn client(&self) -> ClientType {
        self.state.source
    }

    /// Source-compatible command-line-derived instance settings.
    pub fn settings(&self) -> &InstanceSettings {
        &self.settings
    }

    /// Whether this instance is the selected stable tournament manager.
    pub const fn is_tournament_manager(&self) -> bool {
        self.tournament_manager
    }

    /// Immutable state bundle for this process generation.
    pub fn state(&self) -> &Arc<StateBundleSnapshot> {
        &self.state
    }
}

/// API-affecting runtime configuration captured with each publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApiRuntimeSettings {
    /// Whether streamCompanion key-overlay fields are populated.
    pub enable_key_overlay: bool,
    pub enable_mania_key_hit_errors: bool,
}

impl Default for ApiRuntimeSettings {
    fn default() -> Self {
        Self {
            enable_key_overlay: true,
            enable_mania_key_hit_errors: false,
        }
    }
}

/// Immutable focus, lifecycle, and ordered all-instance publication data.
///
/// `revision` is a semantic content key. Publishers retain it when a polling
/// pass merely rematerializes equivalent metadata and increment it whenever
/// focus, readiness, lifecycle, roles, roster, API runtime settings, or any
/// all-instance state used by a builder changes.
#[derive(Clone, Debug, PartialEq)]
pub struct ManagerSnapshot {
    revision: u64,
    focused_pid: Option<Pid>,
    focused_client: Option<ClientType>,
    game_focused: bool,
    runtime_settings: ApiRuntimeSettings,
    instances: BTreeMap<Pid, ManagerInstanceSnapshot>,
}

impl ManagerSnapshot {
    /// Builds a manager snapshot; entries are always stored in ascending PID order.
    ///
    /// Focused client identity is derived from the selected entry so metadata
    /// cannot disagree with the focused state bundle.
    pub fn new(
        revision: u64,
        focused_pid: Option<Pid>,
        game_focused: bool,
        runtime_settings: ApiRuntimeSettings,
        instances: impl IntoIterator<Item = ManagerInstanceSnapshot>,
    ) -> Self {
        let instances: BTreeMap<_, _> = instances
            .into_iter()
            .map(|instance| (instance.pid(), instance))
            .collect();
        let focused_pid = focused_pid.filter(|pid| instances.contains_key(pid));
        let focused_client = focused_pid
            .and_then(|pid| instances.get(&pid))
            .map(ManagerInstanceSnapshot::client);
        Self {
            revision,
            focused_pid,
            focused_client,
            game_focused: game_focused && focused_client.is_some(),
            runtime_settings,
            instances,
        }
    }

    /// Explicit no-focused-client/readiness state at a semantic revision.
    pub fn not_ready(revision: u64, runtime_settings: ApiRuntimeSettings) -> Self {
        Self::new(revision, None, false, runtime_settings, [])
    }

    /// Semantic manager content revision used for cache dependency checks.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// PID selected for public API output, or `None` when not ready.
    pub const fn focused_pid(&self) -> Option<Pid> {
        self.focused_pid
    }

    /// Focused client classification, or `None` when not ready.
    pub const fn focused_client(&self) -> Option<ClientType> {
        self.focused_client
    }

    /// Whether an osu! process owns foreground focus.
    pub const fn game_focused(&self) -> bool {
        self.game_focused
    }

    /// API-affecting runtime settings from this atomic publication.
    pub const fn runtime_settings(&self) -> &ApiRuntimeSettings {
        &self.runtime_settings
    }

    /// All instances in deterministic ascending PID order.
    pub fn instances(&self) -> &BTreeMap<Pid, ManagerInstanceSnapshot> {
        &self.instances
    }

    /// Selected instance entry, if the focused PID is still present.
    pub fn focused_instance(&self) -> Option<&ManagerInstanceSnapshot> {
        self.focused_pid.and_then(|pid| self.instances.get(&pid))
    }

    /// Whether a focused instance state is available to builders.
    pub fn is_ready(&self) -> bool {
        self.focused_instance().is_some()
    }
}

type CachedEncoding = Result<Bytes, ApiError>;

#[derive(Debug, Default)]
struct EncodingCaches {
    v1: OnceLock<CachedEncoding>,
    v2: OnceLock<CachedEncoding>,
    sc: OnceLock<CachedEncoding>,
    precise_v2: OnceLock<CachedEncoding>,
}

impl EncodingCaches {
    fn cell(&self, schema: ApiSchema) -> &OnceLock<CachedEncoding> {
        match schema {
            ApiSchema::V1 => &self.v1,
            ApiSchema::V2 => &self.v2,
            ApiSchema::StreamCompanion => &self.sc,
            ApiSchema::V2Precise => &self.precise_v2,
        }
    }

    fn inherit(&self, previous: &Self, manager_same: bool, regular_same: bool, precise_same: bool) {
        for schema in ApiSchema::ALL {
            let dependency = schema.dependency();
            let reusable = (!dependency.includes_manager() || manager_same)
                && (!dependency.includes_regular() || regular_same)
                && (!dependency.includes_precise() || precise_same);
            if reusable && let Some(result) = previous.cell(schema).get() {
                let _ = self.cell(schema).set(result.clone());
            }
        }
    }
}

/// One atomic manager/focused-state publication shared by all API readers.
#[derive(Debug)]
pub struct CombinedSnapshot {
    manager: Arc<ManagerSnapshot>,
    focused: Option<Arc<StateBundleSnapshot>>,
    caches: EncodingCaches,
}

impl CombinedSnapshot {
    /// Captures manager and focused state without cloning any inner state value.
    pub fn from_manager(manager: Arc<ManagerSnapshot>) -> Self {
        let focused = manager
            .focused_instance()
            .map(|instance| Arc::clone(instance.state()));
        Self {
            manager,
            focused,
            caches: EncodingCaches::default(),
        }
    }

    fn after(manager: Arc<ManagerSnapshot>, previous: &CombinedSnapshot) -> Self {
        let next = Self::from_manager(manager);
        let manager_same = next.manager.revision() == previous.manager.revision();
        let regular_same = option_arc_ptr_eq(next.regular(), previous.regular());
        let precise_same = option_arc_ptr_eq(next.precise(), previous.precise());
        next.caches
            .inherit(&previous.caches, manager_same, regular_same, precise_same);
        next
    }

    /// Immutable focus/lifecycle/all-instance component.
    pub fn manager(&self) -> &Arc<ManagerSnapshot> {
        &self.manager
    }

    /// API-affecting runtime settings from the same atomic publication.
    pub fn runtime_settings(&self) -> &ApiRuntimeSettings {
        self.manager.runtime_settings()
    }

    /// Focused state bundle, or `None` for the explicit not-ready state.
    pub fn focused(&self) -> Option<&Arc<StateBundleSnapshot>> {
        self.focused.as_ref()
    }

    /// Focused source client, if ready.
    pub fn source(&self) -> Option<ClientType> {
        self.focused.as_ref().map(|state| state.source)
    }

    /// Focused source generation, if ready.
    pub fn generation(&self) -> Option<u64> {
        self.focused.as_ref().map(|state| state.generation)
    }

    /// Focused reset epoch, if ready.
    pub fn reset_epoch(&self) -> Option<u64> {
        self.focused.as_ref().map(|state| state.reset_epoch)
    }

    /// Independently shared focused regular component, if ready.
    pub fn regular(&self) -> Option<&Arc<RegularState>> {
        self.focused.as_ref().map(|state| &state.regular)
    }

    /// Independently shared focused precise component, if ready.
    pub fn precise(&self) -> Option<&Arc<PreciseState>> {
        self.focused.as_ref().map(|state| &state.precise)
    }

    /// Returns an already computed unfiltered result, if present.
    pub fn cached(&self, schema: ApiSchema) -> Option<CachedEncoding> {
        self.caches.cell(schema).get().cloned()
    }

    /// Builds and encodes an unfiltered document at most once per schema.
    ///
    /// Successes and ordinary failures are cached and cloned for all callers.
    /// If `encoder` panics, `OnceLock` remains uninitialized and a later call
    /// retries; the cache is never poisoned.
    pub fn encode_unfiltered<F>(&self, schema: ApiSchema, encoder: F) -> CachedEncoding
    where
        F: FnOnce(&CombinedSnapshot) -> CachedEncoding,
    {
        self.caches
            .cell(schema)
            .get_or_init(|| encoder(self))
            .clone()
    }
}

fn option_arc_ptr_eq<T>(left: Option<&Arc<T>>, right: Option<&Arc<T>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

/// Atomic latest-only publication manager for combined API snapshots.
///
/// Publishers are serialized; readers are lock-free and load one complete
/// `Arc<CombinedSnapshot>` from `ArcSwap`. No history or update queue is kept.
#[derive(Debug)]
pub struct SnapshotManager {
    latest: ArcSwap<CombinedSnapshot>,
    publish_lock: Mutex<()>,
}

impl SnapshotManager {
    /// Creates a manager from current immutable focus/lifecycle/all-instance data.
    pub fn new(initial: Arc<ManagerSnapshot>) -> Self {
        Self {
            latest: ArcSwap::from_pointee(CombinedSnapshot::from_manager(initial)),
            publish_lock: Mutex::new(()),
        }
    }

    /// Atomically loads the latest whole publication point.
    pub fn load(&self) -> Arc<CombinedSnapshot> {
        self.latest.load_full()
    }

    /// Atomically replaces manager, focus, and all instance snapshots together.
    pub fn publish(&self, manager: Arc<ManagerSnapshot>) -> Arc<CombinedSnapshot> {
        let _guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = self.latest.load_full();
        let next = Arc::new(CombinedSnapshot::after(manager, &previous));
        self.latest.store(Arc::clone(&next));
        next
    }
}

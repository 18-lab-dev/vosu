use vosu_adapter_support::filtered::SharedDocument;

use super::*;

pub(super) struct SnapshotPathResolver<'a> {
    pub(super) executable: PathBuf,
    pub(super) launch: PathBuf,
    pub(super) environment: &'a EnvironmentSnapshot,
}

impl PathResolver for SnapshotPathResolver<'_> {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }

    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.launch.clone())
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        self.environment.get(name).map(OsString::from)
    }
}

/// Strongly typed bridge from immutable internal snapshots to public wire
/// documents. The four schema modules provide the production implementation.
pub trait SnapshotEncoder: Send + Sync + 'static {
    fn encode(
        &self,
        snapshot: &CombinedSnapshot,
        schema: ApiSchema,
        filters: &[Filter],
    ) -> Result<Bytes, ApiError>;
}

/// Production encoder dispatching to the four frozen wire builders.
#[derive(Default)]
pub struct ProductionSnapshotEncoder;


impl SnapshotEncoder for ProductionSnapshotEncoder {
    fn encode(
        &self,
        snapshot: &CombinedSnapshot,
        schema: ApiSchema,
        filters: &[Filter],
    ) -> Result<Bytes, ApiError> {
        if filters.is_empty() {
            return match schema {
                ApiSchema::V1 => crate::api::v1::encode(snapshot),
                ApiSchema::V2 => crate::api::v2::encode(snapshot),
                ApiSchema::StreamCompanion => crate::api::sc::encode(snapshot),
                ApiSchema::V2Precise => crate::api::precise::encode(snapshot),
            };
        }
        match schema {
            ApiSchema::V1 => encode_selection(snapshot, filters, crate::api::v1::build),
            ApiSchema::V2 => encode_selection(snapshot, filters, crate::api::v2::build),
            ApiSchema::StreamCompanion => {
                encode_selection(snapshot, filters, crate::api::sc::build)
            }
            ApiSchema::V2Precise => encode_selection(snapshot, filters, crate::api::precise::build),
        }
    }
}

/// A wire document built from one publication, kept for the rest of a
/// broadcast pass. It holds that publication's manager and focused-state
/// `Arc`s, so pointer equality with them identifies the same immutable
/// inputs; an address cannot be reused while the document is alive.
struct PassDocument<D> {
    manager: Arc<crate::api::ManagerSnapshot>,
    focused: Option<Arc<vosu_osu_state::StateBundleSnapshot>>,
    document: SharedDocument<D>,
}

impl<D> PassDocument<D> {
    fn built_from(&self, snapshot: &CombinedSnapshot) -> bool {
        Arc::ptr_eq(&self.manager, snapshot.manager())
            && match (&self.focused, snapshot.focused()) {
                (Some(kept), Some(focused)) => Arc::ptr_eq(kept, focused),
                (None, None) => true,
                _ => false,
            }
    }
}

/// Encodes the projection of the document `build` derives from `snapshot`.
/// Every client of a broadcast pass projects the same snapshot, so within a
/// pass the document is built once and reused (the slot type is per document
/// type, hence per schema). Building depends only on the snapshot's immutable
/// manager and focused state.
fn encode_selection<D: Serialize + 'static>(
    snapshot: &CombinedSnapshot,
    filters: &[Filter],
    build: fn(&CombinedSnapshot) -> Result<D, ApiError>,
) -> Result<Bytes, ApiError> {
    crate::server::websocket::with_pass_local(|slot: &mut Option<PassDocument<D>>| {
        if !slot.as_ref().is_some_and(|kept| kept.built_from(snapshot)) {
            // Released before building; it belongs to another snapshot.
            *slot = None;
            *slot = Some(PassDocument {
                manager: Arc::clone(snapshot.manager()),
                focused: snapshot.focused().cloned(),
                document: SharedDocument::new(build(snapshot)?),
            });
        }
        let kept = slot.as_mut().expect("document was just stored");
        kept.document.encode(filters).map_err(Into::into)
    })
    .unwrap_or_else(|| {
        SharedDocument::new(build(snapshot)?)
            .encode(filters)
            .map_err(Into::into)
    })
}

/// Request authorization policy whose immutable snapshot is replaced on config
/// reload. Requests already in flight keep their loaded policy; subsequent
/// requests observe the new allow-list and server host without a listener
/// restart.
pub struct ReloadableAuthorizer {
    policy: ArcSwap<AuthorizationPolicy>,
}

impl ReloadableAuthorizer {
    pub fn new(allowed_ips: &str, server_host: &str) -> Self {
        Self {
            policy: ArcSwap::from_pointee(AuthorizationPolicy::new(allowed_ips, server_host)),
        }
    }

    pub fn reload(&self, allowed_ips: &str, server_host: &str) {
        self.policy
            .store(Arc::new(AuthorizationPolicy::new(allowed_ips, server_host)));
    }
}

impl RequestAuthorizer for ReloadableAuthorizer {
    fn authorize(
        &self,
        peer: std::net::IpAddr,
        headers: RequestHeaders<'_>,
    ) -> AuthorizationDecision {
        self.policy.load().authorize(peer, headers)
    }
}

/// Concrete HTTP and WebSocket state provider over one atomic snapshot owner.
///
/// API consumers read `snapshots`. The selected-field subscription reads
/// `live`, which also carries the samples taken between API cadence points.
pub struct SnapshotProviders {
    snapshots: Arc<SnapshotManager>,
    live: SnapshotManager,
    encoder: Arc<dyn SnapshotEncoder>,
    regular_event: watch::Sender<u64>,
    precise_event: watch::Sender<u64>,
    live_event: watch::Sender<u64>,
    regular_poll_ms: AtomicU64,
    precise_poll_ms: AtomicU64,
}

impl SnapshotProviders {
    pub fn new(initial: Arc<ManagerSnapshot>, encoder: Arc<dyn SnapshotEncoder>) -> Self {
        let revision = initial.revision();
        let (regular_event, _) = watch::channel(revision);
        let (precise_event, _) = watch::channel(revision);
        let (live_event, _) = watch::channel(revision);
        Self {
            snapshots: Arc::new(SnapshotManager::new(Arc::clone(&initial))),
            live: SnapshotManager::new(initial),
            encoder,
            regular_event,
            precise_event,
            live_event,
            regular_poll_ms: AtomicU64::new(150),
            precise_poll_ms: AtomicU64::new(10),
        }
    }

    pub fn snapshot_manager(&self) -> Arc<SnapshotManager> {
        Arc::clone(&self.snapshots)
    }

    pub fn set_poll_rates(&self, regular: Duration, precise: Duration) {
        self.regular_poll_ms.store(
            regular.as_millis().try_into().unwrap_or(u64::MAX),
            Ordering::Release,
        );
        self.precise_poll_ms.store(
            precise.as_millis().try_into().unwrap_or(u64::MAX),
            Ordering::Release,
        );
    }

    /// Publishes one atomic state point to API and live consumers alike.
    pub fn publish(&self, manager: Arc<ManagerSnapshot>) -> Arc<CombinedSnapshot> {
        let snapshot = self.publish_api(Arc::clone(&manager));
        self.publish_live(manager);
        snapshot
    }

    /// Publishes one atomic API state point. Precise consumers depend on every
    /// component and are always woken. Regular schemas are not woken for a
    /// precise-only replacement, matching the source's independent cadence.
    pub fn publish_api(&self, manager: Arc<ManagerSnapshot>) -> Arc<CombinedSnapshot> {
        let revision = manager.revision();
        let previous = self.snapshots.load();
        let snapshot = self.snapshots.publish(manager);
        let regular_same = match (previous.regular(), snapshot.regular()) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        };
        let precise_same = match (previous.precise(), snapshot.precise()) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        };
        if !regular_same || precise_same {
            self.regular_event.send_replace(revision);
        }
        self.precise_event.send_replace(revision);
        snapshot
    }

    /// Publishes one atomic state point for the selected-field subscription only.
    pub fn publish_live(&self, manager: Arc<ManagerSnapshot>) {
        let revision = manager.revision();
        self.live.publish(manager);
        self.live_event.send_replace(revision);
    }

    pub fn load(&self) -> Arc<CombinedSnapshot> {
        self.snapshots.load()
    }
}

impl StateJsonProvider for SnapshotProviders {
    fn cached_json(
        &self,
        route: StateRoute,
    ) -> crate::server::routes::ProviderFuture<'_, CachedJson> {
        Box::pin(async move {
            let schema = api_schema(route);
            let snapshot = self.snapshots.load();
            if snapshot.focused().is_none() {
                return CachedJson::NotReady;
            }
            match self.encoder.encode(&snapshot, schema, &[]) {
                Ok(bytes) => CachedJson::Ready(bytes),
                Err(ApiError::NotReady) => CachedJson::NotReady,
                Err(error) => CachedJson::Failed {
                    status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    body: Bytes::from(
                        serde_json::to_vec(&serde_json::json!({"error": error.to_string()}))
                            .expect("error JSON serializes"),
                    ),
                },
            }
        })
    }
}


pub(super) struct WebSocketSnapshotView {
    snapshot: Arc<CombinedSnapshot>,
    schema: ApiSchema,
    encoder: Arc<dyn SnapshotEncoder>,
}

impl WebSocketStateSnapshot for WebSocketSnapshotView {
    fn generation(&self) -> u64 {
        self.snapshot.manager().revision()
    }

    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.encoder
            .encode(&self.snapshot, self.schema, &[])
            .map_err(|error| error.to_string())
    }

    fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String> {
        self.encoder
            .encode(&self.snapshot, self.schema, filters)
            .map_err(|error| error.to_string())
    }
}

impl WebSocketStateProvider for SnapshotProviders {
    fn poll_rate(&self, schema: WebSocketSchema) -> Duration {
        let milliseconds = if schema == WebSocketSchema::V2Precise {
            self.precise_poll_ms.load(Ordering::Acquire)
        } else {
            self.regular_poll_ms.load(Ordering::Acquire)
        };
        Duration::from_millis(milliseconds)
    }

    fn subscribe(&self, schema: WebSocketSchema) -> watch::Receiver<u64> {
        if schema == WebSocketSchema::V2Precise {
            self.precise_event.subscribe()
        } else {
            self.regular_event.subscribe()
        }
    }

    fn capture(&self, schema: WebSocketSchema) -> Option<Arc<dyn WebSocketStateSnapshot>> {
        let snapshot = self.snapshots.load();
        snapshot.focused()?;
        Some(Arc::new(WebSocketSnapshotView {
            snapshot,
            schema: api_schema_ws(schema),
            encoder: Arc::clone(&self.encoder),
        }))
    }
}

pub(super) const fn api_schema(route: StateRoute) -> ApiSchema {
    match route {
        StateRoute::V1 => ApiSchema::V1,
        StateRoute::StreamCompanion => ApiSchema::StreamCompanion,
        StateRoute::V2 => ApiSchema::V2,
        StateRoute::V2Precise => ApiSchema::V2Precise,
    }
}

pub(super) const fn api_schema_ws(schema: WebSocketSchema) -> ApiSchema {
    match schema {
        WebSocketSchema::V1 => ApiSchema::V1,
        WebSocketSchema::StreamCompanion => ApiSchema::StreamCompanion,
        WebSocketSchema::V2 => ApiSchema::V2,
        WebSocketSchema::V2Precise => ApiSchema::V2Precise,
    }
}


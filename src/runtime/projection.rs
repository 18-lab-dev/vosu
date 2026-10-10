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
        let value = match schema {
            ApiSchema::V1 => serialize_projection_value(&crate::api::v1::build(snapshot)?)?,
            ApiSchema::V2 => serialize_projection_value(&crate::api::v2::build(snapshot)?)?,
            ApiSchema::StreamCompanion => {
                serialize_projection_value(&crate::api::sc::build(snapshot)?)?
            }
            ApiSchema::V2Precise => {
                serialize_projection_value(&crate::api::precise::build(snapshot)?)?
            }
        };
        let projected = canonicalize_javascript_keys(project_filters(&value, filters));
        encode_javascript_json(&projected)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ProjectionValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Utf16CodeUnit(u16),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
    NonFiniteNumber,
}

pub(super) fn serialize_projection_value<T>(value: &T) -> Result<ProjectionValue, ApiError>
where
    T: Serialize,
{
    let json =
        serde_json::to_value(value).map_err(|error| ApiError::serialization(error.to_string()))?;
    let typed =
        serde_value::to_value(value).map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(projection_value_from_serializations(json, &typed))
}

pub(super) fn projection_value_from_serializations(
    json: serde_json::Value,
    typed: &serde_value::Value,
) -> ProjectionValue {
    let typed = unwrap_serde_value(typed);
    match json {
        serde_json::Value::Null
            if matches!(
                typed,
                serde_value::Value::F32(value) if !value.is_finite()
            ) || matches!(
                typed,
                serde_value::Value::F64(value) if !value.is_finite()
            ) =>
        {
            ProjectionValue::NonFiniteNumber
        }
        serde_json::Value::Null => ProjectionValue::Null,
        serde_json::Value::Bool(value) => ProjectionValue::Bool(value),
        serde_json::Value::Number(value) => ProjectionValue::Number(value),
        serde_json::Value::String(value) => ProjectionValue::String(value),
        serde_json::Value::Array(values) => {
            let typed_values = match typed {
                serde_value::Value::Seq(values) => Some(values.as_slice()),
                _ => None,
            };
            ProjectionValue::Array(
                values
                    .into_iter()
                    .enumerate()
                    .map(
                        |(index, value)| match typed_values.and_then(|values| values.get(index)) {
                            Some(typed) => projection_value_from_serializations(value, typed),
                            None => projection_value_from_json(value),
                        },
                    )
                    .collect(),
            )
        }
        serde_json::Value::Object(object) => {
            let typed_object = match typed {
                serde_value::Value::Map(object) => Some(object),
                _ => None,
            };
            ProjectionValue::Object(
                object
                    .into_iter()
                    .map(|(key, value)| {
                        let typed_value = typed_object.and_then(|object| {
                            object.iter().find_map(|(candidate, value)| {
                                matches!(candidate, serde_value::Value::String(candidate) if candidate == &key)
                                    .then_some(value)
                            })
                        });
                        let value = match typed_value {
                            Some(typed) => projection_value_from_serializations(value, typed),
                            None => projection_value_from_json(value),
                        };
                        (key, value)
                    })
                    .collect(),
            )
        }
    }
}

pub(super) fn unwrap_serde_value(mut value: &serde_value::Value) -> &serde_value::Value {
    loop {
        value = match value {
            serde_value::Value::Newtype(inner) | serde_value::Value::Option(Some(inner)) => inner,
            _ => return value,
        };
    }
}

pub(super) fn projection_value_from_json(value: serde_json::Value) -> ProjectionValue {
    match value {
        serde_json::Value::Null => ProjectionValue::Null,
        serde_json::Value::Bool(value) => ProjectionValue::Bool(value),
        serde_json::Value::Number(value) => ProjectionValue::Number(value),
        serde_json::Value::String(value) => ProjectionValue::String(value),
        serde_json::Value::Array(values) => {
            ProjectionValue::Array(values.into_iter().map(projection_value_from_json).collect())
        }
        serde_json::Value::Object(object) => ProjectionValue::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, projection_value_from_json(value)))
                .collect(),
        ),
    }
}

pub(super) fn encode_javascript_json(value: &ProjectionValue) -> Result<Bytes, ApiError> {
    let mut bytes = Vec::new();
    write_projection_json(&mut bytes, value)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(Bytes::from(bytes))
}

pub(super) fn write_projection_json(
    bytes: &mut Vec<u8>,
    value: &ProjectionValue,
) -> io::Result<()> {
    match value {
        ProjectionValue::Null | ProjectionValue::NonFiniteNumber => {
            bytes.extend_from_slice(b"null")
        }
        ProjectionValue::Bool(value) => {
            bytes.extend_from_slice(if *value { b"true" } else { b"false" })
        }
        ProjectionValue::Number(value) => {
            if value.is_i64() {
                bytes
                    .extend_from_slice(value.as_i64().expect("checked i64").to_string().as_bytes());
            } else if value.is_u64() {
                bytes
                    .extend_from_slice(value.as_u64().expect("checked u64").to_string().as_bytes());
            } else {
                crate::calculation::write_js_json_number(
                    bytes,
                    value.as_f64().expect("JSON number"),
                )?;
            }
        }
        ProjectionValue::String(value) => serde_json::to_writer(bytes, value)?,
        ProjectionValue::Utf16CodeUnit(unit) => {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            bytes.extend_from_slice(b"\"\\u");
            bytes.push(HEX[usize::from(unit >> 12)]);
            bytes.push(HEX[usize::from((unit >> 8) & 0xf)]);
            bytes.push(HEX[usize::from((unit >> 4) & 0xf)]);
            bytes.push(HEX[usize::from(unit & 0xf)]);
            bytes.push(b'"');
        }
        ProjectionValue::Array(values) => {
            bytes.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                write_projection_json(bytes, value)?;
            }
            bytes.push(b']');
        }
        ProjectionValue::Object(entries) => {
            bytes.push(b'{');

            for (index, (key, value)) in entries.iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key)?;
                bytes.push(b':');
                write_projection_json(bytes, value)?;
            }
            bytes.push(b'}');
        }
    }
    Ok(())
}

pub(super) fn project_filters(value: &ProjectionValue, filters: &[Filter]) -> ProjectionValue {
    let mut projected = Vec::new();
    for filter in filters {
        match filter {
            Filter::Field(field) => {
                if let Some(value) = javascript_property(value, field) {
                    if let Some((_, existing)) = projected.iter_mut().find(|(key, _)| key == field)
                    {
                        *existing = value;
                    } else {
                        projected.push((field.clone(), value));
                    }
                }
            }
            Filter::Nested { field, keys } => {
                if let Some(value) = javascript_property(value, field)
                    .filter(|value| value != &ProjectionValue::Null)
                {
                    let value = project_filters(&value, keys);
                    if let Some((_, existing)) = projected.iter_mut().find(|(key, _)| key == field)
                    {
                        *existing = value;
                    } else {
                        projected.push((field.clone(), value));
                    }
                }
            }
        }
    }
    ProjectionValue::Object(projected)
}

pub(super) fn javascript_property(value: &ProjectionValue, key: &str) -> Option<ProjectionValue> {
    match value {
        ProjectionValue::Object(object) => object
            .iter()
            .find_map(|(candidate, value)| (candidate == key).then(|| value.clone())),
        ProjectionValue::Array(array) if key == "length" => Some(ProjectionValue::Number(
            serde_json::Number::from(array.len()),
        )),
        ProjectionValue::Array(array) => javascript_array_index(key)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| array.get(index))
            .cloned(),
        ProjectionValue::String(string) if key == "length" => Some(ProjectionValue::Number(
            serde_json::Number::from(string.encode_utf16().count()),
        )),
        ProjectionValue::String(string) => javascript_array_index(key)
            .and_then(|index| usize::try_from(index).ok())
            .and_then(|index| string.encode_utf16().nth(index))
            .map(|unit| {
                char::from_u32(u32::from(unit))
                    .map_or(ProjectionValue::Utf16CodeUnit(unit), |character| {
                        ProjectionValue::String(character.to_string())
                    })
            }),
        ProjectionValue::Utf16CodeUnit(_) if key == "length" => {
            Some(ProjectionValue::Number(serde_json::Number::from(1)))
        }
        ProjectionValue::Utf16CodeUnit(_) if javascript_array_index(key) == Some(0) => {
            Some(value.clone())
        }
        _ => None,
    }
}

pub(super) fn javascript_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

pub(super) fn canonicalize_javascript_keys(value: ProjectionValue) -> ProjectionValue {
    match value {
        ProjectionValue::Object(object) => {
            let mut indices = Vec::new();
            let mut others = Vec::new();
            for (key, value) in object {
                let value = canonicalize_javascript_keys(value);
                if let Some(index) = javascript_array_index(&key) {
                    indices.push((index, key, value));
                } else {
                    others.push((key, value));
                }
            }
            indices.sort_by_key(|(index, _, _)| *index);
            ProjectionValue::Object(
                indices
                    .into_iter()
                    .map(|(_, key, value)| (key, value))
                    .chain(others)
                    .collect(),
            )
        }
        ProjectionValue::Array(values) => ProjectionValue::Array(
            values
                .into_iter()
                .map(canonicalize_javascript_keys)
                .collect(),
        ),
        value => value,
    }
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

struct V2SubscriptionSnapshotView {
    snapshot: Arc<CombinedSnapshot>,
}

impl V2SubscriptionSnapshot for V2SubscriptionSnapshotView {
    fn revision(&self) -> u64 {
        self.snapshot.manager().revision()
    }

    fn document(&self) -> Result<Option<serde_json::Value>, ApiError> {
        match crate::api::v2::document(&self.snapshot) {
            Ok(document) => Ok(Some(document)),
            Err(ApiError::NotReady) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl V2SubscriptionProvider for SnapshotProviders {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.live_event.subscribe()
    }

    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
        Arc::new(V2SubscriptionSnapshotView {
            snapshot: self.live.load(),
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

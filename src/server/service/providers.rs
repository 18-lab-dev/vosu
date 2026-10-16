/// Latest immutable state API adapter. Each wire encoder uses the snapshot's own
/// `OnceLock`, so HTTP and WebSocket consumers share exactly one encoding.
#[derive(Clone)]
pub struct SnapshotStateProvider {
    snapshots: Arc<SnapshotManager>,
}

impl SnapshotStateProvider {
    pub fn new(snapshots: Arc<SnapshotManager>) -> Self {
        Self { snapshots }
    }
    pub fn snapshots(&self) -> &Arc<SnapshotManager> {
        &self.snapshots
    }
}

impl StateJsonProvider for SnapshotStateProvider {
    fn cached_json(&self, route: StateRoute) -> ProviderFuture<'_, CachedJson> {
        Box::pin(async move {
            let snapshot = self.snapshots.load();
            let encoded = match route {
                StateRoute::V1 => api::v1::encode(&snapshot),
                StateRoute::StreamCompanion => api::sc::encode(&snapshot),
                StateRoute::V2 => api::v2::encode(&snapshot),
                StateRoute::V2Precise => api::precise::encode(&snapshot),
            };
            match encoded {
                Ok(bytes) => CachedJson::Ready(bytes),
                Err(ApiError::NotReady) => CachedJson::NotReady,
                Err(error) => CachedJson::Failed {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    body: json_bytes(json!({ "error": error.to_string() })),
                },
            }
        })
    }
}

/// Live beatmap-relative paths and trusted roots needed by calculation and file routes.
///
/// The three current-file paths are always interpreted relative to `songs_root`;
/// they are never promoted to roots of their own.
#[derive(Clone, Debug, Default)]
pub struct BeatmapFiles {
    pub beatmap_file: Option<PathBuf>,
    pub background_file: Option<PathBuf>,
    pub audio_file: Option<PathBuf>,
    pub songs_root: Option<PathBuf>,
    pub skin_root: Option<PathBuf>,
    pub static_root: Option<PathBuf>,
    pub lazer: bool,
    pub ready: bool,
}

/// Narrow cycle-free view of currently selected game files.
pub trait CurrentBeatmapProvider: Send + Sync {
    fn files(&self) -> BeatmapFiles;
    fn current_beatmap_bytes(&self) -> Result<Option<Bytes>, String>;
}

/// Legacy rosu-pp-js compatible calculation adapter.
#[derive(Clone)]
pub struct LegacyCalculateProvider {
    current: Arc<dyn CurrentBeatmapProvider>,
    engine: LegacyRosuEngine,
}

impl LegacyCalculateProvider {
    pub fn new(current: Arc<dyn CurrentBeatmapProvider>) -> Self {
        Self {
            current,
            engine: LegacyRosuEngine,
        }
    }

    async fn calculate(&self, query: PpQuery) -> CalculateResult {
        let bytes = match self.select_beatmap(&query).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return CalculateResult::NotReady,
            Err(message) => return calculation_error(StatusCode::BAD_REQUEST, message),
        };
        let request = match performance_request(&query) {
            Ok(request) => request,
            Err(message) => return calculation_error(StatusCode::BAD_REQUEST, message),
        };
        match self.engine.calculate(&bytes, &request) {
            Ok(result) => match serde_json::to_vec(&result) {
                Ok(body) => CalculateResult::Ready(Bytes::from(body)),
                Err(error) => {
                    calculation_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
                }
            },
            Err(error) => calculation_error(StatusCode::BAD_REQUEST, error.to_string()),
        }
    }

    async fn select_beatmap(&self, query: &PpQuery) -> Result<Option<Bytes>, String> {
        const MAX_BEATMAP_BYTES: u64 = 64 * 1024 * 1024;
        if let Some(requested) = query.path.as_deref() {
            let path = Path::new(requested);
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("osu"))
            {
                return Err("path must select a .osu file".into());
            }
            let canonical = tokio::fs::canonicalize(path)
                .await
                .map_err(|_| "beatmap path does not exist".to_owned())?;
            if canonical
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("osu"))
            {
                return Err("canonical path must select a .osu file".into());
            }
            let metadata = tokio::fs::metadata(&canonical)
                .await
                .map_err(|error| error.to_string())?;
            if !metadata.is_file() {
                return Err("beatmap path must select a regular file".into());
            }
            let mut file = tokio::fs::File::open(&canonical)
                .await
                .map_err(|error| error.to_string())?;
            let metadata = file.metadata().await.map_err(|error| error.to_string())?;
            if !metadata.is_file() {
                return Err("beatmap path must select a regular file".into());
            }
            if metadata.len() > MAX_BEATMAP_BYTES {
                return Err("beatmap exceeds the 64 MiB input limit".into());
            }
            let mut reader = tokio::io::AsyncReadExt::take(&mut file, MAX_BEATMAP_BYTES + 1);
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes)
                .await
                .map_err(|error| error.to_string())?;
            if bytes.len() as u64 > MAX_BEATMAP_BYTES {
                return Err("beatmap exceeds the 64 MiB input limit".into());
            }
            return Ok(Some(Bytes::from(bytes)));
        }
        let bytes = self
            .current
            .current_beatmap_bytes()
            .map_err(|error| error.to_string())?;
        if bytes
            .as_ref()
            .is_some_and(|bytes| bytes.len() > MAX_BEATMAP_BYTES as usize)
        {
            return Err("beatmap exceeds the 64 MiB input limit".into());
        }
        Ok(bytes)
    }
}

impl CalculatePpProvider for LegacyCalculateProvider {
    fn calculate_pp(&self, query: PpQuery) -> ProviderFuture<'_, CalculateResult> {
        Box::pin(async move { self.calculate(query).await })
    }
}

fn performance_request(query: &PpQuery) -> Result<LegacyPerformanceRequest, String> {
    const MIN_CLOCK_RATE: f64 = 0.01;
    const MAX_CLOCK_RATE: f64 = 100.0;
    if query
        .clock_rate
        .is_some_and(|rate| !rate.is_finite() || !(MIN_CLOCK_RATE..=MAX_CLOCK_RATE).contains(&rate))
    {
        return Err("clockRate must be between 0.01 and 100".into());
    }
    let mode = match query.mode {
        None => None,
        Some(0) => Some(crate::calculation::engine::Ruleset::Osu),
        Some(1) => Some(crate::calculation::engine::Ruleset::Taiko),
        Some(2) => Some(crate::calculation::engine::Ruleset::Catch),
        Some(3) => Some(crate::calculation::engine::Ruleset::Mania),
        Some(_) => return Err("mode must be 0, 1, 2, or 3".into()),
    };
    let mods = parse_legacy_mods(query.mods.as_ref())?;
    let hitresult_priority = match query.hitresult_priority.unwrap_or(0) {
        0 => HitResultPriority::BestCase,
        1 => HitResultPriority::WorstCase,
        _ => return Err("hitresultPriority must be 0 or 1".into()),
    };
    Ok(LegacyPerformanceRequest {
        mode,
        lazer: query.lazer,
        mods,
        clock_rate: query.clock_rate,
        ar: query.ar.map(|value| value as f32),
        cs: query.cs.map(|value| value as f32),
        hp: query.hp.map(|value| value as f32),
        od: query.od.map(|value| value as f32),
        passed_objects: query.passed_objects,
        combo: query.combo,
        n_misses: query.n_misses,
        n100: query.n100,
        n300: query.n300,
        n50: query.n50,
        n_geki: query.n_geki,
        n_katu: query.n_katu,
        slider_end_hits: query.slider_end_hits,
        small_tick_hits: query.small_tick_hits,
        large_tick_hits: query.large_tick_hits,
        accuracy: query.accuracy,
        hitresult_priority,
    })
}

fn parse_legacy_mods(input: Option<&super::routes::ModsInput>) -> Result<LegacyMods, String> {
    use super::routes::ModsInput;
    match input {
        None => Ok(LegacyMods::Bits(0)),
        Some(ModsInput::Bits(bits)) => Ok(LegacyMods::Bits(*bits)),
        Some(ModsInput::Json(value)) => serde_json::from_value::<LegacyMods>(value.clone())
            .map_err(|error| format!("invalid mods: {error}")),
        Some(ModsInput::Acronyms(value)) => {
            let compact: String = value
                .chars()
                .filter(|ch| !ch.is_ascii_whitespace() && *ch != ',')
                .collect();
            if !compact.len().is_multiple_of(2)
                || !compact.bytes().all(|byte| byte.is_ascii_alphabetic())
            {
                return Err("invalid mods acronyms".into());
            }
            let mods = compact
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| crate::calculation::engine::StructuredMod {
                    acronym: String::from_utf8_lossy(pair).to_ascii_uppercase(),
                    settings: Default::default(),
                })
                .collect();
            Ok(LegacyMods::Structured(mods))
        }
    }
}

pub trait PageDataProvider: Send + Sync {
    fn render(
        &self,
        page: DynamicPage,
        authority: Option<String>,
    ) -> ProviderFuture<'_, Result<EmbeddedResponse, String>>;
    fn search_counters(
        &self,
        query: &str,
        external: bool,
        authority: Option<String>,
    ) -> ProviderFuture<'_, Result<Bytes, String>>;
}

/// Config update facade for `/api/settingsSave`.
pub trait ConfigUpdateService: Send + Sync {
    fn save(&self, values: Value) -> ProviderFuture<'_, Result<(), String>>;
}

/// Runtime report input facade; report rendering remains in `report.rs`.
pub trait ReportInputProvider: Send + Sync {
    fn capture(&self) -> Result<ReportInput, String>;
    /// Read-only dashboard metadata; no machine probing or log collection.
    fn dashboard_status(&self) -> Value {
        json!({ "instances": [], "available": false })
    }
}

pub struct DeferredUpdateTrigger(Option<Box<dyn FnOnce() + Send + 'static>>);

impl DeferredUpdateTrigger {
    pub fn new(trigger: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(trigger)))
    }

    fn fire(mut self) {
        if let Some(trigger) = self.0.take() {
            trigger();
        }
    }
}

pub enum UpdateRouteResult {
    Latest,
    NoFiles,
    Updated(DeferredUpdateTrigger),
}

/// Single-flight update operation. Implementations prepare and spawn the helper
/// but defer daemon shutdown through `DeferredUpdateTrigger` until the terminal
/// response body has been consumed (or dropped on disconnect).
pub trait UpdaterHandoff: Send + Sync {
    fn run_update(&self) -> ProviderFuture<'_, Result<UpdateRouteResult, String>>;
}


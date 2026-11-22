use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct CalculationInput {
    pub game_folder: PathBuf,
    pub songs_folder: PathBuf,
    pub menu_folder: Option<String>,
    pub filename: Option<String>,
    pub beatmap_path: PathBuf,
    pub checksum: String,
    pub mode: i64,
    pub mods: CalculateMods,
    pub client_scoring: ClientType,
}

#[derive(Debug, Clone)]
pub struct StateCalculationRequest {
    pub input: CalculationInput,
    pub key: StateCalculationKey,
    pub phase: CalculationPhase,
    pub entry: Arc<StateBundleSnapshot>,
    pub current: Arc<StateBundleSnapshot>,
    pub mods: CalculateMods,
    pub calculate_pp: bool,
    pub play_time: f64,
    pub calculation_reset_generation: u64,
    pub gameplay: Option<Score>,
    pub result: Option<RawResultScreen>,
}

#[derive(Debug, Clone, Default)]
pub struct BeatmapCalculationUpdate {
    pub original_mode: Option<i64>,
    /// Update only retained-map timing/event state without replacing map identity,
    /// graph, or calculated metadata.
    pub event_only: bool,
    /// Retain map identity/calculated metadata while replacing graph padding
    /// and timing/event projection from the retained parsed map.
    pub graph_event_only: bool,
    /// Leave the reducer's state key retryable after this accepted update.
    pub retry_state_key: bool,
    /// Stable-only parsed Events background override, already path-sanitized.
    pub background_filename: Option<String>,
    pub first_object_time: Option<i64>,
    pub full_pp: f64,
    pub source_key: Option<String>,
    pub pp_acc: Option<BTreeMap<u8, f64>>,
    pub parsed: Option<Arc<ParsedBeatmap>>,
    pub graph: Arc<StrainGraph>,
    pub event_status: EventStatus,
    pub graph_mp3_length: Option<f64>,
}
impl BeatmapCalculationUpdate {
    pub fn from_parsed(
        parsed: Arc<ParsedBeatmap>,
        play_time: f64,
        mp3_length: Option<f64>,
    ) -> Self {
        Self::from_parsed_reusing(parsed, play_time, mp3_length, None)
    }

    pub fn from_parsed_reusing(
        parsed: Arc<ParsedBeatmap>,
        play_time: f64,
        mp3_length: Option<f64>,
        previous: Option<&BeatmapPPState>,
    ) -> Self {
        let clock_rate = parsed.clock_rate();
        let graph_length = mp3_length;
        let graph = previous
            .filter(|state| {
                state
                    .parsed
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &parsed))
                    && state.graph_mp3_length == mp3_length
            })
            .map_or_else(
                || Arc::new(parsed.build_graph(graph_length, clock_rate)),
                |state| state.graph.clone(),
            );
        Self {
            original_mode: Some(parsed.reporting_mode),
            event_only: false,
            graph_event_only: false,
            retry_state_key: false,
            background_filename: None,
            first_object_time: Some(i64::from(parsed.times.first_object)),
            full_pp: parsed.engine.performance.pp,
            source_key: Some(parsed.cache_key().source_checksum.clone()),
            pp_acc: Some(parsed.engine.pp_by_accuracy.clone()),
            graph,
            graph_mp3_length: mp3_length,
            event_status: parsed.event_status(play_time, clock_rate),
            parsed: Some(parsed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StateCalculationResult {
    pub key: StateCalculationKey,
    pub beatmap: Option<BeatmapCalculationUpdate>,
    pub gameplay: Option<ProgressiveAttributes>,
    pub result: Option<ResultPerformance>,
}

pub type StateCalculationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StateCalculationResult, StateWorkerError>> + Send + 'a>>;

pub trait StateCalculationBackend: Send + Sync + 'static {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a>;
}

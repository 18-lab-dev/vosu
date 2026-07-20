use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::beatmap::{
    AppliedMod, BeatmapCache, BeatmapEngine, BeatmapError, BeatmapInput, DifficultyAttributes,
    DifficultyValues, EngineOutput, EngineRequest, ModSet, PeakStrains, PerformanceAttributes,
    Ruleset as BeatmapRuleset, ScoreData, ScoringMode,
};
use super::progressive::{
    CalculationOutcome, ClientScoring, GameplayCacheKey, GameplayCalculator, GameplayFrame,
    GameplayGate, HitStatistics, LiveGradualProgression, LiveProgressiveBeatmap, ResultCalculator,
    ResultFrame, Ruleset,
};
use super::{mods_checksum, mods_for_calculation, safe_join};
use crate::model::enums::ClientType;
use crate::model::states::{Mod, Statistics};
use crate::state::{
    BeatmapCalculationUpdate, CalculationPhase, StateCalculationBackend, StateCalculationFuture,
    StateCalculationRequest, StateCalculationResult, StateWorkerError,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct LiveBeatmapEngine;

impl BeatmapEngine for LiveBeatmapEngine {
    fn calculate(&self, request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
        let engine = vosu_lazer_pp::LiveLazerEngine;
        let parsed = engine
            .parse(request.content.as_bytes())
            .map_err(engine_error)?;
        let mut map = if parsed.mode() == live_ruleset(request.effective_mode) {
            parsed
        } else {
            engine
                .convert(&parsed, live_ruleset(request.effective_mode))
                .map_err(engine_error)?
        };
        let mods = structured_mods(request.mods)?;
        engine.apply_mods(&mut map, &mods).map_err(engine_error)?;
        let base_hit_windows = vosu_lazer_pp::hit_windows_for_map(&map).map_err(engine_error)?;
        let catch_spatial_window = vosu_lazer_pp::catch_spatial_window_for_map(&map);
        let scale_window = |window: Option<f64>| window.map(|window| window / request.clock_rate);
        let hit_windows = vosu_lazer_pp::HitWindows {
            miss: scale_window(base_hit_windows.miss),
            meh: scale_window(base_hit_windows.meh),
            ok: scale_window(base_hit_windows.ok),
            good: scale_window(base_hit_windows.good),
            great: scale_window(base_hit_windows.great),
            perfect: scale_window(base_hit_windows.perfect),
        };
        let config = vosu_lazer_pp::DifficultyConfig::default();
        let original = engine.original_beatmap_difficulty(&map);
        let converted = engine.beatmap_difficulty(&map);
        let difficulty = engine
            .difficulty_data(&map, &config)
            .map_err(engine_error)?;
        let mut maximum = engine.create_score(&map, 1.0).map_err(engine_error)?;
        let scoring_max_combo = if request.effective_mode == BeatmapRuleset::Mania {
            maximum.perfects
        } else {
            difficulty.max_combo
        };
        let maximum_total_score = maximum.total_score;
        maximum.max_combo = difficulty.max_combo;
        let performance = engine
            .performance_data_from_attrs(&map, &config, &difficulty, maximum)
            .map_err(engine_error)?;
        let strains = engine.strains_data(&map, &config).map_err(engine_error)?;

        let mut pp_by_accuracy = BTreeMap::new();
        for accuracy in 90..=100 {
            let mut score = engine
                .create_score(&map, f64::from(accuracy) / 100.0)
                .map_err(engine_error)?;
            score.max_combo = difficulty.max_combo;
            let attrs = engine
                .performance_data_from_attrs(&map, &config, &difficulty, score)
                .map_err(engine_error)?;
            pp_by_accuracy.insert(accuracy, attrs.pp);
        }

        Ok(EngineOutput {
            original_difficulty: difficulty_values(original),
            converted_difficulty: difficulty_values(converted),
            difficulty: DifficultyAttributes {
                stars: difficulty.stars,
                max_combo: nonnegative_u32(difficulty.max_combo),
                aim: nonzero(difficulty.aim),
                speed: nonzero(difficulty.speed),
                flashlight: nonzero(difficulty.flashlight),
                slider_factor: nonzero(difficulty.slider_factor),
                stamina: nonzero(difficulty.stamina),
                rhythm: nonzero(difficulty.rhythm),
                color: nonzero(difficulty.color),
                reading: nonzero(difficulty.reading),
                hit_window: hit_windows.great,
            },
            hit_windows,
            catch_spatial_window,
            performance: PerformanceAttributes {
                pp: performance.pp,
                accuracy: nonzero(performance.accuracy),
                aim: nonzero(performance.aim),
                difficulty: nonzero(performance.pp_difficulty),
                flashlight: nonzero(performance.flashlight),
                speed: nonzero(performance.speed),
            },
            maximum_score: ScoreData {
                max_combo: nonnegative_u32(scoring_max_combo),
                maximum_score: maximum_total_score.max(0) as u64,
            },
            pp_by_accuracy,
            strains: strain_map(strains),
        })
    }
}

fn engine_error(error: vosu_lazer_pp::Error) -> BeatmapError {
    BeatmapError::Engine(error.to_string())
}

fn difficulty_values(value: vosu_lazer_pp::BeatmapDifficultyData) -> DifficultyValues {
    DifficultyValues {
        approach_rate: f64::from(value.approach_rate),
        circle_size: f64::from(value.circle_size),
        drain_rate: f64::from(value.drain_rate),
        overall_difficulty: f64::from(value.overall_difficulty),
    }
}

fn nonzero(value: f64) -> Option<f64> {
    (value != 0.0).then_some(value)
}

fn nonnegative_u32(value: i32) -> u32 {
    value.max(0) as u32
}

fn strain_map(value: vosu_lazer_pp::StrainsData) -> BTreeMap<String, PeakStrains> {
    let mut result = BTreeMap::new();
    for (name, peak) in [
        ("aim", value.aim),
        ("aimNoSliders", value.aim_without_sliders),
        ("flashlight", value.flashlight),
        ("speed", value.speed),
        ("reading", value.reading),
        ("color", value.color),
        ("rhythm", value.rhythm),
        ("stamina", value.stamina),
        ("movement", value.movement),
        ("strains", value.strains),
    ] {
        result.insert(
            name.to_owned(),
            PeakStrains {
                section_length: f64::from(peak.section_length),
                values: peak.value,
            },
        );
    }
    result
}

fn structured_mods(mods: &[AppliedMod]) -> Result<Vec<vosu_lazer_pp::StructuredMod>, BeatmapError> {
    mods.iter()
        .map(|value| {
            let settings = match value.settings_json.as_deref() {
                Some(json) => serde_json::from_str(json)
                    .map_err(|error| BeatmapError::Engine(error.to_string()))?,
                None => BTreeMap::new(),
            };
            Ok(vosu_lazer_pp::StructuredMod {
                acronym: value.acronym.clone(),
                settings,
            })
        })
        .collect()
}

fn calculate_mods(mods: &[Mod]) -> Result<Vec<vosu_lazer_pp::StructuredMod>, String> {
    mods.iter()
        .map(|value| {
            let settings = value
                .settings
                .as_ref()
                .map(|settings| {
                    settings
                        .iter()
                        .map(|(key, value)| {
                            let value = match value {
                                Value::Bool(value) => vosu_lazer_pp::ModValue::Bool(*value),
                                Value::Number(value) => value
                                    .as_f64()
                                    .map(vosu_lazer_pp::ModValue::Number)
                                    .ok_or_else(|| format!("mod setting `{key}` is not finite"))?,
                                Value::String(value) => {
                                    vosu_lazer_pp::ModValue::String(value.clone())
                                }
                                _ => {
                                    return Err(format!(
                                        "mod setting `{key}` must be boolean, number, or string"
                                    ));
                                }
                            };
                            Ok((key.clone(), value))
                        })
                        .collect::<Result<BTreeMap<_, _>, String>>()
                })
                .transpose()?
                .unwrap_or_default();
            Ok(vosu_lazer_pp::StructuredMod {
                acronym: value.acronym.clone(),
                settings,
            })
        })
        .collect()
}

fn applied_mods(mods: &[Mod]) -> Vec<AppliedMod> {
    mods.iter()
        .map(|value| AppliedMod {
            acronym: value.acronym.clone(),
            settings_json: value
                .settings
                .as_ref()
                .and_then(|settings| serde_json::to_string(settings).ok()),
        })
        .collect()
}

struct BackendState {
    cache: BeatmapCache<LiveBeatmapEngine>,
    live_key: Option<String>,
    live: Option<Arc<LiveProgressiveBeatmap>>,
    reset_generation_seen: Option<u64>,
    reset_baseline_pending: bool,
    gameplay: GameplayCalculator<LiveGradualProgression>,
    result: ResultCalculator,
    gameplay_error_count: u64,
    result_error_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeatmapReadiness {
    Load,
    RetainTheme,
    RetainEventAndRetry,
    Retry,
}

fn beatmap_readiness(
    game_folder: &Path,
    beatmap_path: &Path,
    menu_folder: Option<&str>,
    filename: Option<&str>,
    client: ClientType,
) -> BeatmapReadiness {
    if client == ClientType::Stable {
        if game_folder.as_os_str().is_empty() {
            return BeatmapReadiness::RetainEventAndRetry;
        }
        let Some(filename) = filename else {
            return BeatmapReadiness::RetainEventAndRetry;
        };
        if !filename.ends_with(".osu") {
            return BeatmapReadiness::RetainEventAndRetry;
        }
        let Some(menu_folder) = menu_folder.filter(|folder| !folder.is_empty()) else {
            return BeatmapReadiness::Retry;
        };
        if menu_folder == "." {
            return BeatmapReadiness::RetainTheme;
        }
        return if beatmap_path.as_os_str().is_empty() {
            BeatmapReadiness::Retry
        } else {
            BeatmapReadiness::Load
        };
    }

    if game_folder.as_os_str().is_empty() {
        BeatmapReadiness::Retry
    } else if filename.is_none_or(str::is_empty) {
        BeatmapReadiness::RetainTheme
    } else if beatmap_path.as_os_str().is_empty() || menu_folder.is_none_or(str::is_empty) {
        BeatmapReadiness::Retry
    } else {
        BeatmapReadiness::Load
    }
}

fn parsed_background_filename(client: ClientType, background_file: &str) -> Option<String> {
    (client == ClientType::Stable).then(|| safe_join([background_file]))
}

fn retained_update(
    request: &StateCalculationRequest,
    rebuild_graph: bool,
    retry_state_key: bool,
) -> Option<BeatmapCalculationUpdate> {
    let previous = &request.current.regular.beatmap;
    previous.parsed.as_ref().map(|parsed| {
        let mut update = BeatmapCalculationUpdate::from_parsed_reusing(
            Arc::clone(parsed),
            request.play_time,
            if rebuild_graph {
                request.current.regular.menu.mp3_length
            } else {
                previous.graph_mp3_length
            },
            Some(previous),
        );
        update.event_only = !rebuild_graph;
        update.graph_event_only = rebuild_graph;
        update.retry_state_key = retry_state_key;
        update
    })
}

impl Default for BackendState {
    fn default() -> Self {
        Self {
            cache: BeatmapCache::new(LiveBeatmapEngine),
            live_key: None,
            live: None,
            reset_generation_seen: None,
            reset_baseline_pending: false,
            gameplay: GameplayCalculator::default(),
            result: ResultCalculator::default(),
            gameplay_error_count: 0,
            result_error_count: 0,
        }
    }
}

/// Stateful production backend. Construct one per reducer/instance so gradual
/// cursors, retry epochs, result cache omissions, and max-achieved PP cannot leak
/// between osu! processes.
pub struct LiveStateCalculationBackend {
    state: Arc<Mutex<BackendState>>,
}

impl Default for LiveStateCalculationBackend {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(BackendState::default())),
        }
    }
}

impl LiveStateCalculationBackend {
    pub fn new() -> Self {
        Self::default()
    }

    fn calculate_sync(
        state: &Mutex<BackendState>,
        request: StateCalculationRequest,
    ) -> Result<StateCalculationResult, StateWorkerError> {
        let mut state = state
            .lock()
            .map_err(|_| StateWorkerError::Transition("calculation lock poisoned".into()))?;
        let calculation_reset =
            state.reset_generation_seen != Some(request.calculation_reset_generation);
        if calculation_reset {
            // Persist reset observation before any fallible map work so stale
            // gradual/result caches cannot survive a transient load failure.
            state.reset_generation_seen = Some(request.calculation_reset_generation);
            state.gameplay = GameplayCalculator::default();
            state.reset_baseline_pending = true;
            state.result.reset();
            state.gameplay_error_count = 0;
            state.result_error_count = 0;
        }
        match beatmap_readiness(
            &request.input.game_folder,
            &request.input.beatmap_path,
            request.input.menu_folder.as_deref(),
            request.input.filename.as_deref(),
            request.input.client_scoring,
        ) {
            BeatmapReadiness::Retry => {
                return Err(StateWorkerError::Transition(
                    "beatmap folder or filename is not ready".to_owned(),
                ));
            }
            BeatmapReadiness::RetainTheme => {
                let beatmap = retained_update(&request, true, false);
                return Ok(StateCalculationResult {
                    key: request.key,
                    beatmap,
                    gameplay: None,
                    result: None,
                });
            }
            BeatmapReadiness::RetainEventAndRetry => {
                let beatmap = retained_update(&request, false, true).unwrap_or_else(|| {
                    BeatmapCalculationUpdate {
                        event_only: true,
                        retry_state_key: true,
                        ..BeatmapCalculationUpdate::default()
                    }
                });
                return Ok(StateCalculationResult {
                    key: request.key,
                    beatmap: Some(beatmap),
                    gameplay: None,
                    result: None,
                });
            }
            BeatmapReadiness::Load => {}
        }

        let requested_mode = ruleset(request.input.mode)?;
        let beatmap_mode = beatmap_ruleset(requested_mode);
        let scoring = scoring_mode(request.input.client_scoring);
        let sanitized_mods = mods_for_calculation(
            &request.input.mods.array,
            request.input.client_scoring == ClientType::Lazer,
        );
        let sanitized_mods_checksum = mods_checksum(&sanitized_mods);
        let mod_set = ModSet::new(
            sanitized_mods_checksum.clone(),
            applied_mods(&sanitized_mods),
            request.input.mods.rate,
        )
        .map_err(transition_error)?;
        let parsed = state
            .cache
            .load(
                BeatmapInput::PathWithIdentity {
                    path: &request.input.beatmap_path,
                    memory_identity: &request.input.checksum,
                },
                beatmap_mode,
                &mod_set,
                scoring,
            )
            .map_err(transition_error)?;
        let mode = progressive_ruleset(parsed.effective_mode);
        let mp3_length = request.current.regular.menu.mp3_length;
        let mut beatmap_update = BeatmapCalculationUpdate::from_parsed_reusing(
            Arc::clone(&parsed),
            request.play_time,
            mp3_length,
            Some(&request.current.regular.beatmap),
        );
        beatmap_update.background_filename = parsed_background_filename(
            request.input.client_scoring,
            &parsed.metadata.background_file,
        );

        let live_key = format!(
            "{}:{}:{}:{:?}",
            parsed.cache_key().source_checksum,
            parsed.effective_mode as u8,
            sanitized_mods_checksum,
            scoring
        );
        if state.live_key.as_deref() != Some(&live_key) {
            let mods = calculate_mods(&sanitized_mods).map_err(StateWorkerError::Transition)?;
            let live = LiveProgressiveBeatmap::new(parsed.source().as_bytes(), mode, &mods)
                .map_err(|error| StateWorkerError::Transition(error.to_string()))?;
            state.live = Some(Arc::new(live));
            state.live_key = Some(live_key);
            state.gameplay = GameplayCalculator::default();
            state.reset_baseline_pending = true;
            state.result.reset();
        }
        if state.reset_baseline_pending {
            state.gameplay.reset_for_retry(
                parsed
                    .engine
                    .pp_by_accuracy
                    .get(&100)
                    .copied()
                    .unwrap_or_default(),
            );
            state.reset_baseline_pending = false;
        }

        let mut gameplay_result = None;
        let mut result_result = None;
        let live = state.live.as_ref().map(Arc::clone);
        match request.phase {
            CalculationPhase::Gameplay => {
                if let (Some(score), Some(live)) = (request.gameplay.as_ref(), live.as_deref()) {
                    let first_object = f64::from(parsed.times.first_object);
                    let before_first = match request.input.client_scoring {
                        ClientType::Stable => request.play_time < first_object,
                        ClientType::Lazer => request.play_time <= first_object,
                    };
                    let retry = request.entry.regular.global.play_time
                        > request.current.regular.global.play_time;
                    let reset_fc_pp = parsed
                        .engine
                        .pp_by_accuracy
                        .get(&100)
                        .copied()
                        .unwrap_or_default();
                    if retry {
                        state.gameplay.reset_for_retry(reset_fc_pp);
                    }
                    if before_first {
                        state.gameplay.reset_quick();
                    }
                    let lazer_combo_rewind = request.input.client_scoring == ClientType::Lazer
                        && request.entry.regular.gameplay.combo
                            > request.current.regular.gameplay.combo;
                    let gate = if !request.calculate_pp {
                        GameplayGate::CalculationDisabled
                    } else if request.input.game_folder.as_os_str().is_empty() {
                        GameplayGate::MissingGameFolder
                    } else {
                        GameplayGate::Active
                    };
                    let cache_key = GameplayCacheKey {
                        beatmap_checksum: request.input.checksum.clone(),
                        mode: mode as u8,
                        mods_checksum: request.input.mods.checksum.clone(),
                        mp3_length: mp3_length.unwrap_or_default(),
                    };
                    let frame = GameplayFrame {
                        gate,
                        reset_fc_pp,
                        key: &cache_key,
                        play_time: request.play_time,
                        mode,
                        client: client_scoring(request.input.client_scoring),
                        score: calculator_long(score.score),
                        combo: score.combo,
                        max_combo: score.max_combo,
                        statistics: statistics(&score.statistics),
                    };
                    let outcome = state.gameplay.update(Some(live), frame);
                    match outcome {
                        CalculationOutcome::Updated => {
                            state.gameplay_error_count = 0;
                            gameplay_result = Some(*state.gameplay.attributes());
                        }
                        CalculationOutcome::RetainedError(error) => {
                            report_retained_error(
                                "gameplay progressive calculation",
                                &mut state.gameplay_error_count,
                                &error,
                            );
                        }
                        CalculationOutcome::Unchanged | CalculationOutcome::Skipped(_) => {}
                    }
                    if retry && gameplay_result.is_none() {
                        gameplay_result = Some(*state.gameplay.attributes());
                    }
                    if lazer_combo_rewind {
                        // lazerInstance publishes this transition first, then
                        // invalidates the cursor so the next tick rebuilds it.
                        state.gameplay.reset_quick();
                    }
                }
            }
            CalculationPhase::Result => {
                if let (Some(result), Some(live)) = (request.result.as_ref(), live.as_deref()) {
                    let frame = ResultFrame {
                        beatmap_checksum: &request.input.checksum,
                        mods_checksum: &result.mods.checksum,
                        mode,
                        player_name: &result.player_name,
                        tournament_rank: None,
                        client: client_scoring(request.input.client_scoring),
                        score: calculator_long(result.score),
                        max_combo: result.max_combo,
                        map_max_combo: i64::from(parsed.engine.difficulty.max_combo),
                        displayed_accuracy_percent: result.accuracy,
                        statistics: statistics(&result.statistics),
                    };
                    match state.result.update(Some(live), &frame) {
                        CalculationOutcome::Updated => {
                            state.result_error_count = 0;
                            result_result = Some(state.result.performance());
                        }
                        CalculationOutcome::RetainedError(error) => {
                            report_retained_error(
                                "result screen calculation",
                                &mut state.result_error_count,
                                &error,
                            );
                        }
                        CalculationOutcome::Unchanged | CalculationOutcome::Skipped(_) => {}
                    }
                }
            }
            CalculationPhase::Menu | CalculationPhase::Editor => {}
        }

        Ok(StateCalculationResult {
            key: request.key,
            beatmap: Some(beatmap_update),
            gameplay: gameplay_result,
            result: result_result,
        })
    }
}

impl StateCalculationBackend for LiveStateCalculationBackend {
    fn calculate<'a>(&'a self, request: StateCalculationRequest) -> StateCalculationFuture<'a> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || Self::calculate_sync(&state, request))
                .await
                .map_err(|error| {
                    StateWorkerError::Transition(format!(
                        "calculation worker terminated unexpectedly: {error}"
                    ))
                })?
        })
    }
}

fn report_retained_error(kind: &'static str, count: &mut u64, error: &str) {
    *count = count.saturating_add(1);
    if *count <= 10 {
        tracing::debug!(calculation = kind, failure_count = *count, %error);
    } else {
        tracing::error!(calculation = kind, failure_count = *count, %error);
    }
}

fn transition_error(error: impl std::fmt::Display) -> StateWorkerError {
    StateWorkerError::Transition(error.to_string())
}
/// Mirrors the N-API / C# conversion used when JavaScript `Number` scores
/// cross into the calculator's signed 64-bit score model.
fn calculator_long(value: f64) -> i64 {
    if value.is_nan() {
        0
    } else if value >= i64::MAX as f64 {
        i64::MAX
    } else if value <= i64::MIN as f64 {
        i64::MIN
    } else {
        value.trunc() as i64
    }
}

fn ruleset(mode: i64) -> Result<Ruleset, StateWorkerError> {
    match mode {
        0 => Ok(Ruleset::Osu),
        1 => Ok(Ruleset::Taiko),
        2 => Ok(Ruleset::Catch),
        3 => Ok(Ruleset::Mania),
        _ => Err(StateWorkerError::Transition(format!(
            "unsupported ruleset {mode}"
        ))),
    }
}

fn beatmap_ruleset(mode: Ruleset) -> BeatmapRuleset {
    match mode {
        Ruleset::Osu => BeatmapRuleset::Osu,
        Ruleset::Taiko => BeatmapRuleset::Taiko,
        Ruleset::Catch => BeatmapRuleset::Catch,
        Ruleset::Mania => BeatmapRuleset::Mania,
    }
}
fn progressive_ruleset(mode: BeatmapRuleset) -> Ruleset {
    match mode {
        BeatmapRuleset::Osu => Ruleset::Osu,
        BeatmapRuleset::Taiko => Ruleset::Taiko,
        BeatmapRuleset::Catch => Ruleset::Catch,
        BeatmapRuleset::Mania => Ruleset::Mania,
    }
}

fn live_ruleset(mode: BeatmapRuleset) -> vosu_lazer_pp::Ruleset {
    match mode {
        BeatmapRuleset::Osu => vosu_lazer_pp::Ruleset::Osu,
        BeatmapRuleset::Taiko => vosu_lazer_pp::Ruleset::Taiko,
        BeatmapRuleset::Catch => vosu_lazer_pp::Ruleset::Catch,
        BeatmapRuleset::Mania => vosu_lazer_pp::Ruleset::Mania,
    }
}

fn scoring_mode(client: ClientType) -> ScoringMode {
    match client {
        ClientType::Stable => ScoringMode::Stable,
        ClientType::Lazer => ScoringMode::Lazer,
    }
}

fn client_scoring(client: ClientType) -> ClientScoring {
    match client {
        ClientType::Stable => ClientScoring::Stable,
        ClientType::Lazer => ClientScoring::Lazer,
    }
}

fn statistics(value: &Statistics) -> HitStatistics {
    HitStatistics {
        perfect: value.perfect,
        great: value.great,
        good: value.good,
        ok: value.ok,
        meh: value.meh,
        miss: value.miss,
        small_tick_miss: value.small_tick_miss,
        small_tick_hit: value.small_tick_hit,
        large_tick_miss: value.large_tick_miss,
        large_tick_hit: value.large_tick_hit,
        small_bonus: value.small_bonus,
        large_bonus: value.large_bonus,
        ignore_miss: value.ignore_miss,
        ignore_hit: value.ignore_hit,
        combo_break: value.combo_break,
        slider_tail_hit: value.slider_tail_hit,
    }
}

#[cfg(test)]
mod hit_window_tests {
    use super::*;

    const OSU_OD5: &str = r#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:5
ApproachRate:5

[TimingPoints]
0,500,4,2,1,80,1,0

[HitObjects]
256,192,1000,1,0,0:0:0:0:
"#;

    #[test]
    fn live_engine_scales_map_aware_hit_windows_once_for_dt() {
        let output = LiveBeatmapEngine
            .calculate(EngineRequest {
                content: OSU_OD5,
                original_mode: BeatmapRuleset::Osu,
                effective_mode: BeatmapRuleset::Osu,
                mods: &[AppliedMod {
                    acronym: "DT".to_owned(),
                    settings_json: None,
                }],
                clock_rate: 1.5,
                scoring_mode: ScoringMode::Lazer,
            })
            .unwrap();

        let windows = output.hit_windows;
        assert_eq!(windows.great, Some(33.0));
        assert_eq!(output.difficulty.hit_window, windows.great);
        assert!((windows.ok.unwrap() - 66.333_333_333_333_33).abs() < 1e-9);
        assert!((windows.meh.unwrap() - 99.666_666_666_666_67).abs() < 1e-9);
        assert!((windows.miss.unwrap() - 266.666_666_666_666_7).abs() < 1e-9);
        assert_eq!(windows.good, None);
        assert_eq!(windows.perfect, None);
    }
}

#[cfg(test)]
mod tests;

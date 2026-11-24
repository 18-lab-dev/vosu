//! Shared chart engine for resource consumers and the state calculation backend.
use crate::beatmap::{
    AppliedMod, BeatmapEngine, BeatmapError, DifficultyAttributes, DifficultyValues, EngineOutput,
    EngineRequest, PeakStrains, PerformanceAttributes, Ruleset as BeatmapRuleset, ScoreData,
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone, Copy, Default)]
pub struct LiveBeatmapEngine;

impl BeatmapEngine for LiveBeatmapEngine {
    fn calculate(&self, request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
        let engine = vosu_pp::LiveLazerEngine;
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
        let map_hit_windows = vosu_pp::hit_windows_for_map(&map).map_err(engine_error)?;
        let catch_spatial_window = vosu_pp::catch_spatial_window_for_map(&map);
        let project_window = |window: Option<f64>| window.map(|window| window / request.clock_rate);
        let hit_windows = vosu_pp::HitWindows {
            miss: project_window(map_hit_windows.miss),
            meh: project_window(map_hit_windows.meh),
            ok: project_window(map_hit_windows.ok),
            good: project_window(map_hit_windows.good),
            great: project_window(map_hit_windows.great),
            perfect: project_window(map_hit_windows.perfect),
        };
        let config = vosu_pp::DifficultyConfig::default();
        let original = engine.original_beatmap_difficulty(&map);
        let converted = display_converted_difficulty(
            request.original_mode,
            request.effective_mode,
            request.mods,
            original,
            engine.beatmap_difficulty(&map),
        );
        let (difficulty, strains) = engine
            .difficulty_and_strains_data(&map, &config)
            .map_err(engine_error)?;
        let mut maximum = engine
            .create_score_from_attrs(&map, &difficulty, 1.0)
            .map_err(engine_error)?;
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

        let mut pp_by_accuracy = BTreeMap::new();
        for accuracy in 90..=100 {
            let mut score = engine
                .create_score_from_attrs(&map, &difficulty, f64::from(accuracy) / 100.0)
                .map_err(engine_error)?;
            score.max_combo = difficulty.max_combo;
            let attrs = engine
                .performance_data_from_attrs(&map, &config, &difficulty, score)
                .map_err(engine_error)?;
            pp_by_accuracy.insert(accuracy, attrs.pp);
        }
        let live_prepared = Arc::new(vosu_pp::PreparedBeatmap::new(
            map,
            config,
            difficulty.clone(),
            maximum,
        ));

        Ok(EngineOutput {
            original_difficulty: difficulty_values(original),
            live_prepared: Some(live_prepared),
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
                hit_window: hit_windows.great.and_then(nonzero),
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

fn engine_error(error: vosu_pp::Error) -> BeatmapError {
    BeatmapError::Engine(error.to_string())
}

fn display_converted_difficulty(
    original_mode: BeatmapRuleset,
    effective_mode: BeatmapRuleset,
    mods: &[AppliedMod],
    original: vosu_pp::BeatmapDifficultyData,
    mut converted: vosu_pp::BeatmapDifficultyData,
) -> vosu_pp::BeatmapDifficultyData {
    if original_mode != BeatmapRuleset::Osu || effective_mode != BeatmapRuleset::Mania {
        return converted;
    }
    let mut circle_size =
        (f64::from(original.overall_difficulty).round_ties_even() + 1.0).clamp(4.0, 7.0);
    let mut dual_stages = false;
    for mod_ in mods {
        if mod_.acronym.eq_ignore_ascii_case("DS") {
            dual_stages = true;
        } else if let Some(key_count) = mania_key_count(&mod_.acronym) {
            circle_size = f64::from(key_count);
        }
    }
    converted.circle_size = (circle_size * if dual_stages { 2.0 } else { 1.0 }) as f32;
    converted
}

fn mania_key_count(acronym: &str) -> Option<u8> {
    let digits = acronym
        .strip_suffix('K')
        .or_else(|| acronym.strip_suffix('k'))?;
    let count = digits.parse::<u8>().ok()?;
    (1..=10).contains(&count).then_some(count)
}

fn difficulty_values(value: vosu_pp::BeatmapDifficultyData) -> DifficultyValues {
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

fn strain_map(value: vosu_pp::StrainsData) -> BTreeMap<String, PeakStrains> {
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
                section_length: peak.section_length,
                values: peak.value,
            },
        );
    }
    result
}

fn structured_mods(mods: &[AppliedMod]) -> Result<Vec<vosu_pp::StructuredMod>, BeatmapError> {
    mods.iter()
        .map(|value| {
            let settings = match value.settings_json.as_deref() {
                Some(json) => serde_json::from_str(json)
                    .map_err(|error| BeatmapError::Engine(error.to_string()))?,
                None => BTreeMap::new(),
            };
            Ok(vosu_pp::StructuredMod {
                acronym: value.acronym.clone(),
                settings,
            })
        })
        .collect()
}

fn live_ruleset(mode: BeatmapRuleset) -> vosu_pp::Ruleset {
    match mode {
        BeatmapRuleset::Osu => vosu_pp::Ruleset::Osu,
        BeatmapRuleset::Taiko => vosu_pp::Ruleset::Taiko,
        BeatmapRuleset::Catch => vosu_pp::Ruleset::Catch,
        BeatmapRuleset::Mania => vosu_pp::Ruleset::Mania,
    }
}

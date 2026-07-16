//! Performance-engine boundaries.
//!
//! `LegacyRosuEngine` intentionally maps the rosu-pp-js 4.0.1 builder in its
//! original setter order. The live calculator is re-exported from the separate
//! `vosu-lazer-pp` crate so callers cannot accidentally route both contracts
//! through this legacy builder.

use rosu_map::section::general::GameMode;
use rosu_mods::serde::GameModsSeed;
use rosu_pp::{
    Beatmap, GameMods, Performance,
    any::{
        DifficultyAttributes, HitResultPriority as RosuHitResultPriority, PerformanceAttributes,
        ScoreState,
    },
};
use serde::{Deserialize, Serialize, de::DeserializeSeed};
use thiserror::Error;

#[allow(unused_imports)]
pub use vosu_lazer_pp::{
    DifficultyConfig as LiveDifficultyConfig, Error as LiveError, GradualDifficulty, HitResult,
    LiveLazerEngine, LiveScoreState, ModValue, ParsedBeatmap as LiveBeatmap, Ruleset, ScoreTotals,
    StructuredMod, hit_window,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HitResultPriority {
    #[default]
    BestCase,
    WorstCase,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LegacyMods {
    Bits(u32),
    StructuredSingle(StructuredMod),
    Structured(Vec<StructuredMod>),
}

impl Default for LegacyMods {
    fn default() -> Self {
        Self::Bits(0)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LegacyPerformanceRequest {
    pub mode: Option<Ruleset>,
    pub lazer: Option<bool>,
    pub mods: LegacyMods,
    pub clock_rate: Option<f64>,
    pub ar: Option<f32>,
    pub cs: Option<f32>,
    pub hp: Option<f32>,
    pub od: Option<f32>,
    pub passed_objects: Option<u32>,
    pub combo: Option<u32>,
    pub n_misses: Option<u32>,
    pub n100: Option<u32>,
    pub n300: Option<u32>,
    pub n50: Option<u32>,
    pub n_geki: Option<u32>,
    pub n_katu: Option<u32>,
    pub slider_end_hits: Option<u32>,
    pub small_tick_hits: Option<u32>,
    pub large_tick_hits: Option<u32>,
    pub accuracy: Option<f64>,
    pub hitresult_priority: HitResultPriority,
}

#[derive(Debug, Error)]
pub enum LegacyEngineError {
    #[error("failed to parse beatmap: {0}")]
    Parse(String),
    #[error("cannot convert {from:?} beatmap to {to:?}")]
    Convert { from: Ruleset, to: Ruleset },
    #[error("invalid mods: {0}")]
    Mods(String),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyScoreState {
    pub max_combo: u32,
    pub osu_large_tick_hits: u32,
    pub osu_small_tick_hits: u32,
    pub slider_end_hits: u32,
    pub n_geki: u32,
    pub n_katu: u32,
    pub n300: u32,
    pub n100: u32,
    pub n50: u32,
    pub misses: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyDifficultyAttributes {
    #[serde(serialize_with = "serialize_ruleset_numeric")]
    pub mode: Ruleset,
    pub stars: f64,
    pub is_convert: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim_difficult_slider_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flashlight: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slider_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim_top_weighted_slider_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_top_weighted_slider_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_note_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim_difficult_strain_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_difficult_strain_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested_score_per_object: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_score_base_multiplier: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_legacy_combo_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hp: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_circles: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_sliders: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_large_ticks: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_spinners: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stamina: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rhythm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_fruits: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_droplets: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_tiny_droplets: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_objects: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_hold_notes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ar: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preempt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub great_hit_window: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok_hit_window: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meh_hit_window: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mono_stamina_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mechanical_difficulty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consistency_factor: Option<f64>,
    pub max_combo: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyPerformanceResult {
    pub difficulty: LegacyDifficultyAttributes,
    pub state: LegacyScoreState,
    pub pp: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp_aim: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp_flashlight: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp_speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp_accuracy: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_miss_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_unstable_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_deviation: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub combo_based_estimated_miss_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score_based_estimated_miss_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim_estimated_slider_breaks: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed_estimated_slider_breaks: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp_difficulty: Option<f64>,
}

fn serialize_ruleset_numeric<S>(mode: &Ruleset, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_u8(match mode {
        Ruleset::Osu => 0,
        Ruleset::Taiko => 1,
        Ruleset::Catch => 2,
        Ruleset::Mania => 3,
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LegacyRosuEngine;

impl LegacyRosuEngine {
    pub fn calculate(
        &self,
        bytes: &[u8],
        request: &LegacyPerformanceRequest,
    ) -> Result<LegacyPerformanceResult, LegacyEngineError> {
        let mut map =
            Beatmap::from_bytes(bytes).map_err(|err| LegacyEngineError::Parse(err.to_string()))?;
        if let Some(mode) = request.mode {
            let from = ruleset(map.mode);
            map = map
                .convert(game_mode(mode), &GameMods::default())
                .map_err(|_| LegacyEngineError::Convert { from, to: mode })?;
        }
        let mods = legacy_mods(map.mode, &request.mods)?;

        let mut perf = Performance::new(&map);
        if let Some(value) = request.accuracy {
            perf = perf.accuracy(value);
        }
        if let Some(value) = request.combo {
            perf = perf.combo(value);
        }
        if let Some(value) = request.large_tick_hits {
            perf = perf.large_tick_hits(value);
        }
        if let Some(value) = request.small_tick_hits {
            perf = perf.small_tick_hits(value);
        }
        if let Some(value) = request.slider_end_hits {
            perf = perf.slider_end_hits(value);
        }
        if let Some(value) = request.n_geki {
            perf = perf.n_geki(value);
        }
        if let Some(value) = request.n_katu {
            perf = perf.n_katu(value);
        }
        if let Some(value) = request.n300 {
            perf = perf.n300(value);
        }
        if let Some(value) = request.n100 {
            perf = perf.n100(value);
        }
        if let Some(value) = request.n50 {
            perf = perf.n50(value);
        }
        if let Some(value) = request.n_misses {
            perf = perf.misses(value);
        }
        perf = perf.hitresult_priority(match request.hitresult_priority {
            HitResultPriority::BestCase => RosuHitResultPriority::BestCase,
            HitResultPriority::WorstCase => RosuHitResultPriority::WorstCase,
        });
        perf = perf.mods(mods);
        if let Some(value) = request.clock_rate {
            perf = perf.clock_rate(value);
        }
        if let Some(value) = request.ar {
            perf = perf.ar(value, false);
        }
        if let Some(value) = request.cs {
            perf = perf.cs(value, false);
        }
        if let Some(value) = request.hp {
            perf = perf.hp(value, false);
        }
        if let Some(value) = request.od {
            perf = perf.od(value, false);
        }
        if let Some(value) = request.passed_objects {
            perf = perf.passed_objects(value);
        }
        if let Some(value) = request.lazer {
            perf = perf.lazer(value);
        }

        let state = perf.generate_state();
        let attrs = perf.calculate();
        Ok(map_result(attrs, state))
    }
}

fn legacy_mods(mode: GameMode, mods: &LegacyMods) -> Result<GameMods, LegacyEngineError> {
    match mods {
        LegacyMods::Bits(bits) => Ok((*bits).into()),
        LegacyMods::StructuredSingle(mod_) => structured_mods(mode, std::slice::from_ref(mod_)),
        LegacyMods::Structured(mods) => structured_mods(mode, mods),
    }
}

fn structured_mods(mode: GameMode, mods: &[StructuredMod]) -> Result<GameMods, LegacyEngineError> {
    let mut value =
        serde_json::to_value(mods).map_err(|err| LegacyEngineError::Mods(err.to_string()))?;

    if let serde_json::Value::Array(items) = &mut value {
        for item in items {
            if let Some(serde_json::Value::Object(settings)) = item.get_mut("settings") {
                let original = std::mem::take(settings);
                settings.extend(
                    original
                        .into_iter()
                        .map(|(key, value)| (camel_to_snake(&key), value)),
                );
            }
        }
    }

    let json =
        serde_json::to_string(&value).map_err(|err| LegacyEngineError::Mods(err.to_string()))?;
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    let mode = match mode {
        GameMode::Osu => rosu_mods::GameMode::Osu,
        GameMode::Taiko => rosu_mods::GameMode::Taiko,
        GameMode::Catch => rosu_mods::GameMode::Catch,
        GameMode::Mania => rosu_mods::GameMode::Mania,
    };

    GameModsSeed::Mode {
        mode,
        deny_unknown_fields: true,
    }
    .deserialize(&mut deserializer)
    .map(Into::into)
    .map_err(|err| LegacyEngineError::Mods(err.to_string()))
}

fn camel_to_snake(input: &str) -> String {
    let mut output = String::with_capacity(input.len() + 4);

    for ch in input.chars() {
        if ch.is_ascii_uppercase() {
            if !output.is_empty() {
                output.push('_');
            }

            output.push(ch.to_ascii_lowercase());
        } else {
            output.push(ch);
        }
    }

    output
}

const fn game_mode(mode: Ruleset) -> GameMode {
    match mode {
        Ruleset::Osu => GameMode::Osu,
        Ruleset::Taiko => GameMode::Taiko,
        Ruleset::Catch => GameMode::Catch,
        Ruleset::Mania => GameMode::Mania,
    }
}
const fn ruleset(mode: GameMode) -> Ruleset {
    match mode {
        GameMode::Osu => Ruleset::Osu,
        GameMode::Taiko => Ruleset::Taiko,
        GameMode::Catch => Ruleset::Catch,
        GameMode::Mania => Ruleset::Mania,
    }
}

fn score_state(s: ScoreState) -> LegacyScoreState {
    LegacyScoreState {
        max_combo: s.max_combo,
        osu_large_tick_hits: s.osu_large_tick_hits,
        osu_small_tick_hits: s.osu_small_tick_hits,
        slider_end_hits: s.slider_end_hits,
        n_geki: s.n_geki,
        n_katu: s.n_katu,
        n300: s.n300,
        n100: s.n100,
        n50: s.n50,
        misses: s.misses,
    }
}

fn map_result(attrs: PerformanceAttributes, state: ScoreState) -> LegacyPerformanceResult {
    match attrs {
        PerformanceAttributes::Osu(a) => LegacyPerformanceResult {
            difficulty: map_difficulty(DifficultyAttributes::Osu(a.difficulty)),
            state: score_state(state),
            pp: a.pp,
            pp_aim: Some(a.pp_aim),
            pp_flashlight: Some(a.pp_flashlight),
            pp_speed: Some(a.pp_speed),
            pp_accuracy: Some(a.pp_acc),
            effective_miss_count: Some(a.effective_miss_count),
            speed_deviation: a.speed_deviation,
            combo_based_estimated_miss_count: Some(a.combo_based_estimated_miss_count),
            score_based_estimated_miss_count: a.score_based_estimated_miss_count,
            aim_estimated_slider_breaks: Some(a.aim_estimated_slider_breaks),
            speed_estimated_slider_breaks: Some(a.speed_estimated_slider_breaks),
            ..Default::default()
        },
        PerformanceAttributes::Taiko(a) => LegacyPerformanceResult {
            difficulty: map_difficulty(DifficultyAttributes::Taiko(a.difficulty)),
            state: score_state(state),
            pp: a.pp,
            pp_accuracy: Some(a.pp_acc),
            pp_difficulty: Some(a.pp_difficulty),
            estimated_unstable_rate: a.estimated_unstable_rate,
            ..Default::default()
        },
        PerformanceAttributes::Catch(a) => LegacyPerformanceResult {
            difficulty: map_difficulty(DifficultyAttributes::Catch(a.difficulty)),
            state: score_state(state),
            pp: a.pp,
            ..Default::default()
        },
        PerformanceAttributes::Mania(a) => LegacyPerformanceResult {
            difficulty: map_difficulty(DifficultyAttributes::Mania(a.difficulty)),
            state: score_state(state),
            pp: a.pp,
            pp_difficulty: Some(a.pp_difficulty),
            ..Default::default()
        },
    }
}

fn map_difficulty(attrs: DifficultyAttributes) -> LegacyDifficultyAttributes {
    match attrs {
        DifficultyAttributes::Osu(a) => LegacyDifficultyAttributes {
            mode: Ruleset::Osu,
            stars: a.stars,
            max_combo: a.max_combo,
            aim: Some(a.aim),
            aim_difficult_slider_count: Some(a.aim_difficult_slider_count),
            speed: Some(a.speed),
            flashlight: Some(a.flashlight),
            slider_factor: Some(a.slider_factor),
            aim_top_weighted_slider_factor: Some(a.aim_top_weighted_slider_factor),
            speed_top_weighted_slider_factor: Some(a.speed_top_weighted_slider_factor),
            speed_note_count: Some(a.speed_note_count),
            aim_difficult_strain_count: Some(a.aim_difficult_strain_count),
            speed_difficult_strain_count: Some(a.speed_difficult_strain_count),
            nested_score_per_object: Some(a.nested_score_per_object),
            legacy_score_base_multiplier: Some(a.legacy_score_base_multiplier),
            maximum_legacy_combo_score: Some(a.maximum_legacy_combo_score),
            hp: Some(a.hp),
            ar: Some(a.ar),
            great_hit_window: Some(a.great_hit_window),
            ok_hit_window: Some(a.ok_hit_window),
            meh_hit_window: Some(a.meh_hit_window),
            n_circles: Some(a.n_circles),
            n_sliders: Some(a.n_sliders),
            n_large_ticks: Some(a.n_large_ticks),
            n_spinners: Some(a.n_spinners),
            ..Default::default()
        },
        DifficultyAttributes::Taiko(a) => LegacyDifficultyAttributes {
            mode: Ruleset::Taiko,
            stars: a.stars,
            is_convert: a.is_convert,
            max_combo: a.max_combo,
            stamina: Some(a.stamina),
            rhythm: Some(a.rhythm),
            color: Some(a.color),
            reading: Some(a.reading),
            great_hit_window: Some(a.great_hit_window),
            ok_hit_window: Some(a.ok_hit_window),
            mono_stamina_factor: Some(a.mono_stamina_factor),
            mechanical_difficulty: Some(a.mechanical_difficulty),
            consistency_factor: Some(a.consistency_factor),
            ..Default::default()
        },
        DifficultyAttributes::Catch(a) => LegacyDifficultyAttributes {
            mode: Ruleset::Catch,
            stars: a.stars,
            is_convert: a.is_convert,
            max_combo: a.max_combo(),
            preempt: Some(a.preempt),
            n_fruits: Some(a.n_fruits),
            n_droplets: Some(a.n_droplets),
            n_tiny_droplets: Some(a.n_tiny_droplets),
            ..Default::default()
        },
        DifficultyAttributes::Mania(a) => LegacyDifficultyAttributes {
            mode: Ruleset::Mania,
            stars: a.stars,
            is_convert: a.is_convert,
            max_combo: a.max_combo,
            n_objects: Some(a.n_objects),
            n_hold_notes: Some(a.n_hold_notes),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HitResultPriority, LegacyDifficultyAttributes, LegacyMods, LegacyPerformanceRequest,
        LegacyRosuEngine, Ruleset,
    };

    const MAP: &[u8] = br#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1.4
SliderTickRate:1

[TimingPoints]
0,500,4,2,1,100,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
256,192,1500,2,0,B|320:192,1,140
448,192,2200,1,0,0:0:0:0:
"#;

    #[test]
    fn legacy_json_uses_rosu_js_mode_and_field_order() {
        let attributes = LegacyDifficultyAttributes {
            mode: Ruleset::Taiko,
            stars: 1.5,
            is_convert: true,
            stamina: Some(0.5),
            max_combo: 42,
            ..Default::default()
        };

        assert_eq!(
            serde_json::to_string(&attributes).unwrap(),
            r#"{"mode":1,"stars":1.5,"isConvert":true,"stamina":0.5,"maxCombo":42}"#,
        );
    }

    #[test]
    fn legacy_mods_accept_one_structured_object() {
        let request: LegacyPerformanceRequest =
            serde_json::from_str(r#"{"mods":{"acronym":"HD","settings":{}}}"#).unwrap();

        assert!(matches!(request.mods, LegacyMods::StructuredSingle(_)));
    }
    #[test]
    fn legacy_builder_maps_the_complete_request() {
        let result = LegacyRosuEngine
            .calculate(
                MAP,
                &LegacyPerformanceRequest {
                    mode: Some(Ruleset::Osu),
                    lazer: Some(false),
                    mods: LegacyMods::Bits(8 | 64),
                    clock_rate: Some(1.25),
                    ar: Some(9.5),
                    cs: Some(4.2),
                    hp: Some(6.0),
                    od: Some(8.5),
                    passed_objects: Some(2),
                    combo: Some(1),
                    n_misses: Some(0),
                    n100: Some(1),
                    n300: Some(1),
                    n50: Some(0),
                    n_geki: Some(0),
                    n_katu: Some(0),
                    slider_end_hits: Some(1),
                    small_tick_hits: Some(1),
                    large_tick_hits: Some(1),
                    accuracy: Some(95.0),
                    hitresult_priority: HitResultPriority::WorstCase,
                },
            )
            .unwrap();

        assert!(result.pp.is_finite());
        assert_eq!(result.difficulty.mode, Ruleset::Osu);
        assert_eq!(result.state.max_combo, 1);
    }
}

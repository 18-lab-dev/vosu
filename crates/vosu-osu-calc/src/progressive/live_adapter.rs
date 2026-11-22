use std::fmt;
use std::sync::Arc;

use super::{
    DifficultyAttributes, GradualProgression, PerformanceAttributes, ProgressiveBeatmap, Ruleset,
    ScoreInput,
};

#[derive(Debug)]
pub enum LiveAdapterError {
    Engine(vosu_pp::Error),
    ScoreField(&'static str, i64),
}

impl fmt::Display for LiveAdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(error) => error.fmt(f),
            Self::ScoreField(field, value) => {
                write!(f, "score field `{field}` is outside i32 range: {value}")
            }
        }
    }
}

impl std::error::Error for LiveAdapterError {}

impl From<vosu_pp::Error> for LiveAdapterError {
    fn from(value: vosu_pp::Error) -> Self {
        Self::Engine(value)
    }
}

fn live_ruleset(mode: Ruleset) -> vosu_pp::Ruleset {
    match mode {
        Ruleset::Osu => vosu_pp::Ruleset::Osu,
        Ruleset::Taiko => vosu_pp::Ruleset::Taiko,
        Ruleset::Catch => vosu_pp::Ruleset::Catch,
        Ruleset::Mania => vosu_pp::Ruleset::Mania,
    }
}

pub(super) fn live_score(score: &ScoreInput) -> Result<vosu_pp::ScoreInfoData, LiveAdapterError> {
    fn i32_field(name: &'static str, value: i64) -> Result<i32, LiveAdapterError> {
        value
            .try_into()
            .map_err(|_| LiveAdapterError::ScoreField(name, value))
    }

    Ok(vosu_pp::ScoreInfoData {
        total_score: score.total_score,
        is_legacy_score: score.is_legacy_score,
        accuracy: score.accuracy,
        max_combo: i32_field("maxCombo", score.max_combo)?,
        slider_end_hits: i32_field("sliderEndHits", score.slider_end_hits)?,
        combo_breaks: i32_field("comboBreaks", score.combo_breaks)?,
        ignore_hits: i32_field("ignoreHits", score.ignore_hits)?,
        ignore_misses: i32_field("ignoreMisses", score.ignore_misses)?,
        large_bonuses: i32_field("largeBonuses", score.large_bonuses)?,
        small_bonuses: i32_field("smallBonuses", score.small_bonuses)?,
        large_tick_hits: i32_field("largeTickHits", score.large_tick_hits)?,
        large_tick_misses: i32_field("largeTickMisses", score.large_tick_misses)?,
        small_tick_hits: i32_field("smallTickHits", score.small_tick_hits)?,
        small_tick_misses: i32_field("smallTickMisses", score.small_tick_misses)?,
        perfects: i32_field("perfects", score.perfects)?,
        greats: i32_field("greats", score.greats)?,
        goods: i32_field("goods", score.goods)?,
        oks: i32_field("oks", score.oks)?,
        mehs: i32_field("mehs", score.mehs)?,
        misses: i32_field("misses", score.misses)?,
    })
}

fn performance_attributes(value: vosu_pp::PerformanceAttrsData) -> PerformanceAttributes {
    PerformanceAttributes {
        pp: value.pp,
        accuracy: value.accuracy,
        aim: value.aim,
        difficulty: value.pp_difficulty,
        flashlight: value.flashlight,
        speed: value.speed,
        reading: value.reading,
    }
}

/// Concrete adapter around the pinned native live calculator.
///
/// The parsed/converted/modded map and full difficulty are immutable. A fresh
/// gradual calculator is created only when the source gameplay cache requires
/// it, while full/current/result performance all receive the complete signed
/// score DTO including explicit accuracy.
pub struct LiveProgressiveBeatmap {
    engine: vosu_pp::LiveLazerEngine,
    map: Arc<vosu_pp::ParsedBeatmap>,
    config: vosu_pp::DifficultyConfig,
    full_difficulty: vosu_pp::DifficultyAttrsData,
    max_score: ScoreInput,
}

impl LiveProgressiveBeatmap {
    pub fn from_prepared(prepared: Arc<vosu_pp::PreparedBeatmap>) -> Self {
        let mut max_score = score_input(*prepared.max_score());
        max_score.max_combo = i64::from(prepared.difficulty().max_combo);
        Self {
            engine: vosu_pp::LiveLazerEngine,
            map: prepared.map().clone(),
            config: prepared.config().clone(),
            full_difficulty: prepared.difficulty().clone(),
            max_score,
        }
    }

    pub fn new(
        bytes: &[u8],
        mode: Ruleset,
        mods: &[vosu_pp::StructuredMod],
    ) -> Result<Self, LiveAdapterError> {
        let engine = vosu_pp::LiveLazerEngine;
        let parsed = engine.parse(bytes)?;
        let mut map = if parsed.mode() == live_ruleset(mode) {
            parsed
        } else {
            engine.convert(&parsed, live_ruleset(mode))?
        };
        engine.apply_mods(&mut map, mods)?;
        let config = vosu_pp::DifficultyConfig::default();
        let full_difficulty = engine.difficulty_data(&map, &config)?;
        let mut max_score =
            score_input(engine.create_score_from_attrs(&map, &full_difficulty, 1.0)?);
        max_score.max_combo = i64::from(full_difficulty.max_combo);
        Ok(Self {
            engine,
            map: Arc::new(map),
            config,
            full_difficulty,
            max_score,
        })
    }
}

fn score_input(score: vosu_pp::ScoreInfoData) -> ScoreInput {
    ScoreInput {
        total_score: score.total_score,
        is_legacy_score: score.is_legacy_score,
        accuracy: score.accuracy,
        max_combo: i64::from(score.max_combo),
        perfects: i64::from(score.perfects),
        greats: i64::from(score.greats),
        goods: i64::from(score.goods),
        oks: i64::from(score.oks),
        mehs: i64::from(score.mehs),
        misses: i64::from(score.misses),
        slider_end_hits: i64::from(score.slider_end_hits),
        small_tick_hits: i64::from(score.small_tick_hits),
        small_tick_misses: i64::from(score.small_tick_misses),
        large_tick_hits: i64::from(score.large_tick_hits),
        large_tick_misses: i64::from(score.large_tick_misses),
        large_bonuses: i64::from(score.large_bonuses),
        small_bonuses: i64::from(score.small_bonuses),
        combo_breaks: i64::from(score.combo_breaks),
        ignore_hits: i64::from(score.ignore_hits),
        ignore_misses: i64::from(score.ignore_misses),
    }
}

pub struct LiveGradualProgression {
    gradual: vosu_pp::GradualDifficulty,
}

impl GradualProgression for LiveGradualProgression {
    type Error = LiveAdapterError;
    type Difficulty = vosu_pp::DifficultyAttrsData;

    fn skip_to_time(&mut self, play_time: f64) -> Result<usize, Self::Error> {
        Ok(self.gradual.skip_to_time(play_time))
    }

    fn difficulty(&mut self) -> Result<Self::Difficulty, Self::Error> {
        self.gradual.create_difficulty_attrs().map_err(Into::into)
    }

    fn progressive_accuracy(&mut self, score: &ScoreInput) -> Result<f64, Self::Error> {
        self.gradual
            .calculate_progressive_accuracy(live_score(score)?)
            .map_err(Into::into)
    }

    fn calculate_performance(
        &mut self,
        score: &ScoreInput,
    ) -> Result<PerformanceAttributes, Self::Error> {
        self.gradual
            .performance_from_score(live_score(score)?)
            .map(performance_attributes)
            .map_err(Into::into)
    }
}

impl ProgressiveBeatmap for LiveProgressiveBeatmap {
    type Error = LiveAdapterError;
    type Difficulty = vosu_pp::DifficultyAttrsData;
    type Gradual = LiveGradualProgression;

    fn create_gradual(&self) -> Result<Self::Gradual, Self::Error> {
        self.engine
            .gradual_difficulty(&self.map, &self.config)
            .map(|gradual| LiveGradualProgression { gradual })
            .map_err(Into::into)
    }

    fn full_difficulty(&self) -> &Self::Difficulty {
        &self.full_difficulty
    }

    fn difficulty_summary(&self, difficulty: &Self::Difficulty) -> DifficultyAttributes {
        DifficultyAttributes {
            stars: difficulty.stars,
            max_combo: i64::from(difficulty.max_combo),
            n_sliders: i64::from(difficulty.n_sliders),
            n_objects: i64::from(difficulty.n_objects),
            nested_score_per_object: difficulty.nested_score_per_object,
            maximum_legacy_combo_score: difficulty.maximum_legacy_combo_score,
        }
    }

    fn max_score(&self) -> &ScoreInput {
        &self.max_score
    }

    fn calculate_accuracy(&self, score: &ScoreInput) -> Result<f64, Self::Error> {
        self.engine
            .calculate_accuracy_from_attrs(&self.map, &self.full_difficulty, live_score(score)?)
            .map_err(Into::into)
    }

    fn calculate_performance(
        &self,
        difficulty: &Self::Difficulty,
        score: &ScoreInput,
    ) -> Result<PerformanceAttributes, Self::Error> {
        self.engine
            .performance_data_from_attrs_with_context(
                &self.map,
                &self.config,
                difficulty,
                &self.full_difficulty,
                live_score(score)?,
            )
            .map(performance_attributes)
            .map_err(Into::into)
    }
}

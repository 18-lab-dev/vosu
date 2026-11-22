use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientScoring {
    Stable,
    Lazer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Ruleset {
    Osu = 0,
    Taiko = 1,
    Catch = 2,
    Mania = 3,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HitStatistics {
    pub perfect: i64,
    pub great: i64,
    pub good: i64,
    pub ok: i64,
    pub meh: i64,
    pub miss: i64,
    pub small_tick_miss: i64,
    pub small_tick_hit: i64,
    pub large_tick_miss: i64,
    pub large_tick_hit: i64,
    pub small_bonus: i64,
    pub large_bonus: i64,
    pub ignore_miss: i64,
    pub ignore_hit: i64,
    pub combo_break: i64,
    pub slider_tail_hit: i64,
}

/// Calculator score input, matching the native lazer calculator's ScoreInfoData.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScoreInput {
    pub total_score: i64,
    pub is_legacy_score: bool,
    pub accuracy: f64,
    pub max_combo: i64,
    pub perfects: i64,
    pub greats: i64,
    pub goods: i64,
    pub oks: i64,
    pub mehs: i64,
    pub misses: i64,
    pub slider_end_hits: i64,
    pub small_tick_hits: i64,
    pub small_tick_misses: i64,
    pub large_tick_hits: i64,
    pub large_tick_misses: i64,
    pub large_bonuses: i64,
    pub small_bonuses: i64,
    pub combo_breaks: i64,
    pub ignore_hits: i64,
    pub ignore_misses: i64,
}

impl ScoreInput {
    pub(super) fn from_play(
        score: i64,
        combo: i64,
        client: ClientScoring,
        s: HitStatistics,
    ) -> Self {
        Self {
            total_score: score,
            is_legacy_score: client == ClientScoring::Stable,
            accuracy: 0.0,
            max_combo: combo,
            perfects: s.perfect,
            greats: s.great,
            goods: s.good,
            oks: s.ok,
            mehs: s.meh,
            misses: s.miss,
            slider_end_hits: s.slider_tail_hit,
            small_tick_hits: s.small_tick_hit,
            small_tick_misses: s.small_tick_miss,
            large_tick_hits: s.large_tick_hit,
            large_tick_misses: s.large_tick_miss,
            large_bonuses: s.large_bonus,
            small_bonuses: s.small_bonus,
            combo_breaks: s.combo_break,
            ignore_hits: s.ignore_hit,
            ignore_misses: s.ignore_miss,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DifficultyAttributes {
    pub stars: f64,
    pub max_combo: i64,
    pub n_sliders: i64,
    pub n_objects: i64,
    pub nested_score_per_object: f64,
    pub maximum_legacy_combo_score: f64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PerformanceAttributes {
    pub pp: f64,
    pub accuracy: f64,
    pub aim: f64,
    pub difficulty: f64,
    pub flashlight: f64,
    pub speed: f64,
    pub reading: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProgressiveAttributes {
    pub stars: f64,
    pub pp: f64,
    pub max_achieved: f64,
    pub max_achievable: f64,
    pub fc_pp: f64,
    pub current: PerformanceAttributes,
    pub fc: PerformanceAttributes,
}

pub trait GradualProgression {
    type Error: fmt::Display;
    /// The complete engine difficulty value. It remains opaque here so
    /// mode-specific fields survive the progressive performance call.
    type Difficulty;

    /// Advance to the source's inclusive play-time window and return the number
    /// of newly included objects.
    fn skip_to_time(&mut self, play_time: f64) -> Result<usize, Self::Error>;
    fn difficulty(&mut self) -> Result<Self::Difficulty, Self::Error>;
    fn progressive_accuracy(&mut self, score: &ScoreInput) -> Result<f64, Self::Error>;
    fn calculate_performance(
        &mut self,
        score: &ScoreInput,
    ) -> Result<PerformanceAttributes, Self::Error>;
}

pub trait ProgressiveBeatmap {
    type Error: fmt::Display;
    type Difficulty;
    type Gradual: GradualProgression<Error = Self::Error, Difficulty = Self::Difficulty>;

    fn create_gradual(&self) -> Result<Self::Gradual, Self::Error>;
    fn full_difficulty(&self) -> &Self::Difficulty;
    /// Extract only fields used by source-side score projection and publication;
    /// the complete difficulty is still passed to `calculate_performance`.
    fn difficulty_summary(&self, difficulty: &Self::Difficulty) -> DifficultyAttributes;
    fn max_score(&self) -> &ScoreInput;
    fn calculate_accuracy(&self, score: &ScoreInput) -> Result<f64, Self::Error>;
    fn calculate_performance(
        &self,
        difficulty: &Self::Difficulty,
        score: &ScoreInput,
    ) -> Result<PerformanceAttributes, Self::Error>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameplayCacheKey {
    pub beatmap_checksum: String,
    pub mode: u8,
    pub mods_checksum: String,
    pub mp3_length: f64,
}

impl GameplayCacheKey {
    /// Exact source key, including separators and JavaScript decimal spelling for
    /// the integral fields supplied by memory.
    pub fn source_key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.beatmap_checksum, self.mode, self.mods_checksum, self.mp3_length
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameplayGate {
    Active,
    CalculationDisabled,
    MissingGameFolder,
    BeatmapUnavailable,
    BeforeFirstObject,
    Retry,
    SongSelectionReset,
    NotPlaying,
    Tournament,
}

#[derive(Debug, Clone, Copy)]
pub struct GameplayFrame<'a> {
    pub gate: GameplayGate,
    /// Source `BeatmapPP.resetAttributes()` retains the precomputed 100% PP as
    /// the FC baseline on retry/song selection.
    pub reset_fc_pp: f64,
    pub key: &'a GameplayCacheKey,
    pub play_time: f64,
    pub mode: Ruleset,
    pub client: ClientScoring,
    pub score: i64,
    pub combo: i64,
    pub max_combo: i64,
    pub statistics: HitStatistics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    CalculationDisabled,
    MissingGameFolder,
    BeatmapUnavailable,
    SongSelectionReset,
    NotPlaying,
    Tournament,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CalculationOutcome {
    Updated,
    Unchanged,
    Skipped(SkipReason),
    /// The previous public values are intentionally retained, as in the source.
    RetainedError(String),
}

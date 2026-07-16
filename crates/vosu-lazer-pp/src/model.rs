#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ruleset {
    #[default]
    Osu,
    Taiko,
    Catch,
    Mania,
}

impl Ruleset {
    const fn game_mode(self) -> GameMode {
        match self {
            Self::Osu => GameMode::Osu,
            Self::Taiko => GameMode::Taiko,
            Self::Catch => GameMode::Catch,
            Self::Mania => GameMode::Mania,
        }
    }

    const fn from_game_mode(mode: GameMode) -> Self {
        match mode {
            GameMode::Osu => Self::Osu,
            GameMode::Taiko => Self::Taiko,
            GameMode::Catch => Self::Catch,
            GameMode::Mania => Self::Mania,
        }
    }

    /// Legacy numeric game mode exposed by `PlayBeatmap.Mode`.
    pub const fn mode_id(self) -> i32 {
        match self {
            Self::Osu => 0,
            Self::Taiko => 1,
            Self::Catch => 2,
            Self::Mania => 3,
        }
    }

    pub const fn from_mode_id(mode: i32) -> Option<Self> {
        match mode {
            0 => Some(Self::Osu),
            1 => Some(Self::Taiko),
            2 => Some(Self::Catch),
            3 => Some(Self::Mania),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModValue {
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<ModValue>),
    Object(BTreeMap<String, ModValue>),
    Null,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredMod {
    pub acronym: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub settings: BTreeMap<String, ModValue>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DifficultyConfig {
    pub mods: Vec<StructuredMod>,
    pub clock_rate: Option<f64>,
    pub ar: Option<f32>,
    pub cs: Option<f32>,
    pub hp: Option<f32>,
    pub od: Option<f32>,
    pub passed_objects: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct LiveScoreState {
    pub total_score: u64,
    pub is_legacy_score: bool,
    pub accuracy: f64,
    pub max_combo: u32,
    pub combo_breaks: u32,
    pub ignore_hits: u32,
    pub ignore_misses: u32,
    pub large_bonuses: u32,
    pub small_bonuses: u32,
    pub n_geki: u32,
    pub n300: u32,
    pub n_katu: u32,
    pub n100: u32,
    pub n50: u32,
    pub misses: u32,
    pub large_tick_hits: u32,
    pub large_tick_misses: u32,
    pub small_tick_hits: u32,
    pub slider_end_hits: u32,
    pub small_tick_misses: u32,
}

/// Exact public score record exposed by the pinned calculator wrapper.
///
/// Hit-result fields are signed because the source score simulator intentionally
/// produces negative values for infeasible taiko and catch accuracies.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreInfoData {
    pub total_score: i64,
    pub is_legacy_score: bool,
    pub accuracy: f64,
    pub max_combo: i32,
    pub slider_end_hits: i32,
    pub combo_breaks: i32,
    pub ignore_hits: i32,
    pub ignore_misses: i32,
    pub large_bonuses: i32,
    pub small_bonuses: i32,
    pub large_tick_hits: i32,
    pub large_tick_misses: i32,
    pub small_tick_hits: i32,
    pub small_tick_misses: i32,
    pub perfects: i32,
    pub greats: i32,
    pub goods: i32,
    pub oks: i32,
    pub mehs: i32,
    pub misses: i32,
}

impl From<LiveScoreState> for ScoreInfoData {
    fn from(value: LiveScoreState) -> Self {
        Self {
            total_score: value.total_score.min(i64::MAX as u64) as i64,
            is_legacy_score: value.is_legacy_score,
            accuracy: value.accuracy,
            max_combo: value.max_combo.min(i32::MAX as u32) as i32,
            slider_end_hits: value.slider_end_hits.min(i32::MAX as u32) as i32,
            combo_breaks: value.combo_breaks.min(i32::MAX as u32) as i32,
            ignore_hits: value.ignore_hits.min(i32::MAX as u32) as i32,
            ignore_misses: value.ignore_misses.min(i32::MAX as u32) as i32,
            large_bonuses: value.large_bonuses.min(i32::MAX as u32) as i32,
            small_bonuses: value.small_bonuses.min(i32::MAX as u32) as i32,
            large_tick_hits: value.large_tick_hits.min(i32::MAX as u32) as i32,
            large_tick_misses: value.large_tick_misses.min(i32::MAX as u32) as i32,
            small_tick_hits: value.small_tick_hits.min(i32::MAX as u32) as i32,
            small_tick_misses: value.small_tick_misses.min(i32::MAX as u32) as i32,
            perfects: value.n_geki.min(i32::MAX as u32) as i32,
            greats: value.n300.min(i32::MAX as u32) as i32,
            goods: value.n_katu.min(i32::MAX as u32) as i32,
            oks: value.n100.min(i32::MAX as u32) as i32,
            mehs: value.n50.min(i32::MAX as u32) as i32,
            misses: value.misses.min(i32::MAX as u32) as i32,
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to parse beatmap: {0}")]
    Parse(String),
    #[error("cannot convert {from:?} beatmap to {to:?}")]
    Convert { from: Ruleset, to: Ruleset },
    #[error("invalid structured mods: {0}")]
    Mods(String),
    #[error("hit result {result} is unavailable for {mode:?}")]
    UnsupportedHitResult { mode: Ruleset, result: i32 },
    #[error("accuracy must be finite")]
    NonFiniteAccuracy,
    #[error("gradual calculator has not processed an object")]
    NoCurrentDifficulty,
    #[error("osu strains are unavailable before the full gradual prefix")]
    IncompleteOsuStrains,
    #[error("difficulty attributes do not retain their native calculator state")]
    MissingDifficultyAttributes,
    #[error("difficulty attributes for {actual:?} cannot be used with {expected:?}")]
    DifficultyModeMismatch { expected: Ruleset, actual: Ruleset },
    #[error("score field `{0}` cannot be represented by the unsigned compatibility state")]
    ScoreOutOfRange(&'static str),
}

impl TryFrom<ScoreInfoData> for LiveScoreState {
    type Error = Error;

    fn try_from(value: ScoreInfoData) -> Result<Self, Self::Error> {
        fn unsigned(name: &'static str, value: i32) -> Result<u32, Error> {
            value.try_into().map_err(|_| Error::ScoreOutOfRange(name))
        }

        Ok(Self {
            // Negative non-legacy totals are valid wrapper input and are
            // ignored by every pinned performance calculator. Preserve every
            // representable value and use zero only for that unrepresentable,
            // source-defined no-op case.
            total_score: match value.total_score.try_into() {
                Ok(total_score) => total_score,
                Err(_) if !value.is_legacy_score => 0,
                Err(_) => return Err(Error::ScoreOutOfRange("totalScore")),
            },
            is_legacy_score: value.is_legacy_score,
            accuracy: value.accuracy,
            max_combo: unsigned("maxCombo", value.max_combo)?,
            combo_breaks: unsigned("comboBreaks", value.combo_breaks)?,
            ignore_hits: unsigned("ignoreHits", value.ignore_hits)?,
            ignore_misses: unsigned("ignoreMisses", value.ignore_misses)?,
            large_bonuses: unsigned("largeBonuses", value.large_bonuses)?,
            small_bonuses: unsigned("smallBonuses", value.small_bonuses)?,
            n_geki: unsigned("perfects", value.perfects)?,
            n300: unsigned("greats", value.greats)?,
            n_katu: unsigned("goods", value.goods)?,
            n100: unsigned("oks", value.oks)?,
            n50: unsigned("mehs", value.mehs)?,
            misses: unsigned("misses", value.misses)?,
            large_tick_hits: unsigned("largeTickHits", value.large_tick_hits)?,
            large_tick_misses: unsigned("largeTickMisses", value.large_tick_misses)?,
            small_tick_hits: unsigned("smallTickHits", value.small_tick_hits)?,
            slider_end_hits: unsigned("sliderEndHits", value.slider_end_hits)?,
            small_tick_misses: unsigned("smallTickMisses", value.small_tick_misses)?,
        })
    }
}


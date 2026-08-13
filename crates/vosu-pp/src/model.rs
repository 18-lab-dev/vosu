use std::collections::BTreeMap;

use rosu_map::section::general::GameMode;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

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
    pub const fn from_mode_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Osu),
            1 => Some(Self::Taiko),
            2 => Some(Self::Catch),
            3 => Some(Self::Mania),
            _ => None,
        }
    }

    pub const fn mode_id(self) -> u8 {
        self as u8
    }

    pub(crate) const fn game_mode(self) -> GameMode {
        match self {
            Self::Osu => GameMode::Osu,
            Self::Taiko => GameMode::Taiko,
            Self::Catch => GameMode::Catch,
            Self::Mania => GameMode::Mania,
        }
    }

    pub(crate) const fn mods_mode(self) -> rosu_mods::GameMode {
        match self {
            Self::Osu => rosu_mods::GameMode::Osu,
            Self::Taiko => rosu_mods::GameMode::Taiko,
            Self::Catch => rosu_mods::GameMode::Catch,
            Self::Mania => rosu_mods::GameMode::Mania,
        }
    }
}

pub(crate) const fn ruleset(mode: GameMode) -> Ruleset {
    match mode {
        GameMode::Osu => Ruleset::Osu,
        GameMode::Taiko => Ruleset::Taiko,
        GameMode::Catch => Ruleset::Catch,
        GameMode::Mania => Ruleset::Mania,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModValue {
    Bool(bool),
    Number(f64),
    String(String),
    Json(serde_json::Value),
}

impl ModValue {
    pub(crate) fn number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::String(value) => value.parse().ok(),
            _ => None,
        }
    }

    pub(crate) fn boolean(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            Self::String(value) if value.eq_ignore_ascii_case("true") => Some(true),
            Self::String(value) if value.eq_ignore_ascii_case("false") => Some(false),
            _ => None,
        }
    }
}

impl Default for ModValue {
    fn default() -> Self {
        Self::Bool(false)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredMod {
    pub acronym: String,
    #[serde(default)]
    pub settings: BTreeMap<String, ModValue>,
}

impl StructuredMod {
    pub(crate) fn setting(&self, snake: &str) -> Option<&ModValue> {
        self.settings.get(snake).or_else(|| {
            self.settings
                .iter()
                .find(|(key, _)| camel_to_snake(key) == snake)
                .map(|(_, value)| value)
        })
    }

    pub(crate) fn number(&self, name: &str) -> Option<f64> {
        self.setting(name).and_then(ModValue::number)
    }
    pub(crate) fn boolean(&self, name: &str) -> Option<bool> {
        self.setting(name).and_then(ModValue::boolean)
    }
}

pub(crate) fn camel_to_snake(input: &str) -> String {
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

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DifficultyConfig {
    pub mods: Vec<StructuredMod>,
    pub clock_rate: Option<f64>,
    pub passed_objects: Option<u32>,
    pub ar: Option<f32>,
    pub cs: Option<f32>,
    pub hp: Option<f32>,
    pub od: Option<f32>,
    pub hardrock_offsets: Option<bool>,
    pub lazer: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LiveScoreState {
    pub accuracy: f64,
    pub max_combo: u32,
    pub n_geki: u32,
    pub n_katu: u32,
    pub n300: u32,
    pub n100: u32,
    pub n50: u32,
    pub misses: u32,
    pub slider_end_hits: u32,
    pub small_tick_hits: u32,
    pub large_tick_hits: u32,
    pub large_tick_misses: u32,
    pub small_tick_misses: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
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
            accuracy: value.accuracy,
            max_combo: value.max_combo as i32,
            perfects: value.n_geki as i32,
            greats: value.n300 as i32,
            goods: value.n_katu as i32,
            oks: value.n100 as i32,
            mehs: value.n50 as i32,
            misses: value.misses as i32,
            slider_end_hits: value.slider_end_hits as i32,
            small_tick_hits: value.small_tick_hits as i32,
            large_tick_hits: value.large_tick_hits as i32,
            large_tick_misses: value.large_tick_misses as i32,
            small_tick_misses: value.small_tick_misses as i32,
            ..Self::default()
        }
    }
}

impl TryFrom<ScoreInfoData> for LiveScoreState {
    type Error = Error;
    fn try_from(value: ScoreInfoData) -> Result<Self, Self::Error> {
        Ok(Self {
            accuracy: value.accuracy,
            max_combo: checked("maxCombo", value.max_combo)?,
            n_geki: checked("perfects", value.perfects)?,
            n_katu: checked("goods", value.goods)?,
            n300: checked("greats", value.greats)?,
            n100: checked("oks", value.oks)?,
            n50: checked("mehs", value.mehs)?,
            misses: checked("misses", value.misses)?,
            slider_end_hits: checked("sliderEndHits", value.slider_end_hits)?,
            small_tick_hits: checked("smallTickHits", value.small_tick_hits)?,
            large_tick_hits: checked("largeTickHits", value.large_tick_hits)?,
            large_tick_misses: checked("largeTickMisses", value.large_tick_misses)?,
            small_tick_misses: checked("smallTickMisses", value.small_tick_misses)?,
        })
    }
}

pub(crate) fn checked(field: &'static str, value: i32) -> Result<u32, Error> {
    value
        .try_into()
        .map_err(|_| Error::InvalidScoreField { field, value })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum HitResult {
    None = 0,
    Miss = 1,
    Meh = 2,
    Ok = 3,
    Good = 4,
    Great = 5,
    Perfect = 6,
    SmallTickMiss = 7,
    SmallTickHit = 8,
    LargeTickMiss = 9,
    LargeTickHit = 10,
    SmallBonus = 11,
    LargeBonus = 12,
    IgnoreMiss = 13,
    IgnoreHit = 14,
    ComboBreak = 15,
    SliderTailHit = 16,
    LegacyComboIncrease = 99,
}

impl HitResult {
    pub const ALL: [Self; 18] = [
        Self::None,
        Self::Miss,
        Self::Meh,
        Self::Ok,
        Self::Good,
        Self::Great,
        Self::Perfect,
        Self::SmallTickMiss,
        Self::SmallTickHit,
        Self::LargeTickMiss,
        Self::LargeTickHit,
        Self::SmallBonus,
        Self::LargeBonus,
        Self::IgnoreMiss,
        Self::IgnoreHit,
        Self::ComboBreak,
        Self::SliderTailHit,
        Self::LegacyComboIncrease,
    ];
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Miss),
            2 => Some(Self::Meh),
            3 => Some(Self::Ok),
            4 => Some(Self::Good),
            5 => Some(Self::Great),
            6 => Some(Self::Perfect),
            7 => Some(Self::SmallTickMiss),
            8 => Some(Self::SmallTickHit),
            9 => Some(Self::LargeTickMiss),
            10 => Some(Self::LargeTickHit),
            11 => Some(Self::SmallBonus),
            12 => Some(Self::LargeBonus),
            13 => Some(Self::IgnoreMiss),
            14 => Some(Self::IgnoreHit),
            15 => Some(Self::ComboBreak),
            16 => Some(Self::SliderTailHit),
            99 => Some(Self::LegacyComboIncrease),
            _ => None,
        }
    }
}
impl Serialize for HitResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}
impl<'de> Deserialize<'de> for HitResult {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i32::deserialize(deserializer)?;
        Self::from_i32(value).ok_or_else(|| serde::de::Error::custom("invalid hit result"))
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to parse beatmap: {0}")]
    Parse(String),
    #[error("cannot convert {from:?} beatmap to {to:?}")]
    Convert { from: Ruleset, to: Ruleset },
    #[error("invalid mods: {0}")]
    Mods(String),
    #[error("invalid hit result {result:?} for {mode:?}")]
    HitWindow { mode: Ruleset, result: HitResult },
    #[error("invalid score field `{field}`: {value}")]
    InvalidScoreField { field: &'static str, value: i32 },
    #[error("invalid target accuracy: {0}")]
    InvalidAccuracy(f64),
    #[error("difficulty attributes for {actual:?} cannot be used with {expected:?}")]
    DifficultyModeMismatch { expected: Ruleset, actual: Ruleset },
    #[error("difficulty state is not available at this point")]
    DifficultyUnavailable,
    #[error("invalid calculation input: {0}")]
    Calculation(String),
}

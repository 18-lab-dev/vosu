use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mod {
    pub acronym: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settings: Option<IndexMap<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalculateMods {
    pub checksum: String,
    pub number: i64,
    pub name: String,
    pub array: Vec<Mod>,
    pub rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Statistics {
    pub perfect: i64,
    pub great: i64,
    pub good: i64,
    pub ok: i64,
    pub meh: i64,
    pub miss: i64,
    #[serde(rename = "smallTickMiss")]
    pub small_tick_miss: i64,
    #[serde(rename = "smallTickHit")]
    pub small_tick_hit: i64,
    #[serde(rename = "largeTickMiss")]
    pub large_tick_miss: i64,
    #[serde(rename = "largeTickHit")]
    pub large_tick_hit: i64,
    #[serde(rename = "smallBonus")]
    pub small_bonus: i64,
    #[serde(rename = "largeBonus")]
    pub large_bonus: i64,
    #[serde(rename = "ignoreMiss")]
    pub ignore_miss: i64,
    #[serde(rename = "ignoreHit")]
    pub ignore_hit: i64,
    #[serde(rename = "comboBreak")]
    pub combo_break: i64,
    #[serde(rename = "sliderTailHit")]
    pub slider_tail_hit: i64,
    #[serde(
        rename = "legacyComboIncrease",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_combo_increase: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyOverlayButton {
    pub name: String,
    #[serde(rename = "isPressed")]
    pub is_pressed: bool,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaderboardPlayer {
    #[serde(rename = "userId")]
    pub user_id: i64,
    pub name: String,
    pub score: f64,
    pub combo: i64,
    pub accuracy: f64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    pub mods: CalculateMods,
    pub statistics: Statistics,
    pub team: i64,
    pub position: i64,
    #[serde(rename = "isPassing")]
    pub is_passing: bool,
}

pub type SettingsObject = IndexMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TourneyManagerChatItem {
    pub time: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MultiplayerTeamType {
    Red,
    Blue,
    None,
}

use std::sync::{Arc, LazyLock};

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
    pub name: Arc<str>,
    #[serde(rename = "isPressed")]
    pub is_pressed: bool,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeypressButton {
    pub name: Arc<str>,
    pub is_pressed: bool,
    pub count: i64,
    pub pressed_at: Option<f64>,
    pub released_at: Option<f64>,
}

static KEY_NAMES: LazyLock<[Arc<str>; 18]> = LazyLock::new(|| {
    [
        Arc::from("K1"),
        Arc::from("K2"),
        Arc::from("K3"),
        Arc::from("K4"),
        Arc::from("K5"),
        Arc::from("K6"),
        Arc::from("K7"),
        Arc::from("K8"),
        Arc::from("K9"),
        Arc::from("K10"),
        Arc::from("K11"),
        Arc::from("K12"),
        Arc::from("K13"),
        Arc::from("K14"),
        Arc::from("K15"),
        Arc::from("K16"),
        Arc::from("K17"),
        Arc::from("K18"),
    ]
});

static BUTTON_NAMES: LazyLock<[Arc<str>; 18]> = LazyLock::new(|| {
    [
        Arc::from("B1"),
        Arc::from("B2"),
        Arc::from("B3"),
        Arc::from("B4"),
        Arc::from("B5"),
        Arc::from("B6"),
        Arc::from("B7"),
        Arc::from("B8"),
        Arc::from("B9"),
        Arc::from("B10"),
        Arc::from("B11"),
        Arc::from("B12"),
        Arc::from("B13"),
        Arc::from("B14"),
        Arc::from("B15"),
        Arc::from("B16"),
        Arc::from("B17"),
        Arc::from("B18"),
    ]
});

static LEFT_NAME: LazyLock<Arc<str>> = LazyLock::new(|| Arc::from("L"));
static RIGHT_NAME: LazyLock<Arc<str>> = LazyLock::new(|| Arc::from("R"));
static DRUM_NAME: LazyLock<Arc<str>> = LazyLock::new(|| Arc::from("D"));
static MOUSE_ONE_NAME: LazyLock<Arc<str>> = LazyLock::new(|| Arc::from("M1"));
static MOUSE_TWO_NAME: LazyLock<Arc<str>> = LazyLock::new(|| Arc::from("M2"));

impl KeyOverlayButton {
    pub(crate) fn stable_name(mode: i64, index: usize) -> Arc<str> {
        match (mode, index) {
            (3, _) | (_, 4..) => Arc::clone(&KEY_NAMES[index]),
            (2, 0) => Arc::clone(&LEFT_NAME),
            (2, 1) => Arc::clone(&RIGHT_NAME),
            (2, 2) => Arc::clone(&DRUM_NAME),
            (2, 3) | (_, 3) => Arc::clone(&MOUSE_TWO_NAME),
            (_, 0) => Arc::clone(&KEY_NAMES[0]),
            (_, 1) => Arc::clone(&KEY_NAMES[1]),
            (_, 2) => Arc::clone(&MOUSE_ONE_NAME),
        }
    }

    pub(crate) fn lazer_button_count() -> usize {
        BUTTON_NAMES.len()
    }

    pub(crate) fn lazer_name(index: usize) -> Arc<str> {
        Arc::clone(&BUTTON_NAMES[index])
    }
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

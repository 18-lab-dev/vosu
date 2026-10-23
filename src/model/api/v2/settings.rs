use super::NumberName;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaderboardSettings {
    pub visible: bool,
    #[serde(rename = "type")]
    pub kind: NumberName,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub fullscreen: bool,
    pub width: i64,
    pub height: i64,
    #[serde(rename = "widthFullscreen")]
    pub width_fullscreen: i64,
    #[serde(rename = "heightFullscreen")]
    pub height_fullscreen: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Client {
    #[serde(rename = "updateAvailable")]
    pub update_available: bool,
    pub branch: i64,
    pub version: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreMeter {
    #[serde(rename = "type")]
    pub kind: NumberName,
    pub size: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    #[serde(rename = "useSkinCursor")]
    pub use_skin_cursor: bool,
    #[serde(rename = "autoSize")]
    pub auto_size: bool,
    pub size: f64,
    #[serde(rename = "menuSize")]
    pub menu_size: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mouse {
    #[serde(rename = "rawInput")]
    pub raw_input: bool,
    #[serde(rename = "disableButtons")]
    pub disable_buttons: bool,
    #[serde(rename = "disableWheel")]
    pub disable_wheel: bool,
    pub sensitivity: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tablet {
    pub enabled: bool,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation: f64,
    #[serde(rename = "pressureThreshold")]
    pub pressure_threshold: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScrollDirection {
    pub number: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mania {
    #[serde(rename = "speedBPMScale")]
    pub speed_bpm_scale: bool,
    #[serde(rename = "usePerBeatmapSpeedScale")]
    pub use_per_beatmap_speed_scale: bool,
    #[serde(rename = "scrollSpeed")]
    pub scroll_speed: i64,
    #[serde(rename = "scrollDirection")]
    pub scroll_direction: ScrollDirection,
    #[serde(
        default,
        rename = "scrollSpeedValue",
        skip_serializing_if = "Option::is_none"
    )]
    pub scroll_speed_value: Option<f64>,
    #[serde(
        default,
        rename = "scrollTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub scroll_time: Option<f64>,
    #[serde(
        default,
        rename = "timingBasedNoteColouring",
        skip_serializing_if = "Option::is_none"
    )]
    pub timing_based_note_colouring: Option<bool>,
    #[serde(
        default,
        rename = "perBeatmapScrollSpeed",
        skip_serializing_if = "Option::is_none"
    )]
    pub per_beatmap_scroll_speed: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skin {
    #[serde(rename = "useDefaultSkinInEditor")]
    pub use_default_skin_in_editor: bool,
    #[serde(rename = "ignoreBeatmapSkins")]
    pub ignore_beatmap_skins: bool,
    #[serde(rename = "tintSliderBall")]
    pub tint_slider_ball: bool,
    #[serde(rename = "useTaikoSkin")]
    pub use_taiko_skin: bool,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Volume {
    #[serde(rename = "masterInactive")]
    pub master_inactive: f64,
    pub master: f64,
    pub music: f64,
    pub effect: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Offset {
    pub universal: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Audio {
    #[serde(rename = "ignoreBeatmapSounds")]
    pub ignore_beatmap_sounds: bool,
    #[serde(rename = "useSkinSamples")]
    pub use_skin_samples: bool,
    pub volume: Volume,
    pub offset: Offset,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Background {
    pub storyboard: bool,
    pub video: bool,
    pub blur: f64,
    pub dim: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeybindsOsu {
    pub k1: String,
    pub k2: String,
    #[serde(rename = "smokeKey")]
    pub smoke_key: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeybindsFruits {
    pub k1: String,
    pub k2: String,
    #[serde(rename = "Dash")]
    pub dash: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeybindsTaiko {
    #[serde(rename = "innerLeft")]
    pub inner_left: String,
    #[serde(rename = "innerRight")]
    pub inner_right: String,
    #[serde(rename = "outerLeft")]
    pub outer_left: String,
    #[serde(rename = "outerRight")]
    pub outer_right: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keybinds {
    pub osu: KeybindsOsu,
    pub fruits: KeybindsFruits,
    pub taiko: KeybindsTaiko,
    #[serde(rename = "quickRetry")]
    pub quick_retry: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Visual {
    pub snaking_in_sliders: Option<bool>,
    pub snaking_out_sliders: Option<bool>,
    pub hit_lighting: Option<bool>,
    pub beatmap_colours: Option<bool>,
    pub combo_colour_normalisation: Option<f64>,
    pub hit_animations: Option<bool>,
    pub rate_adjusted_hit_animation: Option<bool>,
    pub scaling_mode: Option<i64>,
    pub position_x: Option<f64>,
    pub position_y: Option<f64>,
    pub size_x: Option<f64>,
    pub size_y: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(rename = "interfaceVisible")]
    pub interface_visible: bool,
    #[serde(rename = "replayUIVisible")]
    pub replay_ui_visible: bool,
    #[serde(rename = "chatVisibilityStatus")]
    pub chat_visibility_status: NumberName,
    pub leaderboard: LeaderboardSettings,
    #[serde(rename = "progressBar")]
    pub progress_bar: NumberName,
    #[serde(rename = "bassDensity")]
    pub bass_density: f64,
    pub resolution: Resolution,
    pub client: Client,
    #[serde(rename = "scoreMeter")]
    pub score_meter: ScoreMeter,
    pub cursor: Cursor,
    pub mouse: Mouse,
    pub tablet: Tablet,
    pub mania: Mania,
    pub sort: NumberName,
    pub group: NumberName,
    pub skin: Skin,
    pub mode: NumberName,
    pub audio: Audio,
    pub background: Background,
    pub keybinds: Keybinds,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual: Option<Visual>,
}

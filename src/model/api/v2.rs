use crate::model::{memory::RankedPlay, states::CalculateMods};
use serde::{Deserialize, Serialize};
mod settings;

pub use settings::*;

fn serialize_js_number<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_f64(*value)
}

fn serialize_optional_js_number<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(value) => serialize_js_number(value, serializer),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumberName {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Game {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<bool>,
    pub paused: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(rename = "playTime", serialize_with = "serialize_js_number")]
    pub play_time: f64,
    #[serde(rename = "playCount")]
    pub play_count: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapTime {
    #[serde(serialize_with = "serialize_js_number")]
    pub live: f64,
    #[serde(rename = "firstObject")]
    pub first_object: i64,
    #[serde(rename = "lastObject", serialize_with = "serialize_js_number")]
    pub last_object: f64,
    #[serde(
        rename = "mp3Length",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_number"
    )]
    pub mp3_length: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchmakingStats {
    pub rating: f64,
    pub rank: Option<i64>,
    pub plays: i64,
    pub wins: i64,
    #[serde(rename = "isProvisional")]
    pub is_provisional: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(rename = "userStatus", skip_serializing_if = "Option::is_none")]
    pub user_status: Option<NumberName>,
    #[serde(rename = "banchoStatus", skip_serializing_if = "Option::is_none")]
    pub bancho_status: Option<NumberName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<NumberName>,
    #[serde(
        rename = "rankedScore",
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_number"
    )]
    pub ranked_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp: Option<f64>,
    #[serde(rename = "playCount", skip_serializing_if = "Option::is_none")]
    pub play_count: Option<i64>,
    #[serde(rename = "globalRank", skip_serializing_if = "Option::is_none")]
    pub global_rank: Option<i64>,
    #[serde(rename = "countryCode", skip_serializing_if = "Option::is_none")]
    pub country_code: Option<NumberName>,
    #[serde(rename = "backgroundColour", skip_serializing_if = "Option::is_none")]
    pub background_colour: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matchmaking: Option<Option<MatchmakingStats>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stars {
    pub live: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aim: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flashlight: Option<f64>,
    #[serde(rename = "sliderFactor", skip_serializing_if = "Option::is_none")]
    pub slider_factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stamina: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rhythm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading: Option<f64>,
    #[serde(rename = "hitWindow", skip_serializing_if = "Option::is_none")]
    pub hit_window: Option<f64>,
    pub total: f64,
}
macro_rules! difficulty_pair {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        pub struct $name {
            pub original: f64,
            pub converted: f64,
        }
    };
}
difficulty_pair!(Ar);
difficulty_pair!(Cs);
difficulty_pair!(Od);
difficulty_pair!(Hp);
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bpm {
    pub realtime: f64,
    pub common: f64,
    pub min: f64,
    pub max: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Objects {
    pub circles: i64,
    pub sliders: i64,
    pub spinners: i64,
    pub holds: i64,
    pub total: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub stars: Stars,
    pub ar: Ar,
    pub cs: Cs,
    pub od: Od,
    pub hp: Hp,
    pub bpm: Bpm,
    pub objects: Objects,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapStatus {
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_number"
    )]
    pub number: Option<f64>,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Beatmap {
    #[serde(rename = "isKiai")]
    pub is_kiai: bool,
    #[serde(rename = "isBreak")]
    pub is_break: bool,
    #[serde(rename = "isConvert")]
    pub is_convert: bool,
    pub time: BeatmapTime,
    pub status: BeatmapStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<i64>,
    pub mode: NumberName,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "artistUnicode", skip_serializing_if = "Option::is_none")]
    pub artist_unicode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "titleUnicode", skip_serializing_if = "Option::is_none")]
    pub title_unicode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapper: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub source: String,
    pub tags: String,
    pub stats: Stats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthBar {
    pub normal: f64,
    pub smooth: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hits {
    #[serde(rename = "0")]
    pub h0: i64,
    #[serde(rename = "50")]
    pub h50: i64,
    #[serde(rename = "100")]
    pub h100: i64,
    #[serde(rename = "300")]
    pub h300: i64,
    pub geki: i64,
    pub katu: i64,
    #[serde(rename = "sliderBreaks")]
    pub slider_breaks: i64,
    #[serde(rename = "sliderEndHits")]
    pub slider_end_hits: i64,
    #[serde(rename = "smallTickHits")]
    pub small_tick_hits: i64,
    #[serde(rename = "largeTickHits")]
    pub large_tick_hits: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LazerHits {
    #[serde(rename = "0")]
    pub h0: i64,
    #[serde(rename = "50")]
    pub h50: i64,
    #[serde(rename = "100")]
    pub h100: i64,
    #[serde(rename = "300")]
    pub h300: i64,
    pub geki: i64,
    pub katu: i64,
    #[serde(rename = "sliderEndHits")]
    pub slider_end_hits: i64,
    #[serde(rename = "smallTickHits")]
    pub small_tick_hits: i64,
    #[serde(rename = "largeTickHits")]
    pub large_tick_hits: i64,
    #[serde(rename = "sliderBreaks")]
    pub slider_breaks: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlayHits {
    Normal(Hits),
    Lazer(LazerHits),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Combo {
    pub current: i64,
    pub max: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rank {
    pub current: String,
    #[serde(rename = "maxThisPlay")]
    pub max_this_play: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetailedPpAttributes {
    pub aim: f64,
    pub speed: f64,
    pub accuracy: f64,
    pub difficulty: f64,
    pub flashlight: f64,
    pub total: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetailedPp {
    pub current: DetailedPpAttributes,
    pub fc: DetailedPpAttributes,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pp {
    pub current: f64,
    pub fc: f64,
    #[serde(rename = "maxAchieved")]
    pub max_achieved: f64,
    #[serde(rename = "maxAchievable")]
    pub max_achievable: f64,
    pub detailed: DetailedPp,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Play {
    pub failed: bool,
    #[serde(rename = "playerName")]
    pub player_name: String,
    pub mode: NumberName,
    #[serde(serialize_with = "serialize_js_number")]
    pub score: f64,
    pub accuracy: f64,
    #[serde(rename = "healthBar")]
    pub health_bar: HealthBar,
    pub hits: PlayHits,
    #[serde(rename = "hitErrorArray")]
    pub hit_error_array: Vec<f64>,
    pub combo: Combo,
    pub mods: CalculateMods,
    pub rank: Rank,
    pub pp: Pp,
    #[serde(rename = "unstableRate")]
    pub unstable_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaderboardHits {
    #[serde(rename = "0")]
    pub h0: i64,
    #[serde(rename = "50")]
    pub h50: i64,
    #[serde(rename = "100")]
    pub h100: i64,
    #[serde(rename = "300")]
    pub h300: i64,
    pub geki: i64,
    pub katu: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leaderboard {
    #[serde(rename = "isFailed")]
    pub is_failed: bool,
    pub position: i64,
    pub team: i64,
    pub id: i64,
    pub name: String,
    #[serde(serialize_with = "serialize_js_number")]
    pub score: f64,
    pub accuracy: f64,
    pub hits: LeaderboardHits,
    pub combo: Combo,
    pub mods: CalculateMods,
    pub rank: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyOverlayButton {
    #[serde(rename = "isPressed")]
    pub is_pressed: bool,
    pub count: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyOverlay {
    pub k1: KeyOverlayButton,
    pub k2: KeyOverlayButton,
    pub m1: KeyOverlayButton,
    pub m2: KeyOverlayButton,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keypress {
    pub name: std::sync::Arc<str>,
    #[serde(rename = "isPressed")]
    pub is_pressed: bool,
    pub count: i64,
    #[serde(rename = "pressedAt")]
    pub pressed_at: Option<f64>,
    #[serde(rename = "releasedAt")]
    pub released_at: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keypresses {
    pub mode: NumberName,
    pub keys: Vec<Keypress>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreciseTourney {
    #[serde(rename = "ipcId", serialize_with = "serialize_js_number")]
    pub ipc_id: f64,
    pub keys: KeyOverlay,
    pub keypresses: Keypresses,
    #[serde(rename = "hitErrors")]
    pub hit_errors: Vec<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreciseResponse {
    pub keys: KeyOverlay,
    pub keypresses: Keypresses,
    #[serde(rename = "hitErrors")]
    pub hit_errors: Vec<f64>,
    pub tourney: Vec<PreciseTourney>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Accuracy {
    #[serde(rename = "90")]
    pub acc_90: f64,
    #[serde(rename = "91")]
    pub acc_91: f64,
    #[serde(rename = "92")]
    pub acc_92: f64,
    #[serde(rename = "93")]
    pub acc_93: f64,
    #[serde(rename = "94")]
    pub acc_94: f64,
    #[serde(rename = "95")]
    pub acc_95: f64,
    #[serde(rename = "96")]
    pub acc_96: f64,
    #[serde(rename = "97")]
    pub acc_97: f64,
    #[serde(rename = "98")]
    pub acc_98: f64,
    #[serde(rename = "99")]
    pub acc_99: f64,
    #[serde(rename = "100")]
    pub acc_100: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    pub name: String,
    pub data: Vec<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    pub series: Vec<Series>,
    pub xaxis: Vec<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Performance {
    pub accuracy: Accuracy,
    pub graph: Graph,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultsHits {
    #[serde(rename = "0")]
    pub h0: i64,
    #[serde(rename = "50")]
    pub h50: i64,
    #[serde(rename = "100")]
    pub h100: i64,
    #[serde(rename = "300")]
    pub h300: i64,
    pub geki: i64,
    pub katu: i64,
    #[serde(rename = "sliderEndHits")]
    pub slider_end_hits: i64,
    #[serde(rename = "smallTickHits")]
    pub small_tick_hits: i64,
    #[serde(rename = "largeTickHits")]
    pub large_tick_hits: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultsPp {
    pub current: f64,
    pub fc: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultsScreen {
    #[serde(rename = "scoreId", serialize_with = "serialize_js_number")]
    pub score_id: f64,
    #[serde(rename = "playerName")]
    pub player_name: String,
    pub mode: NumberName,
    #[serde(serialize_with = "serialize_js_number")]
    pub score: f64,
    pub accuracy: f64,
    pub name: String,
    pub hits: ResultsHits,
    pub mods: CalculateMods,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    pub rank: String,
    pub pp: ResultsPp,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folders {
    pub game: String,
    pub skin: String,
    pub songs: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beatmap: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Files {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub beatmap: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectPath {
    #[serde(rename = "beatmapFile")]
    pub beatmap_file: String,
    #[serde(rename = "beatmapBackground")]
    pub beatmap_background: String,
    #[serde(rename = "beatmapAudio")]
    pub beatmap_audio: String,
    #[serde(rename = "beatmapFolder", skip_serializing_if = "Option::is_none")]
    pub beatmap_folder: Option<String>,
    #[serde(rename = "skinFolder")]
    pub skin_folder: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sides<T> {
    pub left: T,
    pub right: T,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StableTourneyChatMessage {
    pub team: String,
    pub name: String,
    pub message: String,
    pub timestamp: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LazerTourneyChatMessage {
    pub team: String,
    pub timestamp: String,
    pub name: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TourneyChatMessage {
    Stable(StableTourneyChatMessage),
    Lazer(LazerTourneyChatMessage),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TourneyTeam {
    Left,
    Right,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyClientSettings {
    pub mania: TourneyManiaSettings,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyManiaSettings {
    #[serde(rename = "scrollSpeed")]
    pub scroll_speed: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyUser {
    pub id: i64,
    pub name: String,
    pub country: String,
    pub accuracy: f64,
    #[serde(rename = "rankedScore", serialize_with = "serialize_js_number")]
    pub ranked_score: f64,
    #[serde(rename = "playCount")]
    pub play_count: i64,
    #[serde(rename = "globalRank")]
    pub global_rank: i64,
    #[serde(rename = "totalPP")]
    pub total_pp: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyBeatmap {
    pub stats: Stats,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyClient {
    #[serde(rename = "ipcId", serialize_with = "serialize_js_number")]
    pub ipc_id: f64,
    pub team: TourneyTeam,
    pub settings: TourneyClientSettings,
    pub user: TourneyUser,
    pub beatmap: TourneyBeatmap,
    pub play: Play,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tourney {
    #[serde(rename = "scoreVisible")]
    pub score_visible: bool,
    #[serde(rename = "starsVisible")]
    pub stars_visible: bool,
    #[serde(rename = "ipcState")]
    pub ipc_state: i64,
    #[serde(rename = "bestOF")]
    pub best_of: i64,
    pub team: Sides<String>,
    pub points: Sides<i64>,
    pub chat: Vec<TourneyChatMessage>,
    #[serde(rename = "totalScore")]
    pub total_score: Sides<f64>,
    pub clients: Vec<TourneyClient>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub game: Game,
    pub client: String,
    pub server: String,
    pub state: NumberName,
    pub session: Session,
    #[serde(rename = "rankedPlay", skip_serializing_if = "Option::is_none")]
    pub ranked_play: Option<RankedPlay>,
    pub settings: Settings,
    pub profile: Profile,
    pub beatmap: Beatmap,
    pub play: Play,
    pub leaderboard: Vec<Leaderboard>,
    pub performance: Performance,
    #[serde(rename = "resultsScreen")]
    pub results_screen: ResultsScreen,
    pub folders: Folders,
    pub files: Files,
    #[serde(rename = "directPath")]
    pub direct_path: DirectPath,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tourney: Option<Tourney>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
// Keep the established untagged wire representation inline; boxing would alter the public API.
#[allow(clippy::large_enum_variant)]
pub enum Answer {
    Ready(Response),
    Error(ErrorResponse),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PreciseAnswer {
    Ready(PreciseResponse),
    Error(ErrorResponse),
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapStrainSeries {
    pub name: String,
    pub data: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapStrains {
    pub series: Vec<BeatmapStrainSeries>,
    pub xaxis: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folders {
    pub game: String,
    pub skin: String,
    pub songs: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(rename = "showInterface")]
    pub show_interface: bool,
    pub folders: Folders,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MainMenu {
    #[serde(rename = "bassDensity")]
    pub bass_density: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapTimings {
    #[serde(rename = "firstObj")]
    pub first_obj: i64,
    pub current: f64,
    pub full: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mp3: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(rename = "artistOriginal", skip_serializing_if = "Option::is_none")]
    pub artist_original: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "titleOriginal", skip_serializing_if = "Option::is_none")]
    pub title_original: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapper: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bpm {
    pub realtime: f64,
    pub common: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapStats {
    #[serde(rename = "AR")]
    pub ar: f64,
    #[serde(rename = "CS")]
    pub cs: f64,
    #[serde(rename = "OD")]
    pub od: f64,
    #[serde(rename = "HP")]
    pub hp: f64,
    #[serde(rename = "SR")]
    pub sr: f64,
    #[serde(rename = "BPM")]
    pub bpm: Bpm,
    pub circles: i64,
    pub sliders: i64,
    pub spinners: i64,
    pub holds: i64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    #[serde(rename = "fullSR")]
    pub full_sr: f64,
    #[serde(rename = "memoryAR")]
    pub memory_ar: f64,
    #[serde(rename = "memoryCS")]
    pub memory_cs: f64,
    #[serde(rename = "memoryOD")]
    pub memory_od: f64,
    #[serde(rename = "memoryHP")]
    pub memory_hp: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatmapPaths {
    pub full: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Beatmap {
    pub time: BeatmapTimings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(rename = "rankedStatus", skip_serializing_if = "Option::is_none")]
    pub ranked_status: Option<f64>,
    pub metadata: BeatmapMetadata,
    pub stats: BeatmapStats,
    pub path: BeatmapPaths,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectedMods {
    pub num: i64,
    #[serde(rename = "str")]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerformancePoints {
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
    pub strains: Vec<f64>,
    #[serde(rename = "strainsAll")]
    pub strains_all: BeatmapStrains,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    #[serde(rename = "mainMenu")]
    pub main_menu: MainMenu,
    pub state: i64,
    #[serde(rename = "gameMode", skip_serializing_if = "Option::is_none")]
    pub game_mode: Option<i64>,
    #[serde(rename = "isChatEnabled")]
    pub is_chat_enabled: i64,
    pub bm: Beatmap,
    pub mods: SelectedMods,
    pub pp: PerformancePoints,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Combo {
    pub current: i64,
    pub max: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub normal: f64,
    pub smooth: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameplayGrade {
    pub current: String,
    #[serde(rename = "maxThisPlay")]
    pub max_this_play: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameplayHits {
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
    pub grade: GameplayGrade,
    #[serde(rename = "unstableRate")]
    pub unstable_rate: f64,
    #[serde(rename = "hitErrorArray")]
    pub hit_error_array: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameplayPerformancePoints {
    pub current: f64,
    pub fc: f64,
    #[serde(rename = "maxThisPlay")]
    pub max_this_play: f64,
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
pub struct LeaderboardPlayer {
    pub name: String,
    pub score: f64,
    pub combo: i64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    pub mods: String,
    pub h300: i64,
    pub geki: i64,
    pub h100: i64,
    pub katu: i64,
    pub h50: i64,
    pub h0: i64,
    pub team: i64,
    pub position: i64,
    #[serde(rename = "isPassing")]
    pub is_passing: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leaderboard {
    #[serde(rename = "hasLeaderboard")]
    pub has_leaderboard: bool,
    #[serde(rename = "isVisible")]
    pub is_visible: bool,
    pub ourplayer: LeaderboardPlayer,
    pub slots: Vec<LeaderboardPlayer>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gameplay {
    #[serde(rename = "gameMode")]
    pub game_mode: i64,
    pub name: String,
    pub score: f64,
    pub accuracy: f64,
    pub combo: Combo,
    pub hp: Health,
    pub hits: GameplayHits,
    pub pp: GameplayPerformancePoints,
    #[serde(rename = "keyOverlay")]
    pub key_overlay: KeyOverlay,
    pub leaderboard: Leaderboard,
    #[serde(rename = "_isReplayUiHidden")]
    pub is_replay_ui_hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultsScreen {
    #[serde(rename = "0")]
    pub h0: i64,
    #[serde(rename = "50")]
    pub h50: i64,
    #[serde(rename = "100")]
    pub h100: i64,
    #[serde(rename = "300")]
    pub h300: i64,
    pub mode: i64,
    pub name: String,
    pub score: f64,
    pub accuracy: f64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    pub mods: SelectedMods,
    pub geki: i64,
    pub katu: i64,
    pub grade: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyMessage {
    pub team: String,
    pub time: String,
    pub name: String,
    #[serde(rename = "messageBody")]
    pub message_body: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyScore {
    pub left: f64,
    pub right: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyManagerGameplay {
    pub score: TourneyScore,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyBools {
    #[serde(rename = "scoreVisible")]
    pub score_visible: bool,
    #[serde(rename = "starsVisible")]
    pub stars_visible: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyName {
    pub left: String,
    pub right: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyStars {
    pub left: i64,
    pub right: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyManager {
    #[serde(rename = "ipcState")]
    pub ipc_state: i64,
    #[serde(rename = "bestOF")]
    pub best_of: i64,
    #[serde(rename = "teamName")]
    pub team_name: TourneyName,
    pub stars: TourneyStars,
    pub bools: TourneyBools,
    pub chat: Vec<TourneyMessage>,
    pub gameplay: TourneyManagerGameplay,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyHits {
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
    pub grade: GameplayGrade,
    #[serde(rename = "unstableRate")]
    pub unstable_rate: f64,
    #[serde(rename = "hitErrorArray")]
    pub hit_error_array: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyGameplay {
    #[serde(rename = "gameMode")]
    pub game_mode: i64,
    pub name: String,
    pub score: f64,
    pub accuracy: f64,
    pub combo: Combo,
    pub hp: Health,
    pub hits: TourneyHits,
    pub mods: SelectedMods,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyIpcSpec {
    pub name: String,
    pub country: String,
    #[serde(rename = "userID")]
    pub user_id: i64,
    pub accuracy: f64,
    #[serde(rename = "rankedScore")]
    pub ranked_score: f64,
    #[serde(rename = "playCount")]
    pub play_count: i64,
    #[serde(rename = "globalRank")]
    pub global_rank: i64,
    #[serde(rename = "totalPP")]
    pub total_pp: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyIpcClient {
    pub team: String,
    pub spectating: TourneyIpcSpec,
    pub gameplay: TourneyGameplay,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tourney {
    pub manager: TourneyManager,
    #[serde(rename = "ipcClients")]
    pub ipc_clients: Vec<TourneyIpcClient>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserProfile {
    #[serde(rename = "rawLoginStatus", skip_serializing_if = "Option::is_none")]
    pub raw_login_status: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<f64>,
    #[serde(rename = "rankedScore", skip_serializing_if = "Option::is_none")]
    pub ranked_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<f64>,
    #[serde(rename = "playCount", skip_serializing_if = "Option::is_none")]
    pub play_count: Option<i64>,
    #[serde(rename = "playMode", skip_serializing_if = "Option::is_none")]
    pub play_mode: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<i64>,
    #[serde(rename = "countryCode", skip_serializing_if = "Option::is_none")]
    pub country_code: Option<i64>,
    #[serde(rename = "performancePoints", skip_serializing_if = "Option::is_none")]
    pub performance_points: Option<f64>,
    #[serde(rename = "rawBanchoStatus", skip_serializing_if = "Option::is_none")]
    pub raw_bancho_status: Option<i64>,
    #[serde(rename = "backgroundColour", skip_serializing_if = "Option::is_none")]
    pub background_colour: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub client: String,
    pub settings: Settings,
    pub menu: Menu,
    pub gameplay: Gameplay,
    #[serde(rename = "resultsScreen")]
    pub results_screen: ResultsScreen,
    #[serde(rename = "userProfile")]
    pub user_profile: UserProfile,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tourney: Option<Tourney>,
}

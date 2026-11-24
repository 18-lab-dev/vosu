//! Protocol-owned immutable facts decoded directly from one authorized bus cut.
use serde::{Deserialize, de::DeserializeOwned};
use std::{collections::BTreeMap, sync::Arc};
use vosu_osu_model::source::{Combo, HealthRaw, Judgements, OverlaySlots, TourneyAvailability};
use vosu_plugin_api::Selector;
use vosu_surface::{requester::BundledRequester, state::{CapturedSelection, SessionTarget}};
use crate::Error;

/// Only emitted fields create subscription demand; extension histories are excluded.
pub const POINTERS: &[&str] = &[
    "/session/state", "/settings/interfaceVisible", "/settings/replayUIVisible",
    "/settings/chatVisibilityStatus", "/settings/bassDensity", "/beatmap",
    "/internal/paths", "/play/score", "/play/combo", "/play/accuracyPercent",
    "/play/healthRaw", "/play/judgements", "/play/mode", "/play/playerName",
    "/play/statistics", "/play/rank", "/play/unstableRate", "/play/hitErrors/all",
    "/play/mods", "/play/pp", "/keys/overlay", "/leaderboard", "/leaderboardState",
    "/performance", "/result", "/profile", "/tourney", "/tourneyAvailability",
];
pub fn selectors() -> Vec<Selector> {
    POINTERS.iter().map(|pointer| Selector { ns: "vosu.osu".into(), pointer: (*pointer).into() }).collect()
}
#[derive(Clone)]
pub struct View {
    pub capture: CapturedSelection,
    pub client: String,
    pub state: i64,
    pub interface_visible: bool,
    pub replay_ui_visible: bool,
    pub chat_status: i64,
    pub bass_density: f64,
    pub beatmap: Beatmap,
    pub paths: Paths,
    pub play: Play,
    pub keys: Option<OverlaySlots>,
    pub leaderboard: Arc<[Slot]>,
    pub leaderboard_state: LeaderboardState,
    pub performance: Performance,
    pub result: ResultScreen,
    pub profile: Profile,
    pub tourney: Option<Tourney>,
    pub tourney_availability: TourneyAvailability,
}
impl View {
    /// HTTP uses this one-shot capture; it registers no demand and forces no sample.
    pub fn capture(requester: &dyn BundledRequester, target: SessionTarget) -> Result<Self, Error> {
        Self::decode(requester.capture(target, &selectors()).map_err(Error::Surface)?)
    }
    pub fn decode(capture: CapturedSelection) -> Result<Self, Error> {
        let session = capture.session.as_ref().ok_or(Error::NotReady)?;
        let client = match session.variant.as_str() {
            "stable" | "lazer" => session.variant.clone(),
            _ => return Err(Error::NotReady),
        };
        fn get<T: DeserializeOwned>(c: &CapturedSelection, pointer: &str) -> Result<T, Error> {
            let entry = c.values.iter().find(|e| e.ns == "vosu.osu" && e.pointer == pointer).ok_or(Error::NotReady)?;
            if entry.error.is_some() { return Err(Error::NotReady); }
            entry.value.as_ref().ok_or(Error::NotReady)?.deserialized().map_err(|_| Error::Decode(format!("invalid selected value at {pointer}")))
        }
        let state: Number = get(&capture, "/session/state")?;
        let view = Self {
            client, state: state.number,
            interface_visible: get(&capture, "/settings/interfaceVisible")?,
            replay_ui_visible: get(&capture, "/settings/replayUIVisible")?,
            chat_status: get(&capture, "/settings/chatVisibilityStatus")?,
            bass_density: get(&capture, "/settings/bassDensity")?,
            beatmap: get(&capture, "/beatmap")?, paths: get(&capture, "/internal/paths")?,
            play: Play {
                score: get(&capture, "/play/score")?, combo: get(&capture, "/play/combo")?,
                accuracy: get(&capture, "/play/accuracyPercent")?, health: get(&capture, "/play/healthRaw")?,
                judgements: get(&capture, "/play/judgements")?, mode: get(&capture, "/play/mode")?,
                player_name: get(&capture, "/play/playerName")?, statistics: get(&capture, "/play/statistics")?,
                rank: get(&capture, "/play/rank")?, unstable_rate: get(&capture, "/play/unstableRate")?,
                hit_errors: get(&capture, "/play/hitErrors/all")?, mods: get(&capture, "/play/mods")?,
                pp: get(&capture, "/play/pp")?,
            },
            keys: get(&capture, "/keys/overlay")?, leaderboard: get(&capture, "/leaderboard")?,
            leaderboard_state: get(&capture, "/leaderboardState")?, performance: get(&capture, "/performance")?,
            result: get(&capture, "/result")?, profile: get(&capture, "/profile")?,
            tourney: get(&capture, "/tourney")?, tourney_availability: get(&capture, "/tourneyAvailability")?,
            capture,
        };
        Ok(view)
    }
}
#[derive(Debug, Clone, Deserialize)]
pub struct Number { pub number: i64 }
#[derive(Debug, Clone, Deserialize)]
pub struct OptionalNumber { pub number: Option<i64> }
#[derive(Debug, Clone, Deserialize)]
pub struct Mods { pub number: i64, pub name: String }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Beatmap {
    pub selection_mode: Option<i64>, pub id: Option<i64>, pub set: Option<i64>,
    pub checksum: Option<String>, pub status: OptionalFloat,
    pub artist: Option<String>, pub artist_unicode: Option<String>, pub title: Option<String>,
    pub title_unicode: Option<String>, pub mapper: Option<String>, pub version: Option<String>,
    pub time: Time, pub stats: Stats,
}
#[derive(Debug, Clone, Deserialize)]
pub struct OptionalFloat { pub number: Option<f64> }
    #[serde(deserialize_with = "crate::number::optional")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Time { pub live: f64, pub first_hit_object: Option<i64>, pub last_object: f64, pub mp3_length: Option<f64> }
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::optional")]
#[derive(Debug, Clone, Deserialize)]
pub struct Difficulty { pub original: f64, pub converted: f64 }
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
pub struct Stars { pub live: f64, pub total: f64 }
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
pub struct Bpm { pub realtime: f64, pub common: f64, pub min: f64, pub max: f64 }
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
pub struct Objects { pub circles: i64, pub sliders: i64, pub spinners: i64, pub holds: i64 }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats { pub ar: Difficulty, pub cs: Difficulty, pub od: Difficulty, pub hp: Difficulty, pub stars: Stars, pub bpm: Bpm, pub objects: Objects, pub max_combo: i64 }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paths { pub stable: Option<StablePaths>, pub lazer: Option<LazerPaths>, pub current: Option<CurrentPaths>, pub skin_folder: Option<String> }
#[derive(Debug, Clone, Deserialize)]
pub struct StablePaths { pub game: Option<String>, pub songs: Option<String> }
#[derive(Debug, Clone, Deserialize)]
pub struct LazerPaths { pub data: Option<String>, pub files: Option<String> }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentPaths { pub beatmap_folder: Option<String>, pub beatmap_file: Option<String>, pub background: Option<String>, pub audio: Option<String> }
#[derive(Debug, Clone)]
pub struct Play {
    pub score: f64, pub combo: Combo, pub accuracy: f64, pub health: HealthRaw, pub judgements: Judgements,
    pub mode: Number, pub player_name: String, pub statistics: Statistics, pub rank: Rank,
    pub unstable_rate: f64, pub hit_errors: Arc<[f64]>, pub mods: Mods, pub pp: Pp,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Statistics { pub slider_end_hits: i64, pub small_tick_hit: i64, pub large_tick_hit: i64, pub small_tick_miss: i64, pub large_tick_miss: i64, pub large_bonus: i64, pub slider_breaks: i64 }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rank { pub current: String, pub max_this_play: String }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pp { pub current: f64, pub fc: f64, pub max_achievable: f64 }
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player { pub name: String, pub score: f64, pub combo: i64, pub max_combo: i64, pub mods: Mods, pub judgements: Judgements, pub team: i64, pub position: i64, pub is_passing: bool }
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
pub struct LeaderboardState { pub visible: bool, pub player: Player }
#[derive(Debug, Clone, Deserialize)]
pub struct Performance { pub accuracy: BTreeMap<String, f64>, pub graph: Graph }
#[derive(Debug, Clone, Deserialize)]
pub struct Graph { pub primary: Arc<[f64]>, pub series: Arc<[Series]>, pub xaxis: Arc<[f64]> }
#[derive(Debug, Clone, Deserialize)]
pub struct Series { pub name: String, pub data: Arc<[f64]> }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultScreen { pub name: String, pub score: f64, pub accuracy: f64, pub max_combo: i64, pub mods: Mods, pub statistics: Judgements, pub rank: String, pub created_at: String }
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub user_status: OptionalNumber, pub bancho_status: OptionalNumber, pub mode: OptionalNumber, pub country_code: OptionalNumber,
    pub id: Option<i64>, pub name: Option<String>, pub ranked_score: Option<f64>, pub level: Option<f64>, pub accuracy: Option<f64>,
    pub pp: Option<f64>, pub play_count: Option<i64>, pub global_rank: Option<i64>, pub background_colour: Option<String>,
    #[serde(deserialize_with = "crate::number::optional")]
    #[serde(deserialize_with = "crate::number::optional")]
    #[serde(deserialize_with = "crate::number::optional")]
    #[serde(deserialize_with = "crate::number::optional")]
}
#[derive(Debug, Clone, Deserialize)]
pub struct Sides<T> { pub left: T, pub right: T }
#[derive(Debug, Clone, Deserialize)]
pub struct ScoreSides {
    #[serde(deserialize_with = "crate::number::float")]
    pub left: f64,
    #[serde(deserialize_with = "crate::number::float")]
    pub right: f64,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot { pub name: Option<String>, pub score: f64, pub combo: Combo, pub mods: Mods, pub statistics: Judgements, pub team: i64, pub position: i64, pub is_failed: bool }
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tourney {
    pub ipc_state: i64, pub best_of: i64, pub team: Sides<String>, pub points: Sides<i64>,
    pub score_visible: bool, pub stars_visible: bool, pub chat: Arc<[Message]>,
    pub total_score: Sides<f64>, pub clients: Arc<[Client]>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Message { pub timestamp: String, pub name: String, pub message: String }
#[derive(Debug, Clone, Deserialize)]
pub struct Client { pub team: String, pub user: TourneyUser, pub play: ClientPlay }
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TourneyUser { pub id: i64, pub name: String, pub country: String, pub accuracy: f64, pub ranked_score: f64, pub play_count: i64, pub global_rank: i64, #[serde(rename = "totalPP")] pub total_pp: f64 }
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientPlay {
    pub player_name: String, pub mode: Number, pub score: f64, pub accuracy: f64, pub combo: Combo,
    pub health_raw: HealthRaw, pub raw_judgements: Judgements, pub statistics: Statistics,
    pub hit_errors: HitErrors, pub mods: Mods, pub rank: Rank, pub unstable_rate: f64,
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
    #[serde(deserialize_with = "crate::number::float")]
}
#[derive(Debug, Clone, Deserialize)]
pub struct HitErrors { pub all: Arc<[f64]> }

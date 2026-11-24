//! streamCompanion-specific immutable selection. No reader or calculation state crosses this seam.
use crate::Error;
use serde::{Deserialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::sync::Arc;
use vosu_osu_model::{
    source::{Combo, HealthRaw, Judgements, LeaderboardState, OverlaySlots},
    states::{CalculateMods, Statistics},
};
use vosu_plugin_api::{CborValue, Selector};
use vosu_surface::{
    requester::BundledRequester,
    state::{CapturedSelection, SessionTarget},
};

/// The source image is retained for session/generation/revision and chart-resource identity.
#[derive(Clone, Debug)]
pub struct View {
    pub capture: CapturedSelection,
    pub metadata: Metadata,
    pub paths: Paths,
    pub play: Play,
    pub profile: Profile,
    pub raw_status: i64,
    pub watching_replay: bool,
    pub play_time: f64,
    pub chat_status: i64,
    pub interface_visible: bool,
    pub ranking_type: i64,
    pub overlay: OverlaySlots,
    pub hit_errors: Arc<[f64]>,
    pub leaderboard: Arc<[LeaderboardPlayer]>,
    pub leaderboard_state: LeaderboardState,
    pub pp: Performance,
    pub pp_accuracy: BTreeMap<u8, f64>,
    pub live_stars: f64,
    pub current_bpm: f64,
    pub result_mode: Option<i64>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub artist: Option<String>,
    pub artist_unicode: Option<String>,
    pub title: Option<String>,
    pub title_unicode: Option<String>,
    pub mapper: Option<String>,
    pub version: Option<String>,
    pub checksum: Option<String>,
    pub id: Option<i64>,
    pub set: Option<i64>,
    pub selection_mode: Option<i64>,
    pub status: NumberName<f64>,
    pub time: MapTime,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MapTime {
    pub mp3_length: Option<f64>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Paths {
    pub current: Option<CurrentPaths>,
    pub skin_folder: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentPaths {
    pub beatmap_folder: Option<String>,
    pub beatmap_file: Option<String>,
    pub background: Option<String>,
    pub audio: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct NumberName<T> {
    pub number: Option<T>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: Option<i64>,
    pub name: Option<String>,
    pub user_status: NumberName<i64>,
    pub country_code: NumberName<i64>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Play {
    pub player_name: String,
    pub mode: NumberName<i64>,
    pub score: f64,
    pub combo: Combo,
    pub accuracy_percent: f64,
    pub health_raw: HealthRaw,
    pub judgements: Judgements,
    pub retries: i64,
    pub precise_unstable_rate: f64,
    pub mods: CalculateMods,
    pub rank: Rank,
    pub statistics: TickStatistics,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rank {
    pub current: String,
    pub max_this_play: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TickStatistics {
    pub slider_breaks: i64,
    pub small_tick_miss: i64,
    pub small_tick_hit: i64,
    pub large_tick_miss: i64,
    pub large_tick_hit: i64,
    pub large_bonus: i64,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaderboardPlayer {
    pub name: String,
    pub score: f64,
    pub combo: Combo,
    pub mods: CalculateMods,
    pub statistics: Judgements,
    pub team: i64,
    pub position: i64,
    pub is_failed: bool,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Performance {
    pub current: f64,
    pub fc: f64,
    pub detailed: DetailedPerformance,
}
#[derive(Clone, Debug, Deserialize)]
pub struct DetailedPerformance {
    pub current: PerformanceComponents,
}
#[derive(Clone, Debug, Deserialize)]
pub struct PerformanceComponents {
    pub accuracy: f64,
    pub aim: f64,
    pub speed: f64,
    pub difficulty: f64,
}
#[derive(Deserialize)]
struct Stats {
    stars: Stars,
    bpm: Bpm,
}
#[derive(Deserialize)]
struct Stars {
    live: f64,
}
#[derive(Deserialize)]
struct Bpm {
    realtime: f64,
}

/// Exact selected pointers; internal authority is restricted to path echoes.
pub const POINTERS: &[&str] = &[
    "/beatmap",
    "/internal/paths",
    "/play/playerName",
    "/play/mode",
    "/play/score",
    "/play/combo",
    "/play/accuracyPercent",
    "/play/healthRaw",
    "/play/judgements",
    "/play/retries",
    "/play/preciseUnstableRate",
    "/play/mods",
    "/play/rank",
    "/play/statistics",
    "/profile",
    "/session/state",
    "/session/watchingReplay",
    "/settings/chatVisibilityStatus",
    "/settings/interfaceVisible",
    "/settings/leaderboard",
    "/keys/overlay",
    "/play/hitErrors/all",
    "/leaderboard",
    "/leaderboardState",
    "/play/pp",
    "/performance/accuracy",
    "/result/mode",
];
pub fn selectors() -> Vec<Selector> {
    POINTERS
        .iter()
        .map(|pointer| Selector {
            ns: "vosu.osu".into(),
            pointer: (*pointer).into(),
        })
        .chain(std::iter::once(Selector {
            ns: "vosu.chart".into(),
            pointer: "/resources/file".into(),
        }))
        .collect()
}
impl View {
    pub fn capture(requester: &dyn BundledRequester, target: SessionTarget) -> Result<Self, Error> {
        Self::decode(
            requester
                .capture(target, &selectors())
                .map_err(|e| Error::Source(e.to_string()))?,
        )
    }
    pub fn decode(capture: CapturedSelection) -> Result<Self, Error> {
        if capture.session.is_none() {
            return Err(Error::NotReady);
        }
        let image = Image(&capture);
        let stats: Stats = image.get("/beatmap/stats")?;
        let mode: NumberName<i64> = image.get("/result/mode")?;
        let state: NumberName<i64> = image.get("/session/state")?;
        let ranking: NumberName<i64> = image.get("/settings/leaderboard")?;
        let view = Self {
            metadata: image.get("/beatmap")?,
            paths: image.get("/internal/paths")?,
            play: Play {
                player_name: image.get("/play/playerName")?,
                mode: image.get("/play/mode")?,
                score: image.get("/play/score")?,
                combo: image.get("/play/combo")?,
                accuracy_percent: image.get("/play/accuracyPercent")?,
                health_raw: image.get("/play/healthRaw")?,
                judgements: image.get("/play/judgements")?,
                retries: image.get("/play/retries")?,
                precise_unstable_rate: image.get("/play/preciseUnstableRate")?,
                mods: image.get("/play/mods")?,
                rank: image.get("/play/rank")?,
                statistics: image.get("/play/statistics")?,
            },
            profile: image.get("/profile")?,
            raw_status: state.number.ok_or(Error::NotReady)?,
            watching_replay: image.get("/session/watchingReplay")?,
            play_time: image.get("/beatmap/time/live")?,
            chat_status: image
                .get::<NumberName<i64>>("/settings/chatVisibilityStatus")?
                .number
                .ok_or(Error::NotReady)?,
            interface_visible: image.get("/settings/interfaceVisible")?,
            ranking_type: ranking.number.unwrap_or_default(),
            overlay: image.get("/keys/overlay")?,
            hit_errors: image.get("/play/hitErrors/all")?,
            leaderboard: image.get("/leaderboard")?,
            leaderboard_state: image.get("/leaderboardState")?,
            pp: image.get("/play/pp")?,
            pp_accuracy: image
                .get::<BTreeMap<String, f64>>("/performance/accuracy")?
                .into_iter()
                .map(|(k, v)| {
                    k.parse()
                        .map(|k| (k, v))
                        .map_err(|_| Error::Source("invalid accuracy key".into()))
                })
                .collect::<Result<_, _>>()?,
            live_stars: stats.stars.live,
            current_bpm: stats.bpm.realtime,
            result_mode: mode.number,
            capture: capture.clone(),
        };
        Ok(view)
    }
    pub(crate) fn statistics(&self) -> Statistics {
        let j = self.play.judgements;
        Statistics {
            miss: j.miss,
            meh: j.meh,
            ok: j.ok,
            good: j.good,
            great: j.great,
            perfect: j.perfect,
            small_tick_miss: self.play.statistics.small_tick_miss,
            small_tick_hit: self.play.statistics.small_tick_hit,
            large_tick_miss: self.play.statistics.large_tick_miss,
            large_tick_hit: self.play.statistics.large_tick_hit,
            large_bonus: self.play.statistics.large_bonus,
            ..vosu_osu_model::empty_statistics()
        }
    }
    pub(crate) fn mode(&self) -> Option<i64> {
        match self.raw_status {
            2 => self.play.mode.number,
            7 => self.result_mode,
            _ => self.metadata.selection_mode,
        }
    }
}
struct Image<'a>(&'a CapturedSelection);
impl Image<'_> {
    fn get<T: DeserializeOwned>(&self, pointer: &str) -> Result<T, Error> {
        let entry = self
            .0
            .values
            .iter()
            .filter(|entry| {
                entry.ns == "vosu.osu"
                    && (entry.pointer == pointer
                        || pointer
                            .strip_prefix(&entry.pointer)
                            .is_some_and(|rest| rest.starts_with('/')))
            })
            .max_by_key(|entry| entry.pointer.len())
            .ok_or(Error::NotReady)?;
        if entry.error.is_some() {
            return Err(Error::NotReady);
        }
        let mut value = entry.value.as_ref().ok_or(Error::NotReady)?;
        for token in pointer[entry.pointer.len()..]
            .strip_prefix('/')
            .into_iter()
            .flat_map(|rest| rest.split('/'))
        {
            let CborValue::Map(fields) = value else {
                return Err(Error::NotReady);
            };
            value = fields
                .iter()
                .find_map(|(key, value)| (key.as_text() == Some(token)).then_some(value))
                .ok_or(Error::NotReady)?;
        }
        if matches!(value, CborValue::Null) {
            return Err(Error::NotReady);
        }
        value
            .deserialized()
            .map_err(|e| Error::Source(format!("{pointer}: {e}")))
    }
}

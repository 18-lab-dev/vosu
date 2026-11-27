//! Adapter-owned typed source records; these are not a second state channel.
use crate::dto::*;
use serde::Deserialize;
use vosu_osu_model::{source::Judgements, states::CalculateMods};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Statistics {
    pub slider_breaks: i64,
    pub slider_end_hits: i64,
    pub small_tick_hit: i64,
    pub large_tick_hit: i64,
}
#[derive(Debug, Deserialize)]
pub(crate) struct HitErrors {
    pub all: Vec<f64>,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourcePlay {
    pub failed: bool,
    pub player_name: String,
    pub mode: NumberName,
    pub score: f64,
    #[serde(alias = "accuracyPercent")]
    pub accuracy: f64,
    pub health_bar: HealthBar,
    pub judgements: Judgements,
    pub statistics: Statistics,
    pub hit_errors: HitErrors,
    pub combo: Combo,
    pub mods: CalculateMods,
    pub rank: Rank,
    pub pp: Pp,
    pub unstable_rate: f64,
}
impl SourcePlay {
    pub fn into_wire(self, lazer_spectator: bool) -> Play {
        let h = self.judgements;
        let s = self.statistics;
        let hits = if lazer_spectator {
            // Lazer aggregate judgements are already the pinned legacy conversion.
            PlayHits::Lazer(LazerHits {
                h0: h.miss,
                h50: h.meh,
                h100: h.ok,
                h300: h.great,
                geki: h.perfect,
                katu: h.good,
                slider_end_hits: s.slider_end_hits,
                small_tick_hits: s.small_tick_hit,
                large_tick_hits: s.large_tick_hit,
                slider_breaks: s.slider_breaks,
            })
        } else {
            PlayHits::Normal(Hits {
                h0: h.miss,
                h50: h.meh,
                h100: h.ok,
                h300: h.great,
                geki: h.perfect,
                katu: h.good,
                slider_breaks: s.slider_breaks,
                slider_end_hits: s.slider_end_hits,
                small_tick_hits: s.small_tick_hit,
                large_tick_hits: s.large_tick_hit,
            })
        };
        Play {
            failed: self.failed,
            player_name: self.player_name,
            mode: self.mode,
            score: self.score,
            accuracy: self.accuracy,
            health_bar: self.health_bar,
            hits,
            hit_error_array: self.hit_errors.all,
            combo: self.combo,
            mods: self.mods,
            rank: self.rank,
            pp: self.pp,
            unstable_rate: self.unstable_rate,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceClient {
    pub ipc_id: f64,
    pub team: TourneyTeam,
    pub settings: TourneyClientSettings,
    pub user: TourneyUser,
    pub beatmap: TourneyBeatmap,
    pub play: SourcePlay,
}
#[cfg(test)]
#[derive(Debug, Deserialize)]
pub(crate) struct SourceChat {
    pub team: String,
    pub timestamp: String,
    pub name: String,
    pub message: String,
}
#[cfg(test)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceTourney {
    pub score_visible: bool,
    pub stars_visible: bool,
    pub ipc_state: i64,
    pub best_of: i64,
    pub team: Sides<String>,
    pub points: Sides<i64>,
    pub chat: Vec<SourceChat>,
    pub total_score: Sides<f64>,
    pub clients: Vec<SourceClient>,
}
#[cfg(test)]
impl SourceTourney {
    pub fn into_wire(self, lazer: bool) -> Tourney {
        Tourney {
            score_visible: self.score_visible,
            stars_visible: self.stars_visible,
            ipc_state: self.ipc_state,
            best_of: self.best_of,
            team: self.team,
            points: self.points,
            chat: self
                .chat
                .into_iter()
                .map(|m| {
                    if lazer {
                        TourneyChatMessage::Lazer(LazerTourneyChatMessage {
                            team: m.team,
                            timestamp: m.timestamp,
                            name: m.name,
                            message: m.message,
                        })
                    } else {
                        TourneyChatMessage::Stable(StableTourneyChatMessage {
                            team: m.team,
                            name: m.name,
                            message: m.message,
                            timestamp: m.timestamp,
                        })
                    }
                })
                .collect(),
            total_score: self.total_score,
            clients: self
                .clients
                .into_iter()
                .map(|c| TourneyClient {
                    ipc_id: c.ipc_id,
                    team: c.team,
                    settings: c.settings,
                    user: c.user,
                    beatmap: c.beatmap,
                    play: c.play.into_wire(lazer),
                })
                .collect(),
        }
    }
}

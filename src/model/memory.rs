use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::enums::RankedPlayStage;
use super::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton, LeaderboardPlayer, MultiplayerTeamType,
    SettingsObject, Statistics, TourneyManagerChatItem,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanPattern {
    pub pattern: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<i64>,
    #[serde(rename = "nonZeroMask", skip_serializing_if = "Option::is_none")]
    pub non_zero_mask: Option<bool>,
    #[serde(rename = "isTourneyOnly", skip_serializing_if = "Option::is_none")]
    pub is_tourney_only: Option<bool>,
}

pub type ScanPatterns = IndexMap<String, ScanPattern>;

/// The bass-density samples exposed to state and wire builders.
///
/// Reader readiness is represented by `process::ReadStatus`, never by a string
/// inhabiting this payload.
pub type AudioVelocityBase = Vec<f64>;
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
pub struct User {
    pub name: String,
    pub accuracy: f64,
    #[serde(rename = "rankedScore")]
    pub ranked_score: f64,
    pub id: i64,
    pub level: f64,
    #[serde(rename = "playCount")]
    pub play_count: i64,
    #[serde(rename = "playMode")]
    pub play_mode: i64,
    pub rank: i64,
    #[serde(rename = "countryCode")]
    pub country_code: i64,
    #[serde(rename = "performancePoints")]
    pub performance_points: f64,
    #[serde(rename = "rawBanchoStatus")]
    pub raw_bancho_status: i64,
    #[serde(rename = "backgroundColour")]
    pub background_colour: i64,
    #[serde(rename = "rawLoginStatus")]
    pub raw_login_status: i64,
    pub matchmaking: Option<MatchmakingStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsPointers {
    pub config: usize,
    pub binding: usize,
}

pub type Offsets = Vec<usize>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigValue {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BindingValue {
    pub key: i64,
    pub value: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultScreen {
    #[serde(rename = "onlineId")]
    pub online_id: f64,
    #[serde(rename = "playerName")]
    pub player_name: String,
    pub mods: CalculateMods,
    pub mode: i64,
    pub accuracy: f64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    pub score: f64,
    pub statistics: Statistics,
    #[serde(rename = "maximumStatistics")]
    pub maximum_statistics: Statistics,
    pub date: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub failed: bool,
    pub retries: i64,
    #[serde(rename = "playerName")]
    pub player_name: String,
    pub mods: CalculateMods,
    pub mode: i64,
    pub score: f64,
    #[serde(rename = "playerHPSmooth")]
    pub player_hp_smooth: f64,
    #[serde(rename = "playerHP")]
    pub player_hp: f64,
    pub accuracy: f64,
    pub statistics: Statistics,
    #[serde(rename = "maximumStatistics")]
    pub maximum_statistics: Statistics,
    pub combo: i64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pp: Option<f64>,
}

pub type Gameplay = Score;
pub type Mp3Length = f64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HitErrors {
    pub index: i64,
    #[serde(flatten)]
    pub errors: HitErrorArray,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_correction: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Global {
    #[serde(rename = "isWatchingReplay")]
    pub is_watching_replay: bool,
    #[serde(rename = "isReplayUiHidden")]
    pub is_replay_ui_hidden: bool,
    #[serde(rename = "isMultiSpectating")]
    pub is_multi_spectating: bool,
    #[serde(rename = "showInterface")]
    pub show_interface: bool,
    #[serde(rename = "chatStatus")]
    pub chat_status: i64,
    pub status: i64,
    #[serde(rename = "playTime")]
    pub play_time: f64,
    #[serde(rename = "gameTime")]
    pub game_time: i64,
    #[serde(rename = "menuMods")]
    pub menu_mods: CalculateMods,
    #[serde(rename = "skinFolder")]
    pub skin_folder: String,
    #[serde(rename = "memorySongsFolder")]
    pub memory_songs_folder: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GlobalPrecise {
    pub status: i64,
    pub time: f64,
}

mod ranked_status_number {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if !value.is_finite() {
            serializer.serialize_none()
        } else if value.fract() == 0.0 && *value >= i64::MIN as f64 && *value <= i64::MAX as f64 {
            serializer.serialize_i64(*value as i64)
        } else {
            serializer.serialize_f64(*value)
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Option::<f64>::deserialize(deserializer)?.unwrap_or(f64::NAN))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuFull {
    pub gamemode: i64,
    pub checksum: String,
    pub filename: String,
    pub plays: i64,
    pub artist: String,
    #[serde(rename = "artistOriginal")]
    pub artist_original: String,
    pub title: String,
    #[serde(rename = "titleOriginal")]
    pub title_original: String,
    pub ar: f64,
    pub cs: f64,
    pub hp: f64,
    pub od: f64,
    #[serde(rename = "audioFilename")]
    pub audio_filename: String,
    #[serde(rename = "audioFileMimetype")]
    pub audio_file_mimetype: String,
    #[serde(rename = "backgroundFilename")]
    pub background_filename: String,
    #[serde(rename = "backgroundFileMimetype")]
    pub background_file_mimetype: String,
    pub folder: String,
    pub creator: String,
    pub difficulty: String,
    #[serde(rename = "mapID")]
    pub map_id: i64,
    #[serde(rename = "setID")]
    pub set_id: i64,
    #[serde(rename = "rankedStatus")]
    #[serde(with = "ranked_status_number")]
    pub ranked_status: f64,
    #[serde(rename = "objectCount")]
    pub object_count: i64,
}

/// The short menu result is still data: mode and ranked status must survive
/// even when the checksum itself has not changed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MenuChecksum {
    pub gamemode: i64,
    #[serde(rename = "rankedStatus")]
    #[serde(with = "ranked_status_number")]
    pub ranked_status: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
// Keep both tagged wire payloads inline; boxing would alter the public model API.
#[allow(clippy::large_enum_variant)]
pub enum Menu {
    #[serde(rename = "update")]
    Full(MenuFull),
    #[serde(rename = "checksum")]
    Checksum(MenuChecksum),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tourney {
    #[serde(rename = "ipcState")]
    pub ipc_state: i64,
    #[serde(rename = "leftStars")]
    pub left_stars: i64,
    #[serde(rename = "rightStars")]
    pub right_stars: i64,
    #[serde(rename = "bestOf")]
    pub best_of: i64,
    #[serde(rename = "starsVisible")]
    pub stars_visible: bool,
    #[serde(rename = "scoreVisible")]
    pub score_visible: bool,
    #[serde(rename = "firstTeamName")]
    pub first_team_name: String,
    #[serde(rename = "secondTeamName")]
    pub second_team_name: String,
    #[serde(rename = "firstTeamScore")]
    pub first_team_score: i64,
    #[serde(rename = "secondTeamScore")]
    pub second_team_score: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TourneyUser {
    pub id: i64,
    pub name: String,
    pub country: String,
    pub accuracy: f64,
    pub playcount: i64,
    #[serde(rename = "rankedScore")]
    pub ranked_score: f64,
    #[serde(rename = "globalRank")]
    pub global_rank: i64,
    pub pp: f64,
}

pub type TourneyChat = Vec<TourneyManagerChatItem>;
pub type Leaderboard = (bool, Option<LeaderboardPlayer>, Vec<LeaderboardPlayer>);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LazerSpectatorEntry {
    pub team: MultiplayerTeamType,
    pub user: User,
    #[serde(rename = "resultScreen", skip_serializing_if = "Option::is_none")]
    pub result_screen: Option<ResultScreen>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<Score>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LazerSpectator {
    pub chat: Vec<TourneyManagerChatItem>,
    #[serde(rename = "spectatingClients")]
    pub spectating_clients: Vec<LazerSpectatorEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedPlayUserInfo {
    pub id: i64,
    pub rating: f64,
    pub life: f64,
    #[serde(rename = "roundsWon")]
    pub rounds_won: i64,
    #[serde(rename = "damageMultiplier")]
    pub damage_multiplier: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedPlay {
    pub stage: RankedPlayStage,
    #[serde(rename = "currentRound")]
    pub current_round: i64,
    #[serde(rename = "starRating")]
    pub star_rating: f64,
    #[serde(rename = "damageMultiplier")]
    pub damage_multiplier: f64,
    #[serde(rename = "activeUserId")]
    pub active_user_id: i64,
    #[serde(rename = "winningUserId")]
    pub winning_user_id: i64,
    pub users: Vec<RankedPlayUserInfo>,
}

pub type Settings = SettingsObject;
pub type KeyOverlay = Vec<KeyOverlayButton>;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MatchmakingStats, Menu, MenuChecksum, RankedPlay};
    use crate::model::enums::RankedPlayStage;

    #[test]
    fn menu_checksum_is_a_typed_wire_payload() {
        let value = serde_json::to_value(Menu::Checksum(MenuChecksum {
            gamemode: 1,
            ranked_status: 4.0,
        }))
        .unwrap();

        assert_eq!(
            value,
            json!({"type": "checksum", "gamemode": 1, "rankedStatus": 4})
        );
    }
    #[test]
    fn ranked_status_preserves_finite_numbers_and_serializes_unknown_as_null() {
        let known = serde_json::to_value(Menu::Checksum(MenuChecksum {
            gamemode: 0,
            ranked_status: 7.0,
        }))
        .unwrap();
        assert!(known["rankedStatus"].is_number());
        assert_eq!(known["rankedStatus"], json!(7));

        let unknown = serde_json::to_value(Menu::Checksum(MenuChecksum {
            gamemode: 0,
            ranked_status: f64::NAN,
        }))
        .unwrap();
        assert_eq!(unknown["rankedStatus"], serde_json::Value::Null);
    }

    #[test]
    fn ranked_play_serializes_in_source_declaration_order() {
        let encoded = serde_json::to_string(&RankedPlay {
            stage: RankedPlayStage::CardDiscard,
            current_round: 2,
            star_rating: 6.5,
            damage_multiplier: 1.25,
            active_user_id: 3,
            winning_user_id: 4,
            users: Vec::new(),
        })
        .unwrap();

        assert_eq!(
            encoded,
            r#"{"stage":2,"currentRound":2,"starRating":6.5,"damageMultiplier":1.25,"activeUserId":3,"winningUserId":4,"users":[]}"#
        );
    }

    #[test]
    fn explicit_none_serializes_as_null_instead_of_a_reader_sentinel() {
        let value = serde_json::to_value(MatchmakingStats {
            rating: 0.0,
            rank: None,
            plays: 0,
            wins: 0,
            is_provisional: false,
        })
        .unwrap();

        assert_eq!(value["rank"], serde_json::Value::Null);
        assert!(value.as_object().unwrap().contains_key("rank"));
    }
}

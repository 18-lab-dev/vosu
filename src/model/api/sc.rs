use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapBreak {
    #[serde(rename = "startTime")]
    pub start_time: f64,
    #[serde(rename = "endTime")]
    pub end_time: f64,
    #[serde(rename = "hasEffect")]
    pub has_effect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapKiaiPoint {
    #[serde(rename = "startTime")]
    pub start_time: f64,
    pub duration: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapTimingPoint {
    #[serde(rename = "startTime")]
    pub start_time: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    #[serde(rename = "beatLength")]
    pub beat_length: f64,
}

/// Exact flat streamCompanion compatibility document.
///
/// Declaration order is wire order. The three nested objects are intentionally
/// JSON strings because that is the historical streamCompanion protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    #[serde(rename = "osuIsRunning")]
    pub osu_is_running: i64,
    #[serde(rename = "chatIsEnabled")]
    pub chat_is_enabled: i64,
    #[serde(rename = "ingameInterfaceIsEnabled")]
    pub ingame_interface_is_enabled: i64,
    #[serde(rename = "isBreakTime")]
    pub is_break_time: i64,

    #[serde(rename = "banchoIsConnected")]
    pub bancho_is_connected: i64,
    #[serde(rename = "banchoId", skip_serializing_if = "Option::is_none")]
    pub bancho_id: Option<i64>,
    #[serde(rename = "banchoUsername", skip_serializing_if = "Option::is_none")]
    pub bancho_username: Option<String>,
    #[serde(rename = "banchoStatus", skip_serializing_if = "Option::is_none")]
    pub bancho_status: Option<i64>,
    #[serde(rename = "banchoCountry")]
    pub bancho_country: String,

    #[serde(rename = "artistRoman", skip_serializing_if = "Option::is_none")]
    pub artist_roman: Option<String>,
    #[serde(rename = "artistUnicode", skip_serializing_if = "Option::is_none")]
    pub artist_unicode: Option<String>,
    #[serde(rename = "titleRoman", skip_serializing_if = "Option::is_none")]
    pub title_roman: Option<String>,
    #[serde(rename = "titleUnicode", skip_serializing_if = "Option::is_none")]
    pub title_unicode: Option<String>,
    #[serde(rename = "mapArtistTitle")]
    pub map_artist_title: String,
    #[serde(rename = "mapArtistTitleUnicode")]
    pub map_artist_title_unicode: String,
    #[serde(rename = "diffName", skip_serializing_if = "Option::is_none")]
    pub diff_name: Option<String>,
    #[serde(rename = "mapDiff")]
    pub map_diff: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,

    #[serde(rename = "liveStarRating")]
    pub live_star_rating: f64,
    #[serde(rename = "mStars")]
    pub m_stars: f64,
    pub ar: f64,
    #[serde(rename = "mAR")]
    pub m_ar: f64,
    pub od: f64,
    #[serde(rename = "mOD")]
    pub m_od: f64,
    pub cs: f64,
    #[serde(rename = "mCS")]
    pub m_cs: f64,
    pub hp: f64,
    #[serde(rename = "mHP")]
    pub m_hp: f64,

    #[serde(rename = "currentBpm")]
    pub current_bpm: f64,
    pub bpm: f64,
    #[serde(rename = "mainBpm")]
    pub main_bpm: f64,
    #[serde(rename = "maxBpm")]
    pub max_bpm: f64,
    #[serde(rename = "minBpm")]
    pub min_bpm: f64,
    #[serde(rename = "mMainBpm")]
    pub m_main_bpm: f64,
    #[serde(rename = "mMaxBpm")]
    pub m_max_bpm: f64,
    #[serde(rename = "mMinBpm")]
    pub m_min_bpm: f64,
    #[serde(rename = "mBpm")]
    pub m_bpm: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    #[serde(rename = "gameMode", skip_serializing_if = "Option::is_none")]
    pub game_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<i64>,
    pub time: f64,
    pub previewtime: i64,
    pub totaltime: f64,
    #[serde(rename = "timeLeft")]
    pub time_left: String,
    pub drainingtime: f64,
    #[serde(rename = "totalAudioTime", skip_serializing_if = "Option::is_none")]
    pub total_audio_time: Option<f64>,
    #[serde(rename = "firstHitObjectTime")]
    pub first_hit_object_time: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapsetid: Option<i64>,
    #[serde(rename = "mapStrains")]
    pub map_strains: IndexMap<String, f64>,
    #[serde(rename = "mapBreaks")]
    pub map_breaks: Vec<MapBreak>,
    #[serde(rename = "mapKiaiPoints")]
    pub map_kiai_points: Vec<MapKiaiPoint>,
    #[serde(rename = "mapPosition")]
    pub map_position: String,
    #[serde(rename = "mapTimingPoints")]
    pub map_timing_points: Vec<MapTimingPoint>,

    pub sliders: i64,
    pub circles: i64,
    pub spinners: i64,
    #[serde(rename = "maxCombo")]
    pub max_combo: i64,
    #[serde(rename = "mp3Name", skip_serializing_if = "Option::is_none")]
    pub mp3_name: Option<String>,
    #[serde(rename = "osuFileName", skip_serializing_if = "Option::is_none")]
    pub osu_file_name: Option<String>,
    #[serde(
        rename = "backgroundImageFileName",
        skip_serializing_if = "Option::is_none"
    )]
    pub background_image_file_name: Option<String>,
    #[serde(rename = "osuFileLocation")]
    pub osu_file_location: String,
    #[serde(rename = "backgroundImageLocation")]
    pub background_image_location: String,

    pub retries: i64,
    pub username: String,
    pub score: f64,
    #[serde(rename = "playerHp")]
    pub player_hp: f64,
    #[serde(rename = "playerHpSmooth")]
    pub player_hp_smooth: f64,
    pub combo: i64,
    #[serde(rename = "currentMaxCombo")]
    pub current_max_combo: i64,
    #[serde(rename = "keyOverlay")]
    pub key_overlay: String,

    pub geki: i64,
    pub c300: i64,
    pub katsu: i64,
    pub c100: i64,
    pub c50: i64,
    pub miss: i64,
    #[serde(rename = "sliderBreaks")]
    pub slider_breaks: i64,
    pub acc: f64,
    #[serde(rename = "unstableRate")]
    pub unstable_rate: f64,
    #[serde(rename = "convertedUnstableRate")]
    pub converted_unstable_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grade: Option<i64>,
    #[serde(rename = "maxGrade", skip_serializing_if = "Option::is_none")]
    pub max_grade: Option<i64>,
    #[serde(rename = "hitErrors")]
    pub hit_errors: Vec<f64>,
    #[serde(rename = "pressHitErrors")]
    pub press_hit_errors: Vec<f64>,
    #[serde(rename = "releaseHitErrors")]
    pub release_hit_errors: Vec<f64>,
    pub mods: String,
    #[serde(rename = "modsEnum")]
    pub mods_enum: i64,
    #[serde(rename = "ppIfMapEndsNow")]
    pub pp_if_map_ends_now: f64,
    #[serde(rename = "ppIfRestFced")]
    pub pp_if_rest_fced: f64,

    #[serde(rename = "leaderBoardMainPlayer")]
    pub leader_board_main_player: String,
    #[serde(rename = "leaderBoardPlayers")]
    pub leader_board_players: String,
    #[serde(rename = "rankedStatus", skip_serializing_if = "Option::is_none")]
    pub ranked_status: Option<f64>,
    #[serde(rename = "songSelectionRankingType")]
    pub song_selection_ranking_type: i64,
    #[serde(rename = "rawStatus")]
    pub raw_status: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    pub dl: String,

    #[serde(rename = "osu_90PP")]
    pub osu_90_pp: f64,
    #[serde(rename = "osu_95PP")]
    pub osu_95_pp: f64,
    #[serde(rename = "osu_96PP")]
    pub osu_96_pp: f64,
    #[serde(rename = "osu_97PP")]
    pub osu_97_pp: f64,
    #[serde(rename = "osu_98PP")]
    pub osu_98_pp: f64,
    #[serde(rename = "osu_99PP")]
    pub osu_99_pp: f64,
    #[serde(rename = "osu_SSPP")]
    pub osu_sspp: f64,
    #[serde(rename = "osu_m90PP")]
    pub osu_m90_pp: f64,
    #[serde(rename = "osu_m95PP")]
    pub osu_m95_pp: f64,
    #[serde(rename = "osu_m96PP")]
    pub osu_m96_pp: f64,
    #[serde(rename = "osu_m97PP")]
    pub osu_m97_pp: f64,
    #[serde(rename = "osu_m98PP")]
    pub osu_m98_pp: f64,
    #[serde(rename = "osu_m99PP")]
    pub osu_m99_pp: f64,
    #[serde(rename = "osu_mSSPP")]
    pub osu_m_sspp: f64,

    #[serde(rename = "accPpIfMapEndsNow")]
    pub acc_pp_if_map_ends_now: f64,
    #[serde(rename = "aimPpIfMapEndsNow")]
    pub aim_pp_if_map_ends_now: f64,
    #[serde(rename = "speedPpIfMapEndsNow")]
    pub speed_pp_if_map_ends_now: f64,
    #[serde(rename = "strainPpIfMapEndsNow")]
    pub strain_pp_if_map_ends_now: f64,
    #[serde(rename = "noChokePp")]
    pub no_choke_pp: f64,

    pub skin: String,
    #[serde(rename = "skinPath")]
    pub skin_path: String,
    #[serde(rename = "songSelectionScores")]
    pub song_selection_scores: String,
    #[serde(rename = "songSelectionMainPlayerScore")]
    pub song_selection_main_player_score: String,
    #[serde(rename = "songSelectionTotalScores")]
    pub song_selection_total_scores: i64,
    pub status: i64,

    #[serde(rename = "osu_99_9PP")]
    pub osu_99_9_pp: i64,
    #[serde(rename = "osu_m99_9PP")]
    pub osu_m99_9_pp: i64,
    #[serde(rename = "mania_1_000_000PP")]
    pub mania_1_000_000_pp: i64,
    #[serde(rename = "mania_m1_000_000PP")]
    pub mania_m1_000_000_pp: i64,
    #[serde(rename = "simulatedPp")]
    pub simulated_pp: f64,
    pub plays: i64,
    pub tags: String,
    pub source: String,
    #[serde(rename = "starsNomod")]
    pub stars_nomod: i64,
    #[serde(rename = "comboLeft")]
    pub combo_left: i64,
    #[serde(rename = "localTime")]
    pub local_time: i64,
    #[serde(rename = "localTimeISO")]
    pub local_time_iso: String,
    pub sl: i64,
    pub sv: i64,
    pub threadid: i64,
    pub test: String,
}

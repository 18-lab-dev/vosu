//! Source-compatible flat streamCompanion `/json/sc` builder.

use std::cmp::Ordering;

use bytes::Bytes;
use indexmap::IndexMap;
use serde::Serialize;

use crate::api::fixed::{fix_decimals, fix_decimals_to};
use crate::api::{ApiError, ApiSchema, CombinedSnapshot};
use crate::calculation::{js_json_number, safe_join, to_legacy_hits};
use crate::model::api::sc::{MapBreak, MapTimingPoint, Response};
use crate::model::enums::{CountryCodes, Rulesets};
use crate::model::states::{CalculateMods, KeyOverlayButton, LeaderboardPlayer};
use crate::state::{LeaderboardPlayerState, RegularState};

const PLAY: i64 = 2;
const RESULT_SCREEN: i64 = 7;

/// Builds the exact streamCompanion document from one immutable publication.
pub fn build(snapshot: &CombinedSnapshot) -> Result<Response, ApiError> {
    let regular = snapshot.regular().ok_or(ApiError::NotReady)?;
    let precise = snapshot.precise().ok_or(ApiError::NotReady)?;
    let global = &regular.global;
    let menu = &regular.menu;
    let gameplay = &regular.gameplay;
    let beatmap = &regular.beatmap;
    let parsed = beatmap.parsed.as_deref();
    let attributes = beatmap.attributes.unwrap_or_default();

    let raw_status = precise.status;
    let status = stream_companion_status(raw_status, global.is_watching_replay);
    let current_mods = current_mods(regular, raw_status);
    let current_mode = current_mode(regular, raw_status);
    let hits = to_legacy_hits(gameplay.mode as i32, &gameplay.statistics);

    let user = regular.user.profile.as_ref();
    let raw_login_status = user.map(|user| user.raw_login_status);
    let artist = js_undefined(menu.artist.as_deref());
    let artist_original = js_undefined(menu.artist_original.as_deref());
    let title = js_undefined(menu.title.as_deref());
    let title_original = js_undefined(menu.title_original.as_deref());
    let difficulty = js_undefined(menu.difficulty.as_deref());

    let original = parsed.map(|map| &map.engine.original_difficulty);
    let converted = parsed.map(|map| &map.engine.converted_difficulty);
    let map_difficulty = parsed.map(|map| &map.engine.difficulty);
    let bpm = parsed.map(|map| map.bpm).unwrap_or_default();
    let times = parsed.map(|map| map.times).unwrap_or_default();
    let objects = parsed.map(|map| map.object_counts).unwrap_or_default();
    let pp = |accuracy: u8| beatmap.pp_acc.get(&accuracy).copied().unwrap_or_default();

    let common_bpm = round4(bpm.common);
    let max_bpm = round4(bpm.maximum);
    let min_bpm = round4(bpm.minimum);
    let first_object = i64::from(times.first_object);
    let play_time = precise.play_time;
    let (full, time_left, drainingtime) = map_time_projection(times.full, first_object, play_time);
    let key_overlay = encode_json(&KeyOverlayWire::new(
        snapshot.runtime_settings().enable_key_overlay,
        &precise.key_overlay,
    ))?;
    let leader_board_main_player = encode_json(&LeaderboardMainWire::from_state(
        gameplay.leaderboard_visible,
        &gameplay.leaderboard_player,
    ))?;
    let leader_board_players = encode_json(
        &gameplay
            .leaderboard_scores
            .iter()
            .map(LeaderboardPlayerWire::from_player)
            .collect::<Vec<_>>(),
    )?;

    Ok(Response {
        osu_is_running: 1,
        chat_is_enabled: i64::from(global.chat_status > 0),
        ingame_interface_is_enabled: i64::from(global.show_interface),
        is_break_time: 0,
        bancho_is_connected: i64::from(raw_login_status == Some(65_793)),
        bancho_id: user.map(|user| user.id),
        bancho_username: user.map(|user| user.name.clone()),
        bancho_status: raw_login_status,
        bancho_country: user.map_or_else(String::new, |user| country_name(user.country_code)),
        artist_roman: menu.artist.clone(),
        artist_unicode: menu.artist_original.clone(),
        title_roman: menu.title.clone(),
        title_unicode: menu.title_original.clone(),
        map_artist_title: format!("{artist} - {title}"),
        map_artist_title_unicode: format!("{artist_original} - {title_original}"),
        diff_name: menu.difficulty.clone(),
        map_diff: format!("[{difficulty}]"),
        creator: menu.creator.clone(),
        live_star_rating: fix_decimals(attributes.stars),
        m_stars: fix_decimals(map_difficulty.map_or(0.0, |value| value.stars)),
        ar: fix_decimals(original.map_or(0.0, |value| value.approach_rate)),
        m_ar: fix_decimals(converted.map_or(0.0, |value| value.approach_rate)),
        od: fix_decimals(original.map_or(0.0, |value| value.overall_difficulty)),
        m_od: fix_decimals(converted.map_or(0.0, |value| value.overall_difficulty)),
        cs: fix_decimals(original.map_or(0.0, |value| value.circle_size)),
        m_cs: fix_decimals(converted.map_or(0.0, |value| value.circle_size)),
        hp: fix_decimals(original.map_or(0.0, |value| value.drain_rate)),
        m_hp: fix_decimals(converted.map_or(0.0, |value| value.drain_rate)),
        current_bpm: round4(beatmap.event_status.bpm),
        bpm: common_bpm,
        main_bpm: common_bpm,
        max_bpm,
        min_bpm,
        m_main_bpm: common_bpm,
        m_max_bpm: max_bpm,
        m_min_bpm: min_bpm,
        m_bpm: bpm_text(bpm.minimum, bpm.maximum, bpm.common),
        md5: menu.checksum.clone(),
        game_mode: ruleset_name(current_mode),
        mode: current_mode,
        time: play_time / 1_000.0,
        previewtime: i64::from(times.preview),
        totaltime: full,
        time_left,
        drainingtime,
        total_audio_time: menu.mp3_length,
        first_hit_object_time: first_object,
        mapid: menu.map_id,
        mapsetid: menu.set_id,
        map_strains: map_strains(
            &beatmap.graph.x_axis,
            beatmap.graph.series.first().map(|s| s.data.as_slice()),
        ),
        map_breaks: parsed.map_or_else(Vec::new, |map| {
            map.breaks
                .iter()
                .map(|range| MapBreak {
                    start_time: range.start,
                    end_time: range.end,
                    has_effect: range.has_effect,
                })
                .collect()
        }),
        map_kiai_points: Vec::new(),
        map_position: format_milliseconds(play_time),
        map_timing_points: parsed.map_or_else(Vec::new, |map| {
            map.timing_points
                .iter()
                .map(|point| MapTimingPoint {
                    start_time: if point.start == 0.0 || point.start.is_nan() {
                        0.0
                    } else {
                        point.start
                    },
                    bpm: None,
                    beat_length: point.beat_length,
                })
                .collect()
        }),
        sliders: i64::from(objects.sliders),
        circles: i64::from(objects.circles),
        spinners: i64::from(objects.spinners),
        max_combo: parsed.map_or(0, |map| i64::from(map.engine.difficulty.max_combo)),
        mp3_name: menu.audio_filename.clone(),
        osu_file_name: menu.filename.clone(),
        background_image_file_name: menu.background_filename.clone(),
        osu_file_location: path_join(menu.folder.as_deref(), menu.filename.as_deref()),
        background_image_location: path_join(
            menu.folder.as_deref(),
            menu.background_filename.as_deref(),
        ),
        retries: gameplay.retries,
        username: gameplay.player_name.clone(),
        score: gameplay.score,
        player_hp: gameplay.player_hp,
        player_hp_smooth: gameplay.player_hp_smooth,
        combo: gameplay.combo,
        current_max_combo: gameplay.max_combo,
        key_overlay,
        geki: hits.geki,
        c300: hits.n300,
        katsu: hits.katu,
        c100: hits.n100,
        c50: hits.n50,
        miss: hits.n0,
        slider_breaks: gameplay.slider_breaks,
        acc: fix_decimals(gameplay.accuracy),
        unstable_rate: fix_decimals(precise.unstable_rate * current_mods.rate),
        converted_unstable_rate: fix_decimals(precise.unstable_rate),
        grade: grade_number(&gameplay.grade_current),
        max_grade: grade_number(&gameplay.grade_expected),
        hit_errors: precise.hit_errors.all.clone(),
        press_hit_errors: precise.hit_errors.press.clone(),
        release_hit_errors: precise.hit_errors.release.clone(),
        mods: current_mods.name.clone(),
        mods_enum: current_mods.number,
        pp_if_map_ends_now: fix_decimals(attributes.pp),
        pp_if_rest_fced: fix_decimals(attributes.fc_pp),
        leader_board_main_player,
        leader_board_players,
        ranked_status: menu.ranked_status,
        song_selection_ranking_type: regular
            .settings
            .values
            .get("leaderboardType")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
        raw_status,
        dir: menu.folder.clone(),
        dl: format!(
            "https://osu.ppy.sh/b/{}",
            menu.map_id
                .map_or_else(|| "undefined".to_owned(), |id| id.to_string())
        ),
        osu_90_pp: fix_decimals(pp(90)),
        osu_95_pp: fix_decimals(pp(95)),
        osu_96_pp: fix_decimals(pp(96)),
        osu_97_pp: fix_decimals(pp(97)),
        osu_98_pp: fix_decimals(pp(98)),
        osu_99_pp: fix_decimals(pp(99)),
        osu_sspp: fix_decimals(pp(100)),
        osu_m90_pp: fix_decimals(pp(90)),
        osu_m95_pp: fix_decimals(pp(95)),
        osu_m96_pp: fix_decimals(pp(96)),
        osu_m97_pp: fix_decimals(pp(97)),
        osu_m98_pp: fix_decimals(pp(98)),
        osu_m99_pp: fix_decimals(pp(99)),
        osu_m_sspp: fix_decimals(pp(100)),
        acc_pp_if_map_ends_now: fix_decimals(attributes.current.accuracy),
        aim_pp_if_map_ends_now: fix_decimals(attributes.current.aim),
        speed_pp_if_map_ends_now: fix_decimals(attributes.current.speed),
        strain_pp_if_map_ends_now: fix_decimals(attributes.current.difficulty),
        no_choke_pp: fix_decimals(attributes.fc_pp),
        skin: global.skin_folder.clone(),
        skin_path: global.skin_folder.clone(),
        song_selection_scores: "[]".to_owned(),
        song_selection_main_player_score: "{}".to_owned(),
        song_selection_total_scores: -1,
        status,
        osu_99_9_pp: -1,
        osu_m99_9_pp: -1,
        mania_1_000_000_pp: -1,
        mania_m1_000_000_pp: -1,
        simulated_pp: fix_decimals(pp(100)),
        plays: 0,
        tags: String::new(),
        source: String::new(),
        stars_nomod: -1,
        combo_left: -1,
        local_time: 0,
        local_time_iso: "0".to_owned(),
        sl: -1,
        sv: -1,
        threadid: 0,
        test: String::new(),
    })
}

/// Encodes this generation once and shares its immutable bytes.
pub fn encode(snapshot: &CombinedSnapshot) -> Result<Bytes, ApiError> {
    snapshot.encode_unfiltered(ApiSchema::StreamCompanion, |snapshot| {
        let direct = wire::encode(snapshot);
        #[cfg(debug_assertions)]
        {
            let dto = build(snapshot).and_then(|response| encode_json_bytes(&response));
            assert_eq!(
                direct.as_deref().map(String::from_utf8_lossy),
                dto.as_deref().map(String::from_utf8_lossy),
                "direct streamCompanion encoding diverged from the DTO contract",
            );
        }
        direct.map(Bytes::from)
    })
}

mod helpers;
mod wire;

use helpers::*;

#[cfg(test)]
mod tests;

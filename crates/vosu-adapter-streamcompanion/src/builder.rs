//! Source-compatible streamCompanion builder over one coherent bus selection.
use crate::helpers::*;
use crate::{Error as ApiError, chart::ChartFacts, dto::Response, view::View};
use bytes::Bytes;
use vosu_adapter_support::fixed::fix_decimals;
use vosu_osu_model::enums::{CountryCodes, Rulesets};
use vosu_osu_model::to_legacy_hits;

/// Chart facts are supplied only by C1's resource-derived projection.
pub fn build(view: &View, parsed: Option<&ChartFacts>) -> Result<Response, ApiError> {
    let raw_status = view.raw_status;
    let status = stream_companion_status(raw_status, view.watching_replay);
    let current_mods = &view.play.mods;
    let current_mode = view.mode();
    let hits = to_legacy_hits(
        view.play.mode.number.unwrap_or_default() as i32,
        &view.statistics(),
    );

    let user = view.profile.id.map(|_| &view.profile);
    let raw_login_status = view.profile.user_status.number;
    let artist = js_undefined(view.metadata.artist.as_deref());
    let artist_original = js_undefined(view.metadata.artist_unicode.as_deref());
    let title = js_undefined(view.metadata.title.as_deref());
    let title_original = js_undefined(view.metadata.title_unicode.as_deref());
    let difficulty = js_undefined(view.metadata.version.as_deref());

    let original = parsed.map(|map| &map.original);
    let converted = parsed.map(|map| &map.converted);
    let map_difficulty = parsed.map(|map| &map.difficulty);
    let bpm = parsed.map(|map| map.bpm).unwrap_or_default();
    let times = parsed.map(|map| map.times).unwrap_or_default();
    let objects = parsed.map(|map| map.objects).unwrap_or_default();
    let pp = |accuracy: u8| view.pp_accuracy.get(&accuracy).copied().unwrap_or_default();

    let common_bpm = round4(bpm.common);
    let max_bpm = round4(bpm.maximum);
    let min_bpm = round4(bpm.minimum);
    let first_object = i64::from(times.first_object);
    let play_time = view.play_time;
    let (full, time_left, drainingtime) = map_time_projection(times.full, first_object, play_time);
    let key_overlay = encode_json(&KeyOverlayWire::new(&view.overlay))?;
    let leader_board_main_player = encode_json(&LeaderboardMainWire::from_state(
        view.leaderboard_state.visible,
        &view.leaderboard_state.player,
    ))?;
    let leader_board_players = encode_json(
        &view
            .leaderboard
            .iter()
            .map(LeaderboardPlayerWire::from_player)
            .collect::<Vec<_>>(),
    )?;

    Ok(Response {
        osu_is_running: 1,
        chat_is_enabled: i64::from(view.chat_status > 0),
        ingame_interface_is_enabled: i64::from(view.interface_visible),
        is_break_time: 0,
        bancho_is_connected: i64::from(raw_login_status == Some(65_793)),
        bancho_id: user.and_then(|user| user.id),
        bancho_username: user.and_then(|user| user.name.clone()),
        bancho_status: raw_login_status,
        bancho_country: user.map_or_else(String::new, |user| {
            country_name(user.country_code.number.unwrap_or_default())
        }),
        artist_roman: view.metadata.artist.clone(),
        artist_unicode: view.metadata.artist_unicode.clone(),
        title_roman: view.metadata.title.clone(),
        title_unicode: view.metadata.title_unicode.clone(),
        map_artist_title: format!("{artist} - {title}"),
        map_artist_title_unicode: format!("{artist_original} - {title_original}"),
        diff_name: view.metadata.version.clone(),
        map_diff: format!("[{difficulty}]"),
        creator: view.metadata.mapper.clone(),
        live_star_rating: fix_decimals(view.live_stars),
        m_stars: fix_decimals(map_difficulty.map_or(0.0, |value| value.stars)),
        ar: fix_decimals(original.map_or(0.0, |value| value.approach_rate)),
        m_ar: fix_decimals(converted.map_or(0.0, |value| value.approach_rate)),
        od: fix_decimals(original.map_or(0.0, |value| value.overall_difficulty)),
        m_od: fix_decimals(converted.map_or(0.0, |value| value.overall_difficulty)),
        cs: fix_decimals(original.map_or(0.0, |value| value.circle_size)),
        m_cs: fix_decimals(converted.map_or(0.0, |value| value.circle_size)),
        hp: fix_decimals(original.map_or(0.0, |value| value.drain_rate)),
        m_hp: fix_decimals(converted.map_or(0.0, |value| value.drain_rate)),
        current_bpm: round4(view.current_bpm),
        bpm: common_bpm,
        main_bpm: common_bpm,
        max_bpm,
        min_bpm,
        m_main_bpm: common_bpm,
        m_max_bpm: max_bpm,
        m_min_bpm: min_bpm,
        m_bpm: bpm_text(bpm.minimum, bpm.maximum, bpm.common),
        md5: view.metadata.checksum.clone(),
        game_mode: ruleset_name(current_mode),
        mode: current_mode,
        time: play_time / 1_000.0,
        previewtime: i64::from(times.preview),
        totaltime: full,
        time_left,
        drainingtime,
        total_audio_time: view.metadata.time.mp3_length,
        first_hit_object_time: first_object,
        mapid: view.metadata.id,
        mapsetid: view.metadata.set,
        map_strains: parsed.map_or_else(Default::default, |map| map.strains.clone()),
        map_breaks: parsed.map_or_else(Vec::new, |map| map.breaks.clone()),
        map_kiai_points: Vec::new(),
        map_position: format_milliseconds(play_time),
        map_timing_points: parsed.map_or_else(Vec::new, |map| {
            map.timing_points
                .iter()
                .map(|point| crate::dto::MapTimingPoint {
                    start_time: if point.start_time == 0.0 || point.start_time.is_nan() {
                        0.0
                    } else {
                        point.start_time
                    },
                    bpm: None,
                    beat_length: point.beat_length,
                })
                .collect()
        }),
        sliders: i64::from(objects.sliders),
        circles: i64::from(objects.circles),
        spinners: i64::from(objects.spinners),
        max_combo: parsed.map_or(0, |map| i64::from(map.difficulty.max_combo)),
        mp3_name: view.paths.current.as_ref().and_then(|p| p.audio.clone()),
        osu_file_name: view
            .paths
            .current
            .as_ref()
            .and_then(|p| p.beatmap_file.clone()),
        background_image_file_name: view
            .paths
            .current
            .as_ref()
            .and_then(|p| p.background.clone()),
        osu_file_location: path_join(
            view.paths
                .current
                .as_ref()
                .and_then(|p| p.beatmap_folder.as_deref()),
            view.paths
                .current
                .as_ref()
                .and_then(|p| p.beatmap_file.as_deref()),
        ),
        background_image_location: path_join(
            view.paths
                .current
                .as_ref()
                .and_then(|p| p.beatmap_folder.as_deref()),
            view.paths
                .current
                .as_ref()
                .and_then(|p| p.background.as_deref()),
        ),
        retries: view.play.retries,
        username: view.play.player_name.clone(),
        score: view.play.score,
        player_hp: view.play.health_raw.normal,
        player_hp_smooth: view.play.health_raw.smooth,
        combo: view.play.combo.current,
        current_max_combo: view.play.combo.max,
        key_overlay,
        geki: hits.geki,
        c300: hits.n300,
        katsu: hits.katu,
        c100: hits.n100,
        c50: hits.n50,
        miss: hits.n0,
        slider_breaks: view.play.statistics.slider_breaks,
        acc: fix_decimals(view.play.accuracy_percent),
        unstable_rate: fix_decimals(view.play.precise_unstable_rate * current_mods.rate),
        converted_unstable_rate: fix_decimals(view.play.precise_unstable_rate),
        grade: grade_number(&view.play.rank.current),
        max_grade: grade_number(&view.play.rank.max_this_play),
        hit_errors: view.hit_errors.to_vec(),
        mods: current_mods.name.clone(),
        mods_enum: current_mods.number,
        pp_if_map_ends_now: fix_decimals(view.pp.current),
        pp_if_rest_fced: fix_decimals(view.pp.fc),
        leader_board_main_player,
        leader_board_players,
        ranked_status: view.metadata.status.number,
        song_selection_ranking_type: view.ranking_type,
        raw_status,
        dir: view
            .paths
            .current
            .as_ref()
            .and_then(|p| p.beatmap_folder.clone()),
        dl: format!(
            "https://osu.ppy.sh/b/{}",
            view.metadata
                .id
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
        acc_pp_if_map_ends_now: fix_decimals(view.pp.detailed.current.accuracy),
        aim_pp_if_map_ends_now: fix_decimals(view.pp.detailed.current.aim),
        speed_pp_if_map_ends_now: fix_decimals(view.pp.detailed.current.speed),
        strain_pp_if_map_ends_now: fix_decimals(view.pp.detailed.current.difficulty),
        no_choke_pp: fix_decimals(view.pp.fc),
        skin: view.paths.skin_folder.clone().unwrap_or_default(),
        skin_path: view.paths.skin_folder.clone().unwrap_or_default(),
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

pub fn encode(view: &View, chart: Option<&ChartFacts>) -> Result<Bytes, ApiError> {
    wire::encode(view, chart).map(Bytes::from)
}
mod wire;

use super::*;

pub(super) fn select_mods(state: &RegularState, status: i64) -> &CalculateMods {
    match status {
        2 => &state.gameplay.mods,
        7 => &state.result_screen.raw.mods,
        _ => &state.global.menu_mods,
    }
}
pub(super) fn select_mode(state: &RegularState, status: i64) -> Option<i64> {
    match status {
        2 => Some(state.gameplay.mode),
        7 => Some(state.result_screen.raw.mode),
        _ => state.menu.gamemode,
    }
}
pub(super) fn number_name(number: Option<i64>, name: &str) -> NumberName {
    NumberName {
        number,
        name: name.to_owned(),
    }
}
pub(super) fn client_name(source: ClientType) -> &'static str {
    match source {
        ClientType::Stable => "stable",
        ClientType::Lazer => "lazer",
    }
}

pub(super) fn direct_join(folder: Option<&str>, file: Option<&str>) -> String {
    let folder = folder.unwrap_or("");
    let file = file.unwrap_or("");
    let mut joined = PathBuf::from(folder);
    if !file.is_empty() {
        if !folder.is_empty() {
            let mut combined = std::ffi::OsString::from(folder);
            combined.push(std::path::MAIN_SEPARATOR_STR);
            combined.push(file);
            joined = PathBuf::from(combined);
        } else {
            joined = PathBuf::from(file);
        }
    }
    let mut normalized = PathBuf::new();

    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));
                if can_pop {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        normalized.to_string_lossy().into_owned()
    }
}

pub(super) fn get<'a>(settings: &'a SettingsObject, path: &[&str]) -> Option<&'a Value> {
    let mut value = settings.get(*path.first()?)?;
    for key in &path[1..] {
        value = value.get(*key)?;
    }
    Some(value)
}
pub(super) fn bool_value(s: &SettingsObject, p: &[&str]) -> bool {
    get(s, p).and_then(Value::as_bool).unwrap_or(false)
}
pub(super) fn i64_value(s: &SettingsObject, p: &[&str]) -> i64 {
    get(s, p)
        .and_then(Value::as_i64)
        .or_else(|| get(s, p).and_then(Value::as_f64).map(|v| v as i64))
        .unwrap_or(0)
}
pub(super) fn f64_value(s: &SettingsObject, p: &[&str]) -> f64 {
    match get(s, p) {
        Some(Value::Null) => f64::NAN,
        Some(value) => value.as_f64().unwrap_or(0.0),
        None => 0.0,
    }
}
pub(super) fn string_value(s: &SettingsObject, p: &[&str]) -> String {
    get(s, p).and_then(Value::as_str).unwrap_or("").to_owned()
}

pub(super) fn build_settings(state: &RegularState, source: ClientType) -> Settings {
    let s = &state.settings.values;
    let leaderboard_type = i64_value(s, &["leaderboardType"]);
    let score_meter_type = i64_value(s, &["scoreMeter", "type"]);
    let scroll_direction = i64_value(s, &["mania", "scrollDirection"]);
    let sort = i64_value(s, &["sortType"]);
    let group = i64_value(s, &["groupType"]);
    Settings {
        interface_visible: state.global.show_interface,
        replay_ui_visible: !state.global.is_replay_ui_hidden,
        chat_visibility_status: number_name(
            Some(state.global.chat_status),
            chat_status_name(state.global.chat_status),
        ),
        leaderboard: LeaderboardSettings {
            visible: state.gameplay.leaderboard_visible,
            kind: number_name(
                Some(leaderboard_type),
                if source == ClientType::Lazer {
                    lazer_leaderboard_name(leaderboard_type)
                } else {
                    leaderboard_name(leaderboard_type)
                },
            ),
        },
        progress_bar: {
            let n = i64_value(s, &["progressBarType"]);
            number_name(Some(n), progress_bar_name(n))
        },
        bass_density: state.bass_density.density,
        resolution: Resolution {
            fullscreen: bool_value(s, &["resolution", "fullscreen"]),
            width: i64_value(s, &["resolution", "width"]),
            height: i64_value(s, &["resolution", "height"]),
            width_fullscreen: i64_value(s, &["resolution", "widthFullscreen"]),
            height_fullscreen: i64_value(s, &["resolution", "heightFullscreen"]),
        },
        client: Client {
            update_available: bool_value(s, &["client", "updateAvailable"]),
            branch: i64_value(s, &["client", "branch"]),
            version: string_value(s, &["client", "version"]),
        },
        score_meter: ScoreMeter {
            kind: number_name(Some(score_meter_type), score_meter_name(score_meter_type)),
            size: f64_value(s, &["scoreMeter", "size"]),
        },
        cursor: Cursor {
            use_skin_cursor: bool_value(s, &["cursor", "useSkinCursor"]),
            auto_size: bool_value(s, &["cursor", "autoSize"]),
            size: f64_value(s, &["cursor", "size"]),
            menu_size: f64_value(s, &["cursor", "menuSize"]),
        },
        mouse: Mouse {
            raw_input: bool_value(s, &["mouse", "rawInput"]),
            disable_buttons: bool_value(s, &["mouse", "disableButtons"]),
            disable_wheel: bool_value(s, &["mouse", "disableWheel"]),
            sensitivity: f64_value(s, &["mouse", "sensitivity"]),
        },
        tablet: Tablet {
            enabled: bool_value(s, &["tablet", "enabled"]),
            x: f64_value(s, &["tablet", "x"]),
            y: f64_value(s, &["tablet", "y"]),
            width: f64_value(s, &["tablet", "width"]),
            height: f64_value(s, &["tablet", "height"]),
            rotation: f64_value(s, &["tablet", "rotation"]),
            pressure_threshold: f64_value(s, &["tablet", "pressureThreshold"]),
        },
        mania: Mania {
            speed_bpm_scale: bool_value(s, &["mania", "speedBPMScale"]),
            use_per_beatmap_speed_scale: bool_value(s, &["mania", "usePerBeatmapSpeedScale"]),
            scroll_speed: i64_value(s, &["mania", "scrollSpeed"]),
            scroll_direction: ScrollDirection {
                number: scroll_direction,
                name: (!mania_scroll_name(scroll_direction).is_empty())
                    .then(|| mania_scroll_name(scroll_direction).to_owned()),
            },
        },
        sort: number_name(
            Some(sort),
            if source == ClientType::Lazer {
                lazer_sort_name(sort)
            } else {
                sort_name(sort)
            },
        ),
        group: number_name(
            Some(group),
            if source == ClientType::Lazer {
                lazer_group_name(group)
            } else {
                group_name(group)
            },
        ),
        skin: Skin {
            use_default_skin_in_editor: bool_value(s, &["skin", "useDefaultSkinInEditor"]),
            ignore_beatmap_skins: bool_value(s, &["skin", "ignoreBeatmapSkins"]),
            tint_slider_ball: bool_value(s, &["skin", "tintSliderBall"]),
            use_taiko_skin: bool_value(s, &["skin", "useTaikoSkin"]),
            name: string_value(s, &["skin", "name"]),
        },
        mode: number_name(
            state.menu.gamemode,
            state.menu.gamemode.map_or("", ruleset_name),
        ),
        audio: Audio {
            ignore_beatmap_sounds: bool_value(s, &["audio", "ignoreBeatmapSounds"]),
            use_skin_samples: bool_value(s, &["audio", "useSkinSamples"]),
            volume: Volume {
                master_inactive: f64_value(s, &["audio", "volume", "masterInactive"]),
                master: f64_value(s, &["audio", "volume", "master"]),
                music: f64_value(s, &["audio", "volume", "music"]),
                effect: f64_value(s, &["audio", "volume", "effect"]),
            },
            offset: Offset {
                universal: f64_value(s, &["audio", "offset", "universal"]),
            },
        },
        background: Background {
            storyboard: bool_value(s, &["background", "storyboard"]),
            video: bool_value(s, &["background", "video"]),
            blur: f64_value(s, &["background", "blur"]),
            dim: f64_value(s, &["background", "dim"]),
        },
        keybinds: Keybinds {
            osu: KeybindsOsu {
                k1: string_value(s, &["keybinds", "osu", "k1"]),
                k2: string_value(s, &["keybinds", "osu", "k2"]),
                smoke_key: string_value(s, &["keybinds", "osu", "smokeKey"]),
            },
            fruits: KeybindsFruits {
                k1: string_value(s, &["keybinds", "fruits", "k1"]),
                k2: string_value(s, &["keybinds", "fruits", "k2"]),
                dash: string_value(s, &["keybinds", "fruits", "Dash"]),
            },
            taiko: KeybindsTaiko {
                inner_left: string_value(s, &["keybinds", "taiko", "innerLeft"]),
                inner_right: string_value(s, &["keybinds", "taiko", "innerRight"]),
                outer_left: string_value(s, &["keybinds", "taiko", "outerLeft"]),
                outer_right: string_value(s, &["keybinds", "taiko", "outerRight"]),
            },
            quick_retry: string_value(s, &["keybinds", "quickRetry"]),
        },
    }
}

pub(super) fn build_profile(user: Option<&User>) -> Profile {
    let Some(user) = user else {
        return Profile {
            user_status: Some(number_name(None, "")),
            bancho_status: Some(number_name(None, "")),
            id: None,
            name: None,
            mode: Some(number_name(None, "")),
            ranked_score: None,
            level: None,
            accuracy: None,
            pp: None,
            play_count: None,
            global_rank: None,
            country_code: Some(number_name(None, "")),
            background_colour: None,
            matchmaking: None,
        };
    };
    Profile {
        user_status: Some(number_name(
            Some(user.raw_login_status),
            user_login_name(user.raw_login_status),
        )),
        bancho_status: Some(number_name(
            Some(user.raw_bancho_status),
            bancho_name(user.raw_bancho_status),
        )),
        id: Some(user.id),
        name: Some(user.name.clone()),
        mode: Some(number_name(
            Some(user.play_mode),
            ruleset_name(user.play_mode),
        )),
        ranked_score: Some(user.ranked_score),
        level: Some(user.level),
        accuracy: Some(user.accuracy),
        pp: Some(user.performance_points),
        play_count: Some(user.play_count),
        global_rank: Some(user.rank),
        country_code: Some(number_name(
            Some(user.country_code),
            source_country_name(user.country_code),
        )),
        background_colour: Some(if user.background_colour < 0 {
            format!("-{:x}", user.background_colour.unsigned_abs())
        } else {
            format!("{:x}", user.background_colour)
        }),
        matchmaking: Some(
            user.matchmaking
                .as_ref()
                .map(|matchmaking| MatchmakingStats {
                    rating: matchmaking.rating,
                    rank: matchmaking.rank,
                    plays: matchmaking.plays,
                    wins: matchmaking.wins,
                    is_provisional: matchmaking.is_provisional,
                }),
        ),
    }
}

pub(super) fn build_beatmap(
    state: &RegularState,
    parsed: Option<&ParsedBeatmap>,
    source: ClientType,
    current_mode: Option<i64>,
    live_time: f64,
) -> Beatmap {
    let mode = Some(state.beatmap.mode.unwrap_or(0));
    let ranked_status = state.menu.ranked_status;
    Beatmap {
        is_kiai: state.beatmap.event_status.kiai,
        is_break: state.beatmap.event_status.break_time,
        is_convert: mode == Some(0) && mode != current_mode,
        time: BeatmapTime {
            live: live_time,
            first_object: state.beatmap.first_object_time.unwrap_or(0),
            last_object: parsed.map_or(0.0, |p| p.times.full),
            mp3_length: state.menu.mp3_length,
        },
        status: BeatmapStatus {
            number: ranked_status,
            name: ranked_status
                .filter(|value| value.is_finite() && *value != 0.0)
                .map_or_else(String::new, |value| {
                    beatmap_status_name(value as i64).to_owned()
                }),
        },
        checksum: state.menu.checksum.clone(),
        id: state.menu.map_id,
        set: state.menu.set_id,
        mode: number_name(mode, mode.map_or("", ruleset_name)),
        artist: state.menu.artist.clone(),
        artist_unicode: state.menu.artist_original.clone(),
        title: state.menu.title.clone(),
        title_unicode: state.menu.title_original.clone(),
        mapper: state.menu.creator.clone(),
        version: state.menu.difficulty.clone(),
        source: beatmap_source(source, parsed.map(|parsed| parsed.metadata.source.as_str())),
        tags: parsed.map_or("", |p| p.metadata.tags.as_str()).to_owned(),
        stats: build_stats(state, parsed),
    }
}

pub(super) fn beatmap_source(source: ClientType, parsed_source: Option<&str>) -> String {
    if source == ClientType::Lazer {
        String::new()
    } else {
        parsed_source.unwrap_or_default().to_owned()
    }
}

pub(super) fn truthy(value: Option<f64>) -> Option<f64> {
    value.filter(|v| *v != 0.0 && !v.is_nan()).map(fix_decimals)
}
pub(super) fn build_stats(state: &RegularState, parsed: Option<&ParsedBeatmap>) -> Stats {
    let attrs = parsed.map(|p| &p.engine.difficulty);
    let original = parsed.map(|p| &p.engine.original_difficulty);
    let converted = parsed.map(|p| &p.engine.converted_difficulty);
    let counts = parsed.map(|p| p.object_counts);
    let circles = counts.map_or(0, |v| i64::from(v.circles));
    let sliders = counts.map_or(0, |v| i64::from(v.sliders));
    let spinners = counts.map_or(0, |v| i64::from(v.spinners));
    let holds = counts.map_or(0, |v| i64::from(v.holds));
    Stats {
        stars: Stars {
            live: fix_decimals(state.beatmap.attributes.map_or(0.0, |v| v.stars)),
            aim: truthy(attrs.and_then(|v| v.aim)),
            speed: truthy(attrs.and_then(|v| v.speed)),
            flashlight: truthy(attrs.and_then(|v| v.flashlight)),
            slider_factor: truthy(attrs.and_then(|v| v.slider_factor)),
            stamina: truthy(attrs.and_then(|v| v.stamina)),
            rhythm: truthy(attrs.and_then(|v| v.rhythm)),
            color: truthy(attrs.and_then(|v| v.color)),
            reading: truthy(attrs.and_then(|v| v.reading)),
            hit_window: parsed
                .and_then(|p| p.engine.hit_windows.great)
                .map(fix_decimals),
            total: fix_decimals(attrs.map_or(0.0, |v| v.stars)),
        },
        ar: Ar {
            original: fix_decimals(original.map_or(0.0, |v| v.approach_rate)),
            converted: fix_decimals(converted.map_or(0.0, |v| v.approach_rate)),
        },
        cs: Cs {
            original: fix_decimals(original.map_or(0.0, |v| v.circle_size)),
            converted: fix_decimals(converted.map_or(0.0, |v| v.circle_size)),
        },
        od: Od {
            original: fix_decimals(original.map_or(0.0, |v| v.overall_difficulty)),
            converted: fix_decimals(converted.map_or(0.0, |v| v.overall_difficulty)),
        },
        hp: Hp {
            original: fix_decimals(original.map_or(0.0, |v| v.drain_rate)),
            converted: fix_decimals(converted.map_or(0.0, |v| v.drain_rate)),
        },
        timing_windows: timing_windows(parsed),
        bpm: Bpm {
            realtime: fix_decimals_to(state.beatmap.event_status.bpm, 4)
                .unwrap_or(state.beatmap.event_status.bpm),
            common: fix_decimals_to(parsed.map_or(0.0, |p| p.bpm.common), 4).unwrap_or(0.0),
            min: fix_decimals_to(parsed.map_or(0.0, |p| p.bpm.minimum), 4).unwrap_or(0.0),
            max: fix_decimals_to(parsed.map_or(0.0, |p| p.bpm.maximum), 4).unwrap_or(0.0),
        },
        objects: Objects {
            circles,
            sliders,
            spinners,
            holds,
            total: circles + sliders + spinners + holds,
        },
        max_combo: parsed.map_or(0, |v| i64::from(v.engine.maximum_score.max_combo)),
    }
}

pub(super) fn timing_windows(parsed: Option<&ParsedBeatmap>) -> TimingWindows {
    let Some(parsed) = parsed else {
        return TimingWindows::default();
    };
    let windows = &parsed.engine.hit_windows;
    let h300 = parsed
        .engine
        .catch_spatial_window
        .map(|window| window.half_width)
        .or(windows.great);

    TimingWindows {
        h0: windows.miss.map(fix_decimals),
        h50: windows.meh.map(fix_decimals),
        h100: windows.ok.map(fix_decimals),
        h300: h300.map(fix_decimals),
        geki: windows.perfect.map(fix_decimals),
        katu: windows.good.map(fix_decimals),
    }
}

pub(super) fn detailed(
    a: crate::calculation::progressive::PerformanceAttributes,
    total: f64,
) -> DetailedPpAttributes {
    DetailedPpAttributes {
        aim: fix_decimals(a.aim),
        speed: fix_decimals(a.speed),
        accuracy: fix_decimals(a.accuracy),
        difficulty: fix_decimals(a.difficulty),
        flashlight: fix_decimals(a.flashlight),
        total: fix_decimals(total),
    }
}
pub(super) fn build_play(
    gameplay: &GameplayState,
    attrs: Option<&crate::calculation::progressive::ProgressiveAttributes>,
    mods: &CalculateMods,
    precise_hits: &[f64],
) -> Play {
    let a = attrs.copied().unwrap_or_default();
    let s = &gameplay.statistics;
    Play {
        failed: gameplay.failed,
        player_name: gameplay.player_name.clone(),
        mode: number_name(Some(gameplay.mode), ruleset_name(gameplay.mode)),
        score: gameplay.score,
        accuracy: gameplay.accuracy,
        health_bar: HealthBar {
            normal: gameplay.player_hp / 2.0,
            smooth: gameplay.player_hp_smooth / 2.0,
        },
        hits: PlayHits::Normal(Hits {
            h300: s.great,
            geki: s.perfect,
            h100: s.ok,
            katu: s.good,
            h50: s.meh,
            h0: s.miss,
            slider_breaks: gameplay.slider_breaks,
            slider_end_hits: s.slider_tail_hit,
            small_tick_hits: s.small_tick_hit,
            large_tick_hits: s.large_tick_hit,
        }),
        hit_error_array: precise_hits.to_vec(),
        combo: Combo {
            current: gameplay.combo,
            max: gameplay.max_combo,
        },
        mods: mods.clone(),
        rank: Rank {
            current: gameplay.grade_current.clone(),
            max_this_play: gameplay.grade_expected.clone(),
        },
        pp: Pp {
            current: fix_decimals(a.pp),
            fc: fix_decimals(a.fc_pp),
            max_achieved: fix_decimals(a.max_achieved),
            max_achievable: fix_decimals(a.max_achievable),
            detailed: DetailedPp {
                current: detailed(a.current, a.pp),
                fc: detailed(a.fc, a.fc_pp),
            },
        },
        unstable_rate: gameplay.unstable_rate,
    }
}

pub(super) fn build_leaderboard(
    p: &LeaderboardPlayer,
    _source: ClientType,
    _mode: i64,
) -> Leaderboard {
    let s = &p.statistics;
    Leaderboard {
        is_failed: !p.is_passing,
        position: p.position,
        team: p.team,
        id: p.user_id,
        name: p.name.clone(),
        score: p.score,
        accuracy: p.accuracy,
        hits: LeaderboardHits {
            h300: s.great,
            geki: s.perfect,
            h100: s.ok,
            katu: s.good,
            h50: s.meh,
            h0: s.miss,
        },
        combo: Combo {
            current: p.combo,
            max: p.max_combo,
        },
        mods: p.mods.clone(),
        rank: String::new(),
    }
}
pub(super) fn build_performance(state: &RegularState) -> Performance {
    let value = |n: u8| fix_decimals(state.beatmap.pp_acc.get(&n).copied().unwrap_or(0.0));
    Performance {
        accuracy: Accuracy {
            acc_90: value(90),
            acc_91: value(91),
            acc_92: value(92),
            acc_93: value(93),
            acc_94: value(94),
            acc_95: value(95),
            acc_96: value(96),
            acc_97: value(97),
            acc_98: value(98),
            acc_99: value(99),
            acc_100: value(100),
        },
        graph: Graph {
            series: state
                .beatmap
                .graph
                .series
                .iter()
                .map(|s| Series {
                    name: s.name.clone(),
                    data: s.data.clone(),
                })
                .collect(),
            xaxis: state.beatmap.graph.x_axis.clone(),
        },
    }
}
pub(super) fn build_results(state: &RegularState) -> ResultsScreen {
    let r = &state.result_screen;
    let s = &r.raw.statistics;
    ResultsScreen {
        score_id: r.raw.online_id,
        player_name: r.raw.player_name.clone(),
        mode: number_name(Some(r.raw.mode), ruleset_name(r.raw.mode)),
        score: r.raw.score,
        accuracy: r.raw.accuracy,
        name: r.raw.player_name.clone(),
        hits: ResultsHits {
            h300: s.great,
            geki: s.perfect,
            h100: s.ok,
            katu: s.good,
            h50: s.meh,
            h0: s.miss,
            slider_end_hits: s.slider_tail_hit,
            small_tick_hits: s.small_tick_hit,
            large_tick_hits: s.large_tick_hit,
        },
        mods: r.raw.mods.clone(),
        max_combo: r.raw.max_combo,
        rank: r.grade.clone(),
        pp: ResultsPp {
            current: r.pp,
            fc: r.fc_pp,
        },
        created_at: r.raw.date.clone(),
    }
}

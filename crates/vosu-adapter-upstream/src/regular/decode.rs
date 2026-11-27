//! Zero-Serde single-pass decoder from `CapturedSelection` into owned `dto::Response`.
use super::{
    decode_extra::*,
    parse::*,
    parse_settings::*,
    slots::{RegularSlots, Slot},
};
use crate::{
    Error,
    dto::*,
    source::{HitErrors, SourcePlay},
    view::{
        Identity, as_bool, as_f64, as_i64, as_map, as_opt_f64, as_opt_i64, as_opt_str, as_str,
        missing,
    },
};
use ciborium::Value;
use vosu_osu_model::{
    source::TourneyAvailability,
    states::{CalculateMods, Mod},
};
use vosu_surface::state::CapturedSelection;

pub(crate) fn decode_response(capture: &CapturedSelection) -> Result<(Identity, Response), Error> {
    let identity = Identity::from_capture(capture)?;
    let slots = RegularSlots::index(capture);
    let response = decode_with_slots(capture, &identity, &slots)?;
    Ok((identity, response))
}

pub(super) fn decode_response_only(capture: &CapturedSelection) -> Result<Response, Error> {
    let identity = Identity::from_capture(capture)?;
    let slots = RegularSlots::index(capture);
    decode_with_slots(capture, &identity, &slots)
}

fn decode_with_slots(
    capture: &CapturedSelection,
    identity: &Identity,
    slots: &RegularSlots<'_>,
) -> Result<Response, Error> {
    let lazer = identity.session.variant == "lazer";
    let availability = parse_tourney_availability(
        slots.get(Slot::TourneyAvailability)?,
        "/tourneyAvailability",
    )?;
    let tourney = match availability {
        TourneyAvailability::Pending => None,
        TourneyAvailability::MissingScore => return Err(Error::MissingTourneyScore),
        TourneyAvailability::Ready => {
            let val = slots.get(Slot::Tourney)?;
            if matches!(val, Value::Null) {
                None
            } else {
                Some(decode_tourney(val, lazer, "/tourney")?)
            }
        }
    };
    let skin_name = as_opt_str(slots.get(Slot::SkinName)?, "vosu.skin/name")?.unwrap_or("");
    let settings = decode_settings(slots.get(Slot::Settings)?, skin_name, "/settings")?;
    let profile = decode_profile(slots.get(Slot::Profile)?, "/profile")?;
    let raw = parse_health_raw(slots.get(Slot::PlayHealthRaw)?, "/play/healthRaw")?;
    let (rank_cur, rank_max) = parse_rank_ref(slots.get(Slot::PlayRank)?, "/play/rank")?;
    let mut play = SourcePlay {
        failed: as_bool(slots.get(Slot::PlayFailed)?, "/play/failed")?,
        player_name: as_str(slots.get(Slot::PlayPlayerName)?, "/play/playerName")?.to_owned(),
        mode: parse_number_name(slots.get(Slot::PlayMode)?, "/play/mode")?.into_owned(),
        score: as_f64(slots.get(Slot::PlayScore)?, "/play/score")?,
        accuracy: as_f64(
            slots.get(Slot::PlayAccuracyPercent)?,
            "/play/accuracyPercent",
        )?,
        health_bar: HealthBar {
            normal: (raw.normal / 200.0) * 100.0,
            smooth: (raw.smooth / 200.0) * 100.0,
        },
        judgements: parse_judgements(slots.get(Slot::PlayJudgements)?, "/play/judgements")?,
        statistics: parse_statistics(slots.get(Slot::PlayStatistics)?, "/play/statistics")?,
        hit_errors: HitErrors {
            all: parse_f64_vec(slots.get(Slot::PlayHitErrorsAll)?, "/play/hitErrors/all")?,
        },
        combo: parse_combo(slots.get(Slot::PlayCombo)?, "/play/combo")?,
        mods: decode_mods(slots.get(Slot::PlayMods)?, "/play/mods")?,
        rank: Rank {
            current: rank_cur.to_owned(),
            max_this_play: rank_max.to_owned(),
        },
        pp: parse_pp(slots.get(Slot::PlayPp)?, "/play/pp")?,
        unstable_rate: as_f64(slots.get(Slot::PlayUnstableRate)?, "/play/unstableRate")?,
    }
    .into_wire(false);
    let paths = parse_paths(slots.get(Slot::InternalPaths)?, lazer, "/internal/paths")?;
    let direct_path = DirectPath {
        beatmap_file: crate::paths::direct_join(
            paths.current.beatmap_folder,
            paths.current.beatmap_file,
        ),
        beatmap_background: crate::paths::direct_join(
            paths.current.beatmap_folder,
            paths.current.background,
        ),
        beatmap_audio: crate::paths::direct_join(paths.current.beatmap_folder, paths.current.audio),
        beatmap_folder: paths.current.beatmap_folder.map(str::to_owned),
        skin_folder: paths.skin.to_owned(),
    };
    let state = parse_number_name(slots.get(Slot::SessionState)?, "/session/state")?.into_owned();
    let beatmap = decode_beatmap(slots.get(Slot::Beatmap)?, "/beatmap")?;
    let performance = decode_performance(slots.get(Slot::Performance)?, "/performance")?;
    play.pp.fc = vosu_adapter_support::full_combo_pp(
        play.pp.fc,
        performance.accuracy.acc_100,
        state.number.unwrap_or_default(),
        beatmap.time.live,
        beatmap.time.first_object,
    );
    play.pp.detailed.fc.total = play.pp.fc;
    let paused = match slots.get(Slot::PlayClock) {
        Ok(val) => Some(parse_play_clock_paused(val, super::PLAY_CLOCK)?),
        Err(Error::NotReady) => None,
        Err(error) => return Err(error),
    };
    let focused = cfg!(target_os = "windows").then(|| {
        capture
            .roster
            .sessions
            .iter()
            .filter(|session| {
                session.owner == identity.session.owner
                    && session.owner_instance == identity.session.owner_instance
            })
            .any(|session| session.foreground == Some(true))
    });
    let server = as_opt_str(slots.get(Slot::SessionServer)?, "/session/server")?
        .unwrap_or("ppy.sh")
        .to_owned();
    let play_time = as_f64(slots.get(Slot::SessionPlayTime)?, "/session/playTime")?;
    let ranked_play = {
        let val = slots.get(Slot::RankedPlay)?;
        if matches!(val, Value::Null) {
            None
        } else {
            Some(decode_ranked_play(val, "/rankedPlay")?)
        }
    };
    let leaderboard = decode_leaderboard(slots.get(Slot::Leaderboard)?, "/leaderboard")?;
    let results_screen = decode_results(slots.get(Slot::Result)?, "/result")?;
    let room = {
        let val = slots.get(Slot::Room)?;
        if matches!(val, Value::Null) {
            None
        } else {
            Some(decode_room(val, "/room")?)
        }
    };
    Ok(Response {
        game: Game { focused, paused },
        client: identity.session.variant.clone(),
        server,
        state,
        session: Session {
            play_time,
            play_count: 0,
        },
        ranked_play,
        settings,
        profile,
        beatmap,
        play,
        leaderboard,
        performance,
        results_screen,
        folders: Folders {
            game: paths.game.to_owned(),
            songs: paths.songs.to_owned(),
            skin: paths.skin.to_owned(),
            beatmap: paths.current.beatmap_folder.map(str::to_owned),
        },
        files: Files {
            beatmap: paths.current.beatmap_file.map(str::to_owned),
            background: paths.current.background.map(str::to_owned),
            audio: paths.current.audio.map(str::to_owned),
        },
        direct_path,
        tourney,
        room,
    })
}

pub(super) fn decode_mods(v: &Value, ctx: &str) -> Result<CalculateMods, Error> {
    let (checksum, number, name, items, rate) = parse_mods_ref(v, ctx)?;
    let mut array = Vec::with_capacity(items.len());
    for item in items {
        let mut acronym = None;
        let mut settings = None;
        for (mk, mv) in as_map(item, ctx)? {
            match mk.as_text() {
                Some("acronym") => acronym = Some(as_str(mv, ctx)?.to_owned()),
                Some("settings") if !matches!(mv, Value::Null) => {
                    settings = Some(
                        mv.deserialized()
                            .map_err(|error| Error::InvalidSource(format!("{ctx}: {error}")))?,
                    );
                }
                _ => {}
            }
        }
        array.push(Mod {
            acronym: acronym.ok_or_else(|| missing(ctx, "acronym"))?,
            settings,
        });
    }
    Ok(CalculateMods {
        checksum: checksum.to_owned(),
        number,
        name: name.to_owned(),
        array,
        rate,
    })
}

fn decode_settings(v: &Value, skin_name: &str, ctx: &str) -> Result<Settings, Error> {
    let mut interface_visible = None;
    let mut replay_ui_visible = None;
    let mut chat_visibility_status = None;
    let mut leaderboard = None;
    let mut progress_bar = None;
    let mut bass_density = None;
    let mut resolution = None;
    let mut client = None;
    let mut score_meter = None;
    let mut cursor = None;
    let mut mouse = None;
    let mut tablet = None;
    let mut mania = None;
    let mut sort = None;
    let mut group = None;
    let mut skin = None;
    let mut mode = None;
    let mut audio = None;
    let mut background = None;
    let mut keybinds = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("interfaceVisible") => interface_visible = Some(as_bool(val, ctx)?),
            Some("replayUIVisible") => replay_ui_visible = Some(as_bool(val, ctx)?),
            Some("chatVisibilityStatus") => {
                chat_visibility_status = Some(parse_number_name(val, ctx)?.into_owned());
            }
            Some("leaderboard") => {
                let mut visible = None;
                let mut kind = None;
                for (lk, lv) in as_map(val, ctx)? {
                    match lk.as_text() {
                        Some("visible") => visible = Some(as_bool(lv, ctx)?),
                        Some("type") => kind = Some(parse_number_name(lv, ctx)?.into_owned()),
                        _ => {}
                    }
                }
                leaderboard = Some(LeaderboardSettings {
                    visible: visible.ok_or_else(|| missing(ctx, "leaderboard.visible"))?,
                    kind: kind.ok_or_else(|| missing(ctx, "leaderboard.type"))?,
                });
            }
            Some("progressBar") => progress_bar = Some(parse_number_name(val, ctx)?.into_owned()),
            Some("bassDensity") => bass_density = Some(as_f64(val, ctx)?),
            Some("resolution") => resolution = Some(parse_resolution(val, ctx)?),
            Some("client") => {
                let mut update_available = None;
                let mut branch = None;
                let mut version = None;
                for (ck, cv) in as_map(val, ctx)? {
                    match ck.as_text() {
                        Some("updateAvailable") => update_available = Some(as_bool(cv, ctx)?),
                        Some("branch") => branch = Some(as_i64(cv, ctx)?),
                        Some("version") => version = Some(as_str(cv, ctx)?.to_owned()),
                        _ => {}
                    }
                }
                client = Some(Client {
                    update_available: update_available
                        .ok_or_else(|| missing(ctx, "client.updateAvailable"))?,
                    branch: branch.ok_or_else(|| missing(ctx, "client.branch"))?,
                    version: version.ok_or_else(|| missing(ctx, "client.version"))?,
                });
            }
            Some("scoreMeter") => {
                let mut kind = None;
                let mut size = None;
                for (sk, sv) in as_map(val, ctx)? {
                    match sk.as_text() {
                        Some("type") => kind = Some(parse_number_name(sv, ctx)?.into_owned()),
                        Some("size") => size = Some(as_f64(sv, ctx)?),
                        _ => {}
                    }
                }
                score_meter = Some(ScoreMeter {
                    kind: kind.ok_or_else(|| missing(ctx, "scoreMeter.type"))?,
                    size: size.ok_or_else(|| missing(ctx, "scoreMeter.size"))?,
                });
            }
            Some("cursor") => cursor = Some(parse_cursor(val, ctx)?),
            Some("mouse") => mouse = Some(parse_mouse(val, ctx)?),
            Some("tablet") => tablet = Some(parse_tablet(val, ctx)?),
            Some("mania") => {
                let mut speed_bpm_scale = None;
                let mut use_per_beatmap_speed_scale = None;
                let mut scroll_speed = None;
                let mut scroll_direction = None;
                for (mk, mv) in as_map(val, ctx)? {
                    match mk.as_text() {
                        Some("speedBPMScale") => speed_bpm_scale = Some(as_bool(mv, ctx)?),
                        Some("usePerBeatmapSpeedScale") => {
                            use_per_beatmap_speed_scale = Some(as_bool(mv, ctx)?);
                        }
                        Some("scrollSpeed") => scroll_speed = Some(as_i64(mv, ctx)?),
                        Some("scrollDirection") => {
                            let sd = parse_scroll_direction(mv, ctx)?;
                            scroll_direction = Some(ScrollDirection {
                                number: sd.number,
                                name: sd.name.map(str::to_owned),
                            });
                        }
                        _ => {}
                    }
                }
                mania = Some(Mania {
                    speed_bpm_scale: speed_bpm_scale
                        .ok_or_else(|| missing(ctx, "mania.speedBPMScale"))?,
                    use_per_beatmap_speed_scale: use_per_beatmap_speed_scale
                        .ok_or_else(|| missing(ctx, "mania.usePerBeatmapSpeedScale"))?,
                    scroll_speed: scroll_speed.ok_or_else(|| missing(ctx, "mania.scrollSpeed"))?,
                    scroll_direction: scroll_direction
                        .ok_or_else(|| missing(ctx, "mania.scrollDirection"))?,
                });
            }
            Some("sort") => sort = Some(parse_number_name(val, ctx)?.into_owned()),
            Some("group") => group = Some(parse_number_name(val, ctx)?.into_owned()),
            Some("skin") => {
                let mut use_default_skin_in_editor = None;
                let mut ignore_beatmap_skins = None;
                let mut tint_slider_ball = None;
                let mut use_taiko_skin = None;
                for (sk, sv) in as_map(val, ctx)? {
                    match sk.as_text() {
                        Some("useDefaultSkinInEditor") => {
                            use_default_skin_in_editor = Some(as_bool(sv, ctx)?);
                        }
                        Some("ignoreBeatmapSkins") => {
                            ignore_beatmap_skins = Some(as_bool(sv, ctx)?);
                        }
                        Some("tintSliderBall") => tint_slider_ball = Some(as_bool(sv, ctx)?),
                        Some("useTaikoSkin") => use_taiko_skin = Some(as_bool(sv, ctx)?),
                        _ => {}
                    }
                }
                skin = Some(Skin {
                    use_default_skin_in_editor: use_default_skin_in_editor
                        .ok_or_else(|| missing(ctx, "skin.useDefaultSkinInEditor"))?,
                    ignore_beatmap_skins: ignore_beatmap_skins
                        .ok_or_else(|| missing(ctx, "skin.ignoreBeatmapSkins"))?,
                    tint_slider_ball: tint_slider_ball
                        .ok_or_else(|| missing(ctx, "skin.tintSliderBall"))?,
                    use_taiko_skin: use_taiko_skin
                        .ok_or_else(|| missing(ctx, "skin.useTaikoSkin"))?,
                    name: skin_name.to_owned(),
                });
            }
            Some("mode") => mode = Some(parse_number_name(val, ctx)?.into_owned()),
            Some("audio") => audio = Some(parse_audio(val, ctx)?),
            Some("background") => background = Some(parse_background(val, ctx)?),
            Some("keybinds") => keybinds = Some(decode_keybinds(val, ctx)?),
            _ => {}
        }
    }
    Ok(Settings {
        interface_visible: interface_visible.ok_or_else(|| missing(ctx, "interfaceVisible"))?,
        replay_ui_visible: replay_ui_visible.ok_or_else(|| missing(ctx, "replayUIVisible"))?,
        chat_visibility_status: chat_visibility_status
            .ok_or_else(|| missing(ctx, "chatVisibilityStatus"))?,
        leaderboard: leaderboard.ok_or_else(|| missing(ctx, "leaderboard"))?,
        progress_bar: progress_bar.ok_or_else(|| missing(ctx, "progressBar"))?,
        bass_density: bass_density.ok_or_else(|| missing(ctx, "bassDensity"))?,
        resolution: resolution.ok_or_else(|| missing(ctx, "resolution"))?,
        client: client.ok_or_else(|| missing(ctx, "client"))?,
        score_meter: score_meter.ok_or_else(|| missing(ctx, "scoreMeter"))?,
        cursor: cursor.ok_or_else(|| missing(ctx, "cursor"))?,
        mouse: mouse.ok_or_else(|| missing(ctx, "mouse"))?,
        tablet: tablet.ok_or_else(|| missing(ctx, "tablet"))?,
        mania: mania.ok_or_else(|| missing(ctx, "mania"))?,
        sort: sort.ok_or_else(|| missing(ctx, "sort"))?,
        group: group.ok_or_else(|| missing(ctx, "group"))?,
        skin: skin.ok_or_else(|| missing(ctx, "skin"))?,
        mode: mode.ok_or_else(|| missing(ctx, "mode"))?,
        audio: audio.ok_or_else(|| missing(ctx, "audio"))?,
        background: background.ok_or_else(|| missing(ctx, "background"))?,
        keybinds: keybinds.ok_or_else(|| missing(ctx, "keybinds"))?,
    })
}

fn decode_keybinds(v: &Value, ctx: &str) -> Result<Keybinds, Error> {
    let mut osu = None;
    let mut fruits = None;
    let mut taiko = None;
    let mut quick_retry = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("osu") => {
                let mut k1 = None;
                let mut k2 = None;
                let mut smoke_key = None;
                for (ok, ov) in as_map(val, ctx)? {
                    match ok.as_text() {
                        Some("k1") => k1 = Some(as_str(ov, ctx)?.to_owned()),
                        Some("k2") => k2 = Some(as_str(ov, ctx)?.to_owned()),
                        Some("smokeKey") => smoke_key = Some(as_str(ov, ctx)?.to_owned()),
                        _ => {}
                    }
                }
                osu = Some(KeybindsOsu {
                    k1: k1.ok_or_else(|| missing(ctx, "keybinds.osu.k1"))?,
                    k2: k2.ok_or_else(|| missing(ctx, "keybinds.osu.k2"))?,
                    smoke_key: smoke_key.ok_or_else(|| missing(ctx, "keybinds.osu.smokeKey"))?,
                });
            }
            Some("fruits") => {
                let mut k1 = None;
                let mut k2 = None;
                let mut dash = None;
                for (fk, fv) in as_map(val, ctx)? {
                    match fk.as_text() {
                        Some("k1") => k1 = Some(as_str(fv, ctx)?.to_owned()),
                        Some("k2") => k2 = Some(as_str(fv, ctx)?.to_owned()),
                        Some("Dash") => dash = Some(as_str(fv, ctx)?.to_owned()),
                        _ => {}
                    }
                }
                fruits = Some(KeybindsFruits {
                    k1: k1.ok_or_else(|| missing(ctx, "keybinds.fruits.k1"))?,
                    k2: k2.ok_or_else(|| missing(ctx, "keybinds.fruits.k2"))?,
                    dash: dash.ok_or_else(|| missing(ctx, "keybinds.fruits.Dash"))?,
                });
            }
            Some("taiko") => {
                let mut inner_left = None;
                let mut inner_right = None;
                let mut outer_left = None;
                let mut outer_right = None;
                for (tk, tv) in as_map(val, ctx)? {
                    match tk.as_text() {
                        Some("innerLeft") => inner_left = Some(as_str(tv, ctx)?.to_owned()),
                        Some("innerRight") => inner_right = Some(as_str(tv, ctx)?.to_owned()),
                        Some("outerLeft") => outer_left = Some(as_str(tv, ctx)?.to_owned()),
                        Some("outerRight") => outer_right = Some(as_str(tv, ctx)?.to_owned()),
                        _ => {}
                    }
                }
                taiko = Some(KeybindsTaiko {
                    inner_left: inner_left
                        .ok_or_else(|| missing(ctx, "keybinds.taiko.innerLeft"))?,
                    inner_right: inner_right
                        .ok_or_else(|| missing(ctx, "keybinds.taiko.innerRight"))?,
                    outer_left: outer_left
                        .ok_or_else(|| missing(ctx, "keybinds.taiko.outerLeft"))?,
                    outer_right: outer_right
                        .ok_or_else(|| missing(ctx, "keybinds.taiko.outerRight"))?,
                });
            }
            Some("quickRetry") => quick_retry = Some(as_str(val, ctx)?.to_owned()),
            _ => {}
        }
    }
    Ok(Keybinds {
        osu: osu.ok_or_else(|| missing(ctx, "keybinds.osu"))?,
        fruits: fruits.ok_or_else(|| missing(ctx, "keybinds.fruits"))?,
        taiko: taiko.ok_or_else(|| missing(ctx, "keybinds.taiko"))?,
        quick_retry: quick_retry.ok_or_else(|| missing(ctx, "keybinds.quickRetry"))?,
    })
}

fn decode_profile(v: &Value, ctx: &str) -> Result<Profile, Error> {
    let mut user_status = None;
    let mut bancho_status = None;
    let mut id = None;
    let mut name = None;
    let mut mode = None;
    let mut ranked_score = None;
    let mut level = None;
    let mut accuracy = None;
    let mut pp = None;
    let mut play_count = None;
    let mut global_rank = None;
    let mut country_code = None;
    let mut background_colour = None;
    let mut mm = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("userStatus") => {
                user_status = parse_opt_number_name(val, ctx)?.map(NumberNameRef::into_owned);
            }
            Some("banchoStatus") => {
                bancho_status = parse_opt_number_name(val, ctx)?.map(NumberNameRef::into_owned);
            }
            Some("id") => id = as_opt_i64(val, ctx)?,
            Some("name") => name = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("mode") => {
                mode = parse_opt_number_name(val, ctx)?.map(NumberNameRef::into_owned);
            }
            Some("rankedScore") => ranked_score = as_opt_f64(val, ctx)?,
            Some("level") => level = as_opt_f64(val, ctx)?,
            Some("accuracy") => accuracy = as_opt_f64(val, ctx)?,
            Some("pp") => pp = as_opt_f64(val, ctx)?,
            Some("playCount") => play_count = as_opt_i64(val, ctx)?,
            Some("globalRank") => global_rank = as_opt_i64(val, ctx)?,
            Some("countryCode") => {
                country_code = parse_opt_number_name(val, ctx)?.map(NumberNameRef::into_owned);
            }
            Some("backgroundColour") => {
                background_colour = as_opt_str(val, ctx)?.map(str::to_owned);
            }
            Some("matchmaking") if !matches!(val, Value::Null) => {
                mm = Some(parse_matchmaking(val, ctx)?);
            }
            _ => {}
        }
    }
    let matchmaking = match mm {
        Some(stats) => Some(Some(stats)),
        None if id.is_some() => Some(None),
        None => None,
    };
    Ok(Profile {
        user_status,
        bancho_status,
        id,
        name,
        mode,
        ranked_score,
        level,
        accuracy,
        pp,
        play_count,
        global_rank,
        country_code,
        background_colour,
        matchmaking,
    })
}

fn decode_beatmap(v: &Value, ctx: &str) -> Result<Beatmap, Error> {
    let mut is_kiai = None;
    let mut is_break = None;
    let mut is_convert = None;
    let mut time = None;
    let mut status = None;
    let mut checksum = None;
    let mut id = None;
    let mut set = None;
    let mut mode = None;
    let mut artist = None;
    let mut artist_unicode = None;
    let mut title = None;
    let mut title_unicode = None;
    let mut mapper = None;
    let mut version = None;
    let mut source = None;
    let mut tags = None;
    let mut stats = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("isKiai") => is_kiai = Some(as_bool(val, ctx)?),
            Some("isBreak") => is_break = Some(as_bool(val, ctx)?),
            Some("isConvert") => is_convert = Some(as_bool(val, ctx)?),
            Some("time") => time = Some(parse_beatmap_time(val, ctx)?),
            Some("status") => {
                let s = parse_beatmap_status(val, ctx)?;
                status = Some(BeatmapStatus {
                    number: s.number,
                    name: s.name.to_owned(),
                });
            }
            Some("checksum") => checksum = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("id") => id = as_opt_i64(val, ctx)?,
            Some("set") => set = as_opt_i64(val, ctx)?,
            Some("mode") => mode = Some(parse_number_name(val, ctx)?.into_owned()),
            Some("artist") => artist = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("artistUnicode") => artist_unicode = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("title") => title = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("titleUnicode") => title_unicode = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("mapper") => mapper = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("version") => version = as_opt_str(val, ctx)?.map(str::to_owned),
            Some("source") => source = Some(as_str(val, ctx)?.to_owned()),
            Some("tags") => tags = Some(as_str(val, ctx)?.to_owned()),
            Some("stats") => stats = Some(parse_stats(val, ctx)?),
            _ => {}
        }
    }
    Ok(Beatmap {
        is_kiai: is_kiai.ok_or_else(|| missing(ctx, "isKiai"))?,
        is_break: is_break.ok_or_else(|| missing(ctx, "isBreak"))?,
        is_convert: is_convert.ok_or_else(|| missing(ctx, "isConvert"))?,
        time: time.ok_or_else(|| missing(ctx, "time"))?,
        status: status.ok_or_else(|| missing(ctx, "status"))?,
        checksum,
        id,
        set,
        mode: mode.ok_or_else(|| missing(ctx, "mode"))?,
        artist,
        artist_unicode,
        title,
        title_unicode,
        mapper,
        version,
        source: source.ok_or_else(|| missing(ctx, "source"))?,
        tags: tags.ok_or_else(|| missing(ctx, "tags"))?,
        stats: stats.ok_or_else(|| missing(ctx, "stats"))?,
    })
}

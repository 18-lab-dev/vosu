//! Settings, leaderboard, results, ranked-play, and tourney CBOR parsers.
use crate::{
    Error,
    dto::*,
    view::{as_array, as_bool, as_f64, as_i64, as_map, as_opt_i64, as_str, missing},
};
use ciborium::Value;
use vosu_osu_model::enums::RankedPlayStage;

pub(super) fn parse_ranked_play_stage(v: &Value, ctx: &str) -> Result<RankedPlayStage, Error> {
    let raw = as_i64(v, ctx)?;
    i32::try_from(raw)
        .ok()
        .and_then(RankedPlayStage::from_value)
        .ok_or_else(|| Error::InvalidSource(format!("{ctx}: invalid RankedPlayStage {raw}")))
}

pub(super) fn parse_matchmaking(v: &Value, ctx: &str) -> Result<MatchmakingStats, Error> {
    let mut rating = None;
    let mut rank = None;
    let mut plays = None;
    let mut wins = None;
    let mut is_provisional = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("rating") => rating = Some(as_f64(val, ctx)?),
            Some("rank") => rank = as_opt_i64(val, ctx)?,
            Some("plays") => plays = Some(as_i64(val, ctx)?),
            Some("wins") => wins = Some(as_i64(val, ctx)?),
            Some("isProvisional") => is_provisional = Some(as_bool(val, ctx)?),
            _ => {}
        }
    }
    Ok(MatchmakingStats {
        rating: rating.ok_or_else(|| missing(ctx, "rating"))?,
        rank,
        plays: plays.ok_or_else(|| missing(ctx, "plays"))?,
        wins: wins.ok_or_else(|| missing(ctx, "wins"))?,
        is_provisional: is_provisional.ok_or_else(|| missing(ctx, "isProvisional"))?,
    })
}

pub(super) fn parse_leaderboard_hits(v: &Value, ctx: &str) -> Result<LeaderboardHits, Error> {
    let mut h0 = None;
    let mut h50 = None;
    let mut h100 = None;
    let mut h300 = None;
    let mut geki = None;
    let mut katu = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("0" | "miss") => h0 = Some(as_i64(val, ctx)?),
            Some("50" | "meh") => h50 = Some(as_i64(val, ctx)?),
            Some("100" | "ok") => h100 = Some(as_i64(val, ctx)?),
            Some("300" | "great") => h300 = Some(as_i64(val, ctx)?),
            Some("geki" | "perfect") => geki = Some(as_i64(val, ctx)?),
            Some("katu" | "good") => katu = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(LeaderboardHits {
        h0: h0.ok_or_else(|| missing(ctx, "0"))?,
        h50: h50.ok_or_else(|| missing(ctx, "50"))?,
        h100: h100.ok_or_else(|| missing(ctx, "100"))?,
        h300: h300.ok_or_else(|| missing(ctx, "300"))?,
        geki: geki.ok_or_else(|| missing(ctx, "geki"))?,
        katu: katu.ok_or_else(|| missing(ctx, "katu"))?,
    })
}

pub(super) fn parse_results_hits(v: &Value, ctx: &str) -> Result<ResultsHits, Error> {
    let mut h0 = None;
    let mut h50 = None;
    let mut h100 = None;
    let mut h300 = None;
    let mut geki = None;
    let mut katu = None;
    let mut slider_end_hits = None;
    let mut small_tick_hits = None;
    let mut large_tick_hits = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("0" | "miss") => h0 = Some(as_i64(val, ctx)?),
            Some("50" | "meh") => h50 = Some(as_i64(val, ctx)?),
            Some("100" | "ok") => h100 = Some(as_i64(val, ctx)?),
            Some("300" | "great") => h300 = Some(as_i64(val, ctx)?),
            Some("geki" | "perfect") => geki = Some(as_i64(val, ctx)?),
            Some("katu" | "good") => katu = Some(as_i64(val, ctx)?),
            Some("sliderEndHits") => slider_end_hits = Some(as_i64(val, ctx)?),
            Some("smallTickHits") => small_tick_hits = Some(as_i64(val, ctx)?),
            Some("largeTickHits") => large_tick_hits = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(ResultsHits {
        h0: h0.ok_or_else(|| missing(ctx, "0"))?,
        h50: h50.ok_or_else(|| missing(ctx, "50"))?,
        h100: h100.ok_or_else(|| missing(ctx, "100"))?,
        h300: h300.ok_or_else(|| missing(ctx, "300"))?,
        geki: geki.ok_or_else(|| missing(ctx, "geki"))?,
        katu: katu.ok_or_else(|| missing(ctx, "katu"))?,
        slider_end_hits: slider_end_hits.ok_or_else(|| missing(ctx, "sliderEndHits"))?,
        small_tick_hits: small_tick_hits.ok_or_else(|| missing(ctx, "smallTickHits"))?,
        large_tick_hits: large_tick_hits.ok_or_else(|| missing(ctx, "largeTickHits"))?,
    })
}

pub(super) fn parse_results_pp(v: &Value, ctx: &str) -> Result<ResultsPp, Error> {
    let mut current = None;
    let mut fc = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("current") => current = Some(as_f64(val, ctx)?),
            Some("fc") => fc = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(ResultsPp {
        current: current.ok_or_else(|| missing(ctx, "current"))?,
        fc: fc.ok_or_else(|| missing(ctx, "fc"))?,
    })
}

pub(super) fn parse_resolution(v: &Value, ctx: &str) -> Result<Resolution, Error> {
    let mut fullscreen = None;
    let mut width = None;
    let mut height = None;
    let mut width_fullscreen = None;
    let mut height_fullscreen = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("fullscreen") => fullscreen = Some(as_bool(val, ctx)?),
            Some("width") => width = Some(as_i64(val, ctx)?),
            Some("height") => height = Some(as_i64(val, ctx)?),
            Some("widthFullscreen") => width_fullscreen = Some(as_i64(val, ctx)?),
            Some("heightFullscreen") => height_fullscreen = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Resolution {
        fullscreen: fullscreen.ok_or_else(|| missing(ctx, "fullscreen"))?,
        width: width.ok_or_else(|| missing(ctx, "width"))?,
        height: height.ok_or_else(|| missing(ctx, "height"))?,
        width_fullscreen: width_fullscreen.ok_or_else(|| missing(ctx, "widthFullscreen"))?,
        height_fullscreen: height_fullscreen.ok_or_else(|| missing(ctx, "heightFullscreen"))?,
    })
}

pub(super) fn parse_cursor(v: &Value, ctx: &str) -> Result<Cursor, Error> {
    let mut use_skin_cursor = None;
    let mut auto_size = None;
    let mut size = None;
    let mut menu_size = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("useSkinCursor") => use_skin_cursor = Some(as_bool(val, ctx)?),
            Some("autoSize") => auto_size = Some(as_bool(val, ctx)?),
            Some("size") => size = Some(as_f64(val, ctx)?),
            Some("menuSize") => menu_size = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Cursor {
        use_skin_cursor: use_skin_cursor.ok_or_else(|| missing(ctx, "useSkinCursor"))?,
        auto_size: auto_size.ok_or_else(|| missing(ctx, "autoSize"))?,
        size: size.ok_or_else(|| missing(ctx, "size"))?,
        menu_size: menu_size.ok_or_else(|| missing(ctx, "menuSize"))?,
    })
}

pub(super) fn parse_mouse(v: &Value, ctx: &str) -> Result<Mouse, Error> {
    let mut raw_input = None;
    let mut disable_buttons = None;
    let mut disable_wheel = None;
    let mut sensitivity = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("rawInput") => raw_input = Some(as_bool(val, ctx)?),
            Some("disableButtons") => disable_buttons = Some(as_bool(val, ctx)?),
            Some("disableWheel") => disable_wheel = Some(as_bool(val, ctx)?),
            Some("sensitivity") => sensitivity = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Mouse {
        raw_input: raw_input.ok_or_else(|| missing(ctx, "rawInput"))?,
        disable_buttons: disable_buttons.ok_or_else(|| missing(ctx, "disableButtons"))?,
        disable_wheel: disable_wheel.ok_or_else(|| missing(ctx, "disableWheel"))?,
        sensitivity: sensitivity.ok_or_else(|| missing(ctx, "sensitivity"))?,
    })
}

pub(super) fn parse_tablet(v: &Value, ctx: &str) -> Result<Tablet, Error> {
    let mut enabled = None;
    let mut x = None;
    let mut y = None;
    let mut width = None;
    let mut height = None;
    let mut rotation = None;
    let mut pressure_threshold = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("enabled") => enabled = Some(as_bool(val, ctx)?),
            Some("x") => x = Some(as_f64(val, ctx)?),
            Some("y") => y = Some(as_f64(val, ctx)?),
            Some("width") => width = Some(as_f64(val, ctx)?),
            Some("height") => height = Some(as_f64(val, ctx)?),
            Some("rotation") => rotation = Some(as_f64(val, ctx)?),
            Some("pressureThreshold") => pressure_threshold = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Tablet {
        enabled: enabled.ok_or_else(|| missing(ctx, "enabled"))?,
        x: x.ok_or_else(|| missing(ctx, "x"))?,
        y: y.ok_or_else(|| missing(ctx, "y"))?,
        width: width.ok_or_else(|| missing(ctx, "width"))?,
        height: height.ok_or_else(|| missing(ctx, "height"))?,
        rotation: rotation.ok_or_else(|| missing(ctx, "rotation"))?,
        pressure_threshold: pressure_threshold.ok_or_else(|| missing(ctx, "pressureThreshold"))?,
    })
}

pub(super) fn parse_audio(v: &Value, ctx: &str) -> Result<Audio, Error> {
    let mut ignore_beatmap_sounds = None;
    let mut use_skin_samples = None;
    let mut volume = None;
    let mut offset = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("ignoreBeatmapSounds") => ignore_beatmap_sounds = Some(as_bool(val, ctx)?),
            Some("useSkinSamples") => use_skin_samples = Some(as_bool(val, ctx)?),
            Some("volume") => {
                let mut master_inactive = None;
                let mut master = None;
                let mut music = None;
                let mut effect = None;
                for (vk, vv) in as_map(val, ctx)? {
                    match vk.as_text() {
                        Some("masterInactive") => master_inactive = Some(as_f64(vv, ctx)?),
                        Some("master") => master = Some(as_f64(vv, ctx)?),
                        Some("music") => music = Some(as_f64(vv, ctx)?),
                        Some("effect") => effect = Some(as_f64(vv, ctx)?),
                        _ => {}
                    }
                }
                volume = Some(Volume {
                    master_inactive: master_inactive
                        .ok_or_else(|| missing(ctx, "volume.masterInactive"))?,
                    master: master.ok_or_else(|| missing(ctx, "volume.master"))?,
                    music: music.ok_or_else(|| missing(ctx, "volume.music"))?,
                    effect: effect.ok_or_else(|| missing(ctx, "volume.effect"))?,
                });
            }
            Some("offset") => {
                let mut universal = None;
                for (ok, ov) in as_map(val, ctx)? {
                    if ok.as_text() == Some("universal") {
                        universal = Some(as_f64(ov, ctx)?);
                    }
                }
                offset = Some(Offset {
                    universal: universal.ok_or_else(|| missing(ctx, "offset.universal"))?,
                });
            }
            _ => {}
        }
    }
    Ok(Audio {
        ignore_beatmap_sounds: ignore_beatmap_sounds
            .ok_or_else(|| missing(ctx, "ignoreBeatmapSounds"))?,
        use_skin_samples: use_skin_samples.ok_or_else(|| missing(ctx, "useSkinSamples"))?,
        volume: volume.ok_or_else(|| missing(ctx, "volume"))?,
        offset: offset.ok_or_else(|| missing(ctx, "offset"))?,
    })
}

pub(super) fn parse_background(v: &Value, ctx: &str) -> Result<Background, Error> {
    let mut storyboard = None;
    let mut video = None;
    let mut blur = None;
    let mut dim = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("storyboard") => storyboard = Some(as_bool(val, ctx)?),
            Some("video") => video = Some(as_bool(val, ctx)?),
            Some("blur") => blur = Some(as_f64(val, ctx)?),
            Some("dim") => dim = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Background {
        storyboard: storyboard.ok_or_else(|| missing(ctx, "storyboard"))?,
        video: video.ok_or_else(|| missing(ctx, "video"))?,
        blur: blur.ok_or_else(|| missing(ctx, "blur"))?,
        dim: dim.ok_or_else(|| missing(ctx, "dim"))?,
    })
}

pub(super) fn parse_tourney_team(v: &Value, ctx: &str) -> Result<TourneyTeam, Error> {
    match as_str(v, ctx)? {
        "left" => Ok(TourneyTeam::Left),
        "right" => Ok(TourneyTeam::Right),
        other => Err(Error::InvalidSource(format!(
            "{ctx}: unknown tourney team `{other}`"
        ))),
    }
}

pub(super) fn parse_sides_i64(v: &Value, ctx: &str) -> Result<Sides<i64>, Error> {
    let mut left = None;
    let mut right = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("left") => left = Some(as_i64(val, ctx)?),
            Some("right") => right = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Sides {
        left: left.ok_or_else(|| missing(ctx, "left"))?,
        right: right.ok_or_else(|| missing(ctx, "right"))?,
    })
}

pub(super) fn parse_sides_f64(v: &Value, ctx: &str) -> Result<Sides<f64>, Error> {
    let mut left = None;
    let mut right = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("left") => left = Some(as_f64(val, ctx)?),
            Some("right") => right = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(Sides {
        left: left.ok_or_else(|| missing(ctx, "left"))?,
        right: right.ok_or_else(|| missing(ctx, "right"))?,
    })
}

pub(super) fn parse_sides_str<'a>(v: &'a Value, ctx: &str) -> Result<(&'a str, &'a str), Error> {
    let mut left = None;
    let mut right = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("left") => left = Some(as_str(val, ctx)?),
            Some("right") => right = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    Ok((
        left.ok_or_else(|| missing(ctx, "left"))?,
        right.ok_or_else(|| missing(ctx, "right"))?,
    ))
}

pub(super) fn parse_rank_ref<'a>(v: &'a Value, ctx: &str) -> Result<(&'a str, &'a str), Error> {
    let mut current = None;
    let mut max_this_play = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("current") => current = Some(as_str(val, ctx)?),
            Some("maxThisPlay") => max_this_play = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    Ok((
        current.ok_or_else(|| missing(ctx, "current"))?,
        max_this_play.ok_or_else(|| missing(ctx, "maxThisPlay"))?,
    ))
}

pub(super) fn parse_mods_ref<'a>(
    v: &'a Value,
    ctx: &str,
) -> Result<(&'a str, i64, &'a str, &'a [Value], f64), Error> {
    let mut checksum = None;
    let mut number = None;
    let mut name = None;
    let mut array = None;
    let mut rate = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("checksum") => checksum = Some(as_str(val, ctx)?),
            Some("number") => number = Some(as_i64(val, ctx)?),
            Some("name") => name = Some(as_str(val, ctx)?),
            Some("array") => array = Some(as_array(val, ctx)?),
            Some("rate") => rate = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok((
        checksum.ok_or_else(|| missing(ctx, "checksum"))?,
        number.ok_or_else(|| missing(ctx, "number"))?,
        name.ok_or_else(|| missing(ctx, "name"))?,
        array.ok_or_else(|| missing(ctx, "array"))?,
        rate.ok_or_else(|| missing(ctx, "rate"))?,
    ))
}

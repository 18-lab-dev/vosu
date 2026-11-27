//! Direct CBOR -> JSON writer for `/settings`, keeping stack frames compact.
use super::{
    direct::push_number_name,
    parse::{parse_number_name, parse_scroll_direction},
    parse_settings::{
        parse_audio, parse_background, parse_cursor, parse_mouse, parse_resolution, parse_tablet,
    },
};
use crate::{
    Error,
    view::{as_bool, as_f64, as_i64, as_map, as_str, missing},
};
use ciborium::Value;
use vosu_adapter_support::json::{push_bool, push_js_number, push_str, write_i64};

pub(super) fn write_settings(
    out: &mut Vec<u8>,
    v: &Value,
    skin_name: &str,
    ctx: &str,
) -> Result<(), Error> {
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
            Some("chatVisibilityStatus") => chat_visibility_status = Some(val),
            Some("leaderboard") => leaderboard = Some(val),
            Some("progressBar") => progress_bar = Some(val),
            Some("bassDensity") => bass_density = Some(as_f64(val, ctx)?),
            Some("resolution") => resolution = Some(val),
            Some("client") => client = Some(val),
            Some("scoreMeter") => score_meter = Some(val),
            Some("cursor") => cursor = Some(val),
            Some("mouse") => mouse = Some(val),
            Some("tablet") => tablet = Some(val),
            Some("mania") => mania = Some(val),
            Some("sort") => sort = Some(val),
            Some("group") => group = Some(val),
            Some("skin") => skin = Some(val),
            Some("mode") => mode = Some(val),
            Some("audio") => audio = Some(val),
            Some("background") => background = Some(val),
            Some("keybinds") => keybinds = Some(val),
            _ => {}
        }
    }

    out.extend_from_slice(b"{\"interfaceVisible\":");
    push_bool(
        out,
        interface_visible.ok_or_else(|| missing(ctx, "interfaceVisible"))?,
    );
    out.extend_from_slice(b",\"replayUIVisible\":");
    push_bool(
        out,
        replay_ui_visible.ok_or_else(|| missing(ctx, "replayUIVisible"))?,
    );
    out.extend_from_slice(b",\"chatVisibilityStatus\":");
    let chat_nn = parse_number_name(
        chat_visibility_status.ok_or_else(|| missing(ctx, "chatVisibilityStatus"))?,
        ctx,
    )?;
    push_number_name(out, chat_nn);

    out.extend_from_slice(b",\"leaderboard\":");
    write_leaderboard_settings(
        out,
        leaderboard.ok_or_else(|| missing(ctx, "leaderboard"))?,
        ctx,
    )?;

    out.extend_from_slice(b",\"progressBar\":");
    let pb = parse_number_name(
        progress_bar.ok_or_else(|| missing(ctx, "progressBar"))?,
        ctx,
    )?;
    push_number_name(out, pb);

    out.extend_from_slice(b",\"bassDensity\":");
    push_js_number(
        out,
        bass_density.ok_or_else(|| missing(ctx, "bassDensity"))?,
    );

    out.extend_from_slice(b",\"resolution\":");
    let res = parse_resolution(resolution.ok_or_else(|| missing(ctx, "resolution"))?, ctx)?;
    out.extend_from_slice(b"{\"fullscreen\":");
    push_bool(out, res.fullscreen);
    out.extend_from_slice(b",\"width\":");
    write_i64(out, res.width);
    out.extend_from_slice(b",\"height\":");
    write_i64(out, res.height);
    out.extend_from_slice(b",\"widthFullscreen\":");
    write_i64(out, res.width_fullscreen);
    out.extend_from_slice(b",\"heightFullscreen\":");
    write_i64(out, res.height_fullscreen);
    out.push(b'}');

    out.extend_from_slice(b",\"client\":");
    write_client(out, client.ok_or_else(|| missing(ctx, "client"))?, ctx)?;

    out.extend_from_slice(b",\"scoreMeter\":");
    write_score_meter(
        out,
        score_meter.ok_or_else(|| missing(ctx, "scoreMeter"))?,
        ctx,
    )?;

    out.extend_from_slice(b",\"cursor\":");
    let cur = parse_cursor(cursor.ok_or_else(|| missing(ctx, "cursor"))?, ctx)?;
    out.extend_from_slice(b"{\"useSkinCursor\":");
    push_bool(out, cur.use_skin_cursor);
    out.extend_from_slice(b",\"autoSize\":");
    push_bool(out, cur.auto_size);
    out.extend_from_slice(b",\"size\":");
    push_js_number(out, cur.size);
    out.extend_from_slice(b",\"menuSize\":");
    push_js_number(out, cur.menu_size);
    out.push(b'}');

    out.extend_from_slice(b",\"mouse\":");
    let ms = parse_mouse(mouse.ok_or_else(|| missing(ctx, "mouse"))?, ctx)?;
    out.extend_from_slice(b"{\"rawInput\":");
    push_bool(out, ms.raw_input);
    out.extend_from_slice(b",\"disableButtons\":");
    push_bool(out, ms.disable_buttons);
    out.extend_from_slice(b",\"disableWheel\":");
    push_bool(out, ms.disable_wheel);
    out.extend_from_slice(b",\"sensitivity\":");
    push_js_number(out, ms.sensitivity);
    out.push(b'}');

    out.extend_from_slice(b",\"tablet\":");
    let tb = parse_tablet(tablet.ok_or_else(|| missing(ctx, "tablet"))?, ctx)?;
    out.extend_from_slice(b"{\"enabled\":");
    push_bool(out, tb.enabled);
    out.extend_from_slice(b",\"x\":");
    push_js_number(out, tb.x);
    out.extend_from_slice(b",\"y\":");
    push_js_number(out, tb.y);
    out.extend_from_slice(b",\"width\":");
    push_js_number(out, tb.width);
    out.extend_from_slice(b",\"height\":");
    push_js_number(out, tb.height);
    out.extend_from_slice(b",\"rotation\":");
    push_js_number(out, tb.rotation);
    out.extend_from_slice(b",\"pressureThreshold\":");
    push_js_number(out, tb.pressure_threshold);
    out.push(b'}');

    out.extend_from_slice(b",\"mania\":");
    write_mania(out, mania.ok_or_else(|| missing(ctx, "mania"))?, ctx)?;

    out.extend_from_slice(b",\"sort\":");
    push_number_name(
        out,
        parse_number_name(sort.ok_or_else(|| missing(ctx, "sort"))?, ctx)?,
    );

    out.extend_from_slice(b",\"group\":");
    push_number_name(
        out,
        parse_number_name(group.ok_or_else(|| missing(ctx, "group"))?, ctx)?,
    );

    out.extend_from_slice(b",\"skin\":");
    write_skin(
        out,
        skin.ok_or_else(|| missing(ctx, "skin"))?,
        skin_name,
        ctx,
    )?;

    out.extend_from_slice(b",\"mode\":");
    push_number_name(
        out,
        parse_number_name(mode.ok_or_else(|| missing(ctx, "mode"))?, ctx)?,
    );

    out.extend_from_slice(b",\"audio\":");
    let au = parse_audio(audio.ok_or_else(|| missing(ctx, "audio"))?, ctx)?;
    out.extend_from_slice(b"{\"ignoreBeatmapSounds\":");
    push_bool(out, au.ignore_beatmap_sounds);
    out.extend_from_slice(b",\"useSkinSamples\":");
    push_bool(out, au.use_skin_samples);
    out.extend_from_slice(b",\"volume\":{\"masterInactive\":");
    push_js_number(out, au.volume.master_inactive);
    out.extend_from_slice(b",\"master\":");
    push_js_number(out, au.volume.master);
    out.extend_from_slice(b",\"music\":");
    push_js_number(out, au.volume.music);
    out.extend_from_slice(b",\"effect\":");
    push_js_number(out, au.volume.effect);
    out.extend_from_slice(b"},\"offset\":{\"universal\":");
    push_js_number(out, au.offset.universal);
    out.extend_from_slice(b"}}");

    out.extend_from_slice(b",\"background\":");
    let bg = parse_background(background.ok_or_else(|| missing(ctx, "background"))?, ctx)?;
    out.extend_from_slice(b"{\"storyboard\":");
    push_bool(out, bg.storyboard);
    out.extend_from_slice(b",\"video\":");
    push_bool(out, bg.video);
    out.extend_from_slice(b",\"blur\":");
    push_js_number(out, bg.blur);
    out.extend_from_slice(b",\"dim\":");
    push_js_number(out, bg.dim);
    out.push(b'}');

    out.extend_from_slice(b",\"keybinds\":");
    write_keybinds(out, keybinds.ok_or_else(|| missing(ctx, "keybinds"))?, ctx)?;
    out.push(b'}');
    Ok(())
}

fn write_leaderboard_settings(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut visible = None;
    let mut kind = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("visible") => visible = Some(as_bool(val, ctx)?),
            Some("type") => kind = Some(parse_number_name(val, ctx)?),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"visible\":");
    push_bool(
        out,
        visible.ok_or_else(|| missing(ctx, "leaderboard.visible"))?,
    );
    out.extend_from_slice(b",\"type\":");
    push_number_name(out, kind.ok_or_else(|| missing(ctx, "leaderboard.type"))?);
    out.push(b'}');
    Ok(())
}

fn write_client(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut update_available = None;
    let mut branch = None;
    let mut version = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("updateAvailable") => update_available = Some(as_bool(val, ctx)?),
            Some("branch") => branch = Some(as_i64(val, ctx)?),
            Some("version") => version = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"updateAvailable\":");
    push_bool(
        out,
        update_available.ok_or_else(|| missing(ctx, "client.updateAvailable"))?,
    );
    out.extend_from_slice(b",\"branch\":");
    write_i64(out, branch.ok_or_else(|| missing(ctx, "client.branch"))?);
    out.extend_from_slice(b",\"version\":");
    push_str(out, version.ok_or_else(|| missing(ctx, "client.version"))?);
    out.push(b'}');
    Ok(())
}

fn write_score_meter(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut kind = None;
    let mut size = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("type") => kind = Some(parse_number_name(val, ctx)?),
            Some("size") => size = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"type\":");
    push_number_name(out, kind.ok_or_else(|| missing(ctx, "scoreMeter.type"))?);
    out.extend_from_slice(b",\"size\":");
    push_js_number(out, size.ok_or_else(|| missing(ctx, "scoreMeter.size"))?);
    out.push(b'}');
    Ok(())
}

fn write_mania(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut speed_bpm_scale = None;
    let mut use_per_beatmap = None;
    let mut scroll_speed = None;
    let mut scroll_direction = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("speedBPMScale") => speed_bpm_scale = Some(as_bool(val, ctx)?),
            Some("usePerBeatmapSpeedScale") => use_per_beatmap = Some(as_bool(val, ctx)?),
            Some("scrollSpeed") => scroll_speed = Some(as_i64(val, ctx)?),
            Some("scrollDirection") => scroll_direction = Some(parse_scroll_direction(val, ctx)?),
            _ => {}
        }
    }
    let dir = scroll_direction.ok_or_else(|| missing(ctx, "mania.scrollDirection"))?;
    out.extend_from_slice(b"{\"speedBPMScale\":");
    push_bool(
        out,
        speed_bpm_scale.ok_or_else(|| missing(ctx, "mania.speedBPMScale"))?,
    );
    out.extend_from_slice(b",\"usePerBeatmapSpeedScale\":");
    push_bool(
        out,
        use_per_beatmap.ok_or_else(|| missing(ctx, "mania.usePerBeatmapSpeedScale"))?,
    );
    out.extend_from_slice(b",\"scrollSpeed\":");
    write_i64(
        out,
        scroll_speed.ok_or_else(|| missing(ctx, "mania.scrollSpeed"))?,
    );
    out.extend_from_slice(b",\"scrollDirection\":{\"number\":");
    write_i64(out, dir.number);
    if let Some(name) = dir.name {
        out.extend_from_slice(b",\"name\":");
        push_str(out, name);
    }
    out.extend_from_slice(b"}}");
    Ok(())
}

fn write_skin(out: &mut Vec<u8>, v: &Value, skin_name: &str, ctx: &str) -> Result<(), Error> {
    let mut default_editor = None;
    let mut ignore_beatmap = None;
    let mut tint = None;
    let mut taiko = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("useDefaultSkinInEditor") => default_editor = Some(as_bool(val, ctx)?),
            Some("ignoreBeatmapSkins") => ignore_beatmap = Some(as_bool(val, ctx)?),
            Some("tintSliderBall") => tint = Some(as_bool(val, ctx)?),
            Some("useTaikoSkin") => taiko = Some(as_bool(val, ctx)?),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"useDefaultSkinInEditor\":");
    push_bool(
        out,
        default_editor.ok_or_else(|| missing(ctx, "skin.useDefaultSkinInEditor"))?,
    );
    out.extend_from_slice(b",\"ignoreBeatmapSkins\":");
    push_bool(
        out,
        ignore_beatmap.ok_or_else(|| missing(ctx, "skin.ignoreBeatmapSkins"))?,
    );
    out.extend_from_slice(b",\"tintSliderBall\":");
    push_bool(
        out,
        tint.ok_or_else(|| missing(ctx, "skin.tintSliderBall"))?,
    );
    out.extend_from_slice(b",\"useTaikoSkin\":");
    push_bool(out, taiko.ok_or_else(|| missing(ctx, "skin.useTaikoSkin"))?);
    out.extend_from_slice(b",\"name\":");
    push_str(out, skin_name);
    out.push(b'}');
    Ok(())
}

fn write_keybinds(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut osu = None;
    let mut fruits = None;
    let mut taiko = None;
    let mut quick_retry = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("osu") => osu = Some(val),
            Some("fruits") => fruits = Some(val),
            Some("taiko") => taiko = Some(val),
            Some("quickRetry") => quick_retry = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    let (mut ok1, mut ok2, mut osmoke) = (None, None, None);
    for (k, val) in as_map(osu.ok_or_else(|| missing(ctx, "keybinds.osu"))?, ctx)? {
        match k.as_text() {
            Some("k1") => ok1 = Some(as_str(val, ctx)?),
            Some("k2") => ok2 = Some(as_str(val, ctx)?),
            Some("smokeKey") => osmoke = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    let (mut fk1, mut fk2, mut fdash) = (None, None, None);
    for (k, val) in as_map(fruits.ok_or_else(|| missing(ctx, "keybinds.fruits"))?, ctx)? {
        match k.as_text() {
            Some("k1") => fk1 = Some(as_str(val, ctx)?),
            Some("k2") => fk2 = Some(as_str(val, ctx)?),
            Some("Dash") => fdash = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    let (mut til, mut tir, mut tol, mut tor) = (None, None, None, None);
    for (k, val) in as_map(taiko.ok_or_else(|| missing(ctx, "keybinds.taiko"))?, ctx)? {
        match k.as_text() {
            Some("innerLeft") => til = Some(as_str(val, ctx)?),
            Some("innerRight") => tir = Some(as_str(val, ctx)?),
            Some("outerLeft") => tol = Some(as_str(val, ctx)?),
            Some("outerRight") => tor = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"osu\":{\"k1\":");
    push_str(out, ok1.ok_or_else(|| missing(ctx, "keybinds.osu.k1"))?);
    out.extend_from_slice(b",\"k2\":");
    push_str(out, ok2.ok_or_else(|| missing(ctx, "keybinds.osu.k2"))?);
    out.extend_from_slice(b",\"smokeKey\":");
    push_str(
        out,
        osmoke.ok_or_else(|| missing(ctx, "keybinds.osu.smokeKey"))?,
    );
    out.extend_from_slice(b"},\"fruits\":{\"k1\":");
    push_str(out, fk1.ok_or_else(|| missing(ctx, "keybinds.fruits.k1"))?);
    out.extend_from_slice(b",\"k2\":");
    push_str(out, fk2.ok_or_else(|| missing(ctx, "keybinds.fruits.k2"))?);
    out.extend_from_slice(b",\"Dash\":");
    push_str(
        out,
        fdash.ok_or_else(|| missing(ctx, "keybinds.fruits.Dash"))?,
    );
    out.extend_from_slice(b"},\"taiko\":{\"innerLeft\":");
    push_str(
        out,
        til.ok_or_else(|| missing(ctx, "keybinds.taiko.innerLeft"))?,
    );
    out.extend_from_slice(b",\"innerRight\":");
    push_str(
        out,
        tir.ok_or_else(|| missing(ctx, "keybinds.taiko.innerRight"))?,
    );
    out.extend_from_slice(b",\"outerLeft\":");
    push_str(
        out,
        tol.ok_or_else(|| missing(ctx, "keybinds.taiko.outerLeft"))?,
    );
    out.extend_from_slice(b",\"outerRight\":");
    push_str(
        out,
        tor.ok_or_else(|| missing(ctx, "keybinds.taiko.outerRight"))?,
    );
    out.extend_from_slice(b"},\"quickRetry\":");
    push_str(
        out,
        quick_retry.ok_or_else(|| missing(ctx, "keybinds.quickRetry"))?,
    );
    out.push(b'}');
    Ok(())
}

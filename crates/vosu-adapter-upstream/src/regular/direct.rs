//! Single-pass direct CBOR -> JSON wire writer for unfiltered regular v2.
//!
//! Borrows every string and array straight from `CapturedSelection` and writes
//! with infallible `Vec<u8>` primitives so unfiltered HTTP/WS broadcasts
//! materialize zero intermediate `dto::Response` strings or vectors.
use super::{
    direct_extra::*,
    direct_settings::write_settings,
    parse::*,
    parse_settings::*,
    slots::{RegularSlots, Slot},
};
use crate::{
    Error,
    dto::*,
    view::{
        Identity, as_array, as_bool, as_f64, as_map, as_opt_f64, as_opt_i64, as_opt_str, as_str,
        missing, write_cbor_json,
    },
};
use bytes::Bytes;
use ciborium::Value;
use vosu_adapter_support::json::{push_bool, push_js_number, push_str, write_i64};
use vosu_osu_model::source::TourneyAvailability;
use vosu_surface::state::CapturedSelection;

#[inline]
pub(super) fn push_number_name(out: &mut Vec<u8>, nn: NumberNameRef<'_>) {
    if let Some(number) = nn.number {
        out.extend_from_slice(b"{\"number\":");
        write_i64(out, number);
        out.extend_from_slice(b",\"name\":");
    } else {
        out.extend_from_slice(b"{\"name\":");
    }
    push_str(out, nn.name);
    out.push(b'}');
}

#[inline]
fn push_opt_field_sep(out: &mut Vec<u8>, open_len: usize, key: &[u8]) {
    out.push(if out.len() == open_len { b'{' } else { b',' });
    out.extend_from_slice(key);
}

pub(crate) fn encode_capture(capture: &CapturedSelection) -> Result<Bytes, Error> {
    let session = Identity::validate_session(capture)?;
    let lazer = session.variant == "lazer";
    let slots = RegularSlots::index(capture);

    let availability = parse_tourney_availability(
        slots.get(Slot::TourneyAvailability)?,
        "/tourneyAvailability",
    )?;
    let tourney_val = match availability {
        TourneyAvailability::Pending => None,
        TourneyAvailability::MissingScore => return Err(Error::MissingTourneyScore),
        TourneyAvailability::Ready => {
            let val = slots.get(Slot::Tourney)?;
            (!matches!(val, Value::Null)).then_some(val)
        }
    };

    let state = parse_number_name(slots.get(Slot::SessionState)?, "/session/state")?;
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
            .filter(|s| s.owner == session.owner && s.owner_instance == session.owner_instance)
            .any(|s| s.foreground == Some(true))
    });
    let server =
        as_opt_str(slots.get(Slot::SessionServer)?, "/session/server")?.unwrap_or("ppy.sh");
    let play_time = as_f64(slots.get(Slot::SessionPlayTime)?, "/session/playTime")?;
    let ranked_play_val = {
        let val = slots.get(Slot::RankedPlay)?;
        (!matches!(val, Value::Null)).then_some(val)
    };
    let skin_name = as_opt_str(slots.get(Slot::SkinName)?, "vosu.skin/name")?.unwrap_or("");
    let settings_val = slots.get(Slot::Settings)?;
    let profile_val = slots.get(Slot::Profile)?;
    let beatmap_val = slots.get(Slot::Beatmap)?;
    let perf_val = slots.get(Slot::Performance)?;
    let (perf_acc, perf_series, perf_xaxis) = parse_performance_ref(perf_val, "/performance")?;
    let hit_errors_val = as_array(slots.get(Slot::PlayHitErrorsAll)?, "/play/hitErrors/all")?;
    let leaderboard_val = as_array(slots.get(Slot::Leaderboard)?, "/leaderboard")?;
    let result_val = slots.get(Slot::Result)?;
    let paths = parse_paths(slots.get(Slot::InternalPaths)?, lazer, "/internal/paths")?;
    let room_val = {
        let val = slots.get(Slot::Room)?;
        (!matches!(val, Value::Null)).then_some(val)
    };

    let capacity = 4096
        + hit_errors_val.len() * 8
        + leaderboard_val.len() * 256
        + perf_xaxis.len() * 8
        + perf_series.len() * 128
        + if tourney_val.is_some() { 256 } else { 0 };
    let mut out = Vec::with_capacity(capacity);

    out.extend_from_slice(b"{\"game\":");
    push_game(&mut out, focused, paused);
    out.extend_from_slice(b",\"client\":");
    push_str(&mut out, &session.variant);
    out.extend_from_slice(b",\"server\":");
    push_str(&mut out, server);
    out.extend_from_slice(b",\"state\":");
    push_number_name(&mut out, state);
    out.extend_from_slice(b",\"session\":{\"playTime\":");
    push_js_number(&mut out, play_time);
    out.extend_from_slice(b",\"playCount\":0}");
    if let Some(rp) = ranked_play_val {
        out.extend_from_slice(b",\"rankedPlay\":");
        write_ranked_play(&mut out, rp, "/rankedPlay")?;
    }
    out.extend_from_slice(b",\"settings\":");
    write_settings(&mut out, settings_val, skin_name, "/settings")?;
    out.extend_from_slice(b",\"profile\":");
    write_profile(&mut out, profile_val, "/profile")?;
    out.extend_from_slice(b",\"beatmap\":");
    let beatmap_time = write_beatmap(&mut out, beatmap_val, "/beatmap")?;
    out.extend_from_slice(b",\"play\":");
    write_top_play(
        &mut out,
        &slots,
        hit_errors_val,
        perf_acc.acc_100,
        state.number.unwrap_or_default(),
        beatmap_time.live,
        beatmap_time.first_object,
    )?;
    out.extend_from_slice(b",\"leaderboard\":");
    write_leaderboard(&mut out, leaderboard_val, "/leaderboard")?;
    out.extend_from_slice(b",\"performance\":");
    write_performance(&mut out, &perf_acc, perf_series, perf_xaxis, "/performance")?;
    out.extend_from_slice(b",\"resultsScreen\":");
    write_results(&mut out, result_val, "/result")?;
    out.extend_from_slice(b",\"folders\":");
    push_folders(&mut out, &paths);
    out.extend_from_slice(b",\"files\":");
    push_files(&mut out, &paths.current);
    out.extend_from_slice(b",\"directPath\":");
    push_direct_path(&mut out, &paths);
    if let Some(t) = tourney_val {
        out.extend_from_slice(b",\"tourney\":");
        write_tourney(&mut out, t, lazer, "/tourney")?;
    }
    if let Some(r) = room_val {
        out.extend_from_slice(b",\"room\":");
        write_room(&mut out, r, "/room")?;
    }
    out.push(b'}');
    Ok(Bytes::from(out))
}

#[inline]
fn push_game(out: &mut Vec<u8>, focused: Option<bool>, paused: Option<bool>) {
    let open = out.len();
    if let Some(f) = focused {
        push_opt_field_sep(out, open, b"\"focused\":");
        push_bool(out, f);
    }
    if let Some(p) = paused {
        push_opt_field_sep(out, open, b"\"paused\":");
        push_bool(out, p);
    }
    if out.len() == open {
        out.extend_from_slice(b"{}");
    } else {
        out.push(b'}');
    }
}

pub(super) fn write_f64_cbor_array(
    out: &mut Vec<u8>,
    items: &[Value],
    ctx: &str,
) -> Result<(), Error> {
    if items.is_empty() {
        out.extend_from_slice(b"[]");
        return Ok(());
    }
    out.push(b'[');
    for (i, item) in items.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        push_js_number(out, as_f64(item, ctx)?);
    }
    out.push(b']');
    Ok(())
}

pub(super) fn write_mods_cbor(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let (checksum, number, name, items, rate) = parse_mods_ref(v, ctx)?;
    out.extend_from_slice(b"{\"checksum\":");
    push_str(out, checksum);
    out.extend_from_slice(b",\"number\":");
    write_i64(out, number);
    out.extend_from_slice(b",\"name\":");
    push_str(out, name);
    out.extend_from_slice(b",\"array\":[");
    for (i, item) in items.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        let mut acronym = None;
        let mut settings = None;
        for (mk, mv) in as_map(item, ctx)? {
            match mk.as_text() {
                Some("acronym") => acronym = Some(as_str(mv, ctx)?),
                Some("settings") if !matches!(mv, Value::Null) => settings = Some(mv),
                _ => {}
            }
        }
        let acronym = acronym.ok_or_else(|| missing(ctx, "acronym"))?;
        out.extend_from_slice(b"{\"acronym\":");
        push_str(out, acronym);
        if let Some(settings) = settings {
            let _ = as_map(settings, ctx)?;
            out.extend_from_slice(b",\"settings\":");
            write_cbor_json(out, settings, ctx)?;
        }
        out.push(b'}');
    }
    out.extend_from_slice(b"],\"rate\":");
    push_js_number(out, rate);
    out.push(b'}');
    Ok(())
}

fn write_profile(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
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
            Some("userStatus") => user_status = parse_opt_number_name(val, ctx)?,
            Some("banchoStatus") => bancho_status = parse_opt_number_name(val, ctx)?,
            Some("id") => id = as_opt_i64(val, ctx)?,
            Some("name") => name = as_opt_str(val, ctx)?,
            Some("mode") => mode = parse_opt_number_name(val, ctx)?,
            Some("rankedScore") => ranked_score = as_opt_f64(val, ctx)?,
            Some("level") => level = as_opt_f64(val, ctx)?,
            Some("accuracy") => accuracy = as_opt_f64(val, ctx)?,
            Some("pp") => pp = as_opt_f64(val, ctx)?,
            Some("playCount") => play_count = as_opt_i64(val, ctx)?,
            Some("globalRank") => global_rank = as_opt_i64(val, ctx)?,
            Some("countryCode") => country_code = parse_opt_number_name(val, ctx)?,
            Some("backgroundColour") => background_colour = as_opt_str(val, ctx)?,
            Some("matchmaking") if !matches!(val, Value::Null) => {
                mm = Some(parse_matchmaking(val, ctx)?);
            }
            _ => {}
        }
    }
    let open = out.len();
    if let Some(v) = user_status {
        push_opt_field_sep(out, open, b"\"userStatus\":");
        push_number_name(out, v);
    }
    if let Some(v) = bancho_status {
        push_opt_field_sep(out, open, b"\"banchoStatus\":");
        push_number_name(out, v);
    }
    if let Some(v) = id {
        push_opt_field_sep(out, open, b"\"id\":");
        write_i64(out, v);
    }
    if let Some(v) = name {
        push_opt_field_sep(out, open, b"\"name\":");
        push_str(out, v);
    }
    if let Some(v) = mode {
        push_opt_field_sep(out, open, b"\"mode\":");
        push_number_name(out, v);
    }
    if let Some(v) = ranked_score {
        push_opt_field_sep(out, open, b"\"rankedScore\":");
        push_js_number(out, v);
    }
    if let Some(v) = level {
        push_opt_field_sep(out, open, b"\"level\":");
        push_js_number(out, v);
    }
    if let Some(v) = accuracy {
        push_opt_field_sep(out, open, b"\"accuracy\":");
        push_js_number(out, v);
    }
    if let Some(v) = pp {
        push_opt_field_sep(out, open, b"\"pp\":");
        push_js_number(out, v);
    }
    if let Some(v) = play_count {
        push_opt_field_sep(out, open, b"\"playCount\":");
        write_i64(out, v);
    }
    if let Some(v) = global_rank {
        push_opt_field_sep(out, open, b"\"globalRank\":");
        write_i64(out, v);
    }
    if let Some(v) = country_code {
        push_opt_field_sep(out, open, b"\"countryCode\":");
        push_number_name(out, v);
    }
    if let Some(v) = background_colour {
        push_opt_field_sep(out, open, b"\"backgroundColour\":");
        push_str(out, v);
    }
    if let Some(m) = mm {
        push_opt_field_sep(out, open, b"\"matchmaking\":{\"rating\":");
        push_js_number(out, m.rating);
        out.extend_from_slice(b",\"rank\":");
        if let Some(r) = m.rank {
            write_i64(out, r);
        } else {
            out.extend_from_slice(b"null");
        }
        out.extend_from_slice(b",\"plays\":");
        write_i64(out, m.plays);
        out.extend_from_slice(b",\"wins\":");
        write_i64(out, m.wins);
        out.extend_from_slice(b",\"isProvisional\":");
        push_bool(out, m.is_provisional);
        out.push(b'}');
    } else if id.is_some() {
        push_opt_field_sep(out, open, b"\"matchmaking\":null");
    }
    if out.len() == open {
        out.extend_from_slice(b"{}");
    } else {
        out.push(b'}');
    }
    Ok(())
}

pub(super) fn push_stats(out: &mut Vec<u8>, s: &Stats) {
    out.extend_from_slice(b"{\"stars\":{\"live\":");
    push_js_number(out, s.stars.live);
    for (key, val) in [
        (&b",\"aim\":"[..], s.stars.aim),
        (b",\"speed\":", s.stars.speed),
        (b",\"flashlight\":", s.stars.flashlight),
        (b",\"sliderFactor\":", s.stars.slider_factor),
        (b",\"stamina\":", s.stars.stamina),
        (b",\"rhythm\":", s.stars.rhythm),
        (b",\"color\":", s.stars.color),
        (b",\"reading\":", s.stars.reading),
        (b",\"hitWindow\":", s.stars.hit_window),
    ] {
        if let Some(v) = val {
            out.extend_from_slice(key);
            push_js_number(out, v);
        }
    }
    out.extend_from_slice(b",\"total\":");
    push_js_number(out, s.stars.total);
    out.extend_from_slice(b"},\"ar\":{\"original\":");
    push_js_number(out, s.ar.original);
    out.extend_from_slice(b",\"converted\":");
    push_js_number(out, s.ar.converted);
    out.extend_from_slice(b"},\"cs\":{\"original\":");
    push_js_number(out, s.cs.original);
    out.extend_from_slice(b",\"converted\":");
    push_js_number(out, s.cs.converted);
    out.extend_from_slice(b"},\"od\":{\"original\":");
    push_js_number(out, s.od.original);
    out.extend_from_slice(b",\"converted\":");
    push_js_number(out, s.od.converted);
    out.extend_from_slice(b"},\"hp\":{\"original\":");
    push_js_number(out, s.hp.original);
    out.extend_from_slice(b",\"converted\":");
    push_js_number(out, s.hp.converted);
    out.extend_from_slice(b"},\"bpm\":{\"realtime\":");
    push_js_number(out, s.bpm.realtime);
    out.extend_from_slice(b",\"common\":");
    push_js_number(out, s.bpm.common);
    out.extend_from_slice(b",\"min\":");
    push_js_number(out, s.bpm.min);
    out.extend_from_slice(b",\"max\":");
    push_js_number(out, s.bpm.max);
    out.extend_from_slice(b"},\"objects\":{\"circles\":");
    write_i64(out, s.objects.circles);
    out.extend_from_slice(b",\"sliders\":");
    write_i64(out, s.objects.sliders);
    out.extend_from_slice(b",\"spinners\":");
    write_i64(out, s.objects.spinners);
    out.extend_from_slice(b",\"holds\":");
    write_i64(out, s.objects.holds);
    out.extend_from_slice(b",\"total\":");
    write_i64(out, s.objects.total);
    out.extend_from_slice(b"},\"hitWindow\":");
    let open = out.len();
    for (key, val) in [
        (&b"\"miss\":"[..], s.hit_window.miss),
        (b"\"meh\":", s.hit_window.meh),
        (b"\"ok\":", s.hit_window.ok),
        (b"\"good\":", s.hit_window.good),
        (b"\"great\":", s.hit_window.great),
        (b"\"perfect\":", s.hit_window.perfect),
    ] {
        if let Some(v) = val {
            push_opt_field_sep(out, open, key);
            push_js_number(out, v);
        }
    }
    if out.len() == open {
        out.extend_from_slice(b"{}");
    } else {
        out.push(b'}');
    }
    out.extend_from_slice(b",\"maxCombo\":");
    write_i64(out, s.max_combo);
    out.push(b'}');
}

fn write_beatmap(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<BeatmapTime, Error> {
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
            Some("status") => status = Some(parse_beatmap_status(val, ctx)?),
            Some("checksum") => checksum = as_opt_str(val, ctx)?,
            Some("id") => id = as_opt_i64(val, ctx)?,
            Some("set") => set = as_opt_i64(val, ctx)?,
            Some("mode") => mode = Some(parse_number_name(val, ctx)?),
            Some("artist") => artist = as_opt_str(val, ctx)?,
            Some("artistUnicode") => artist_unicode = as_opt_str(val, ctx)?,
            Some("title") => title = as_opt_str(val, ctx)?,
            Some("titleUnicode") => title_unicode = as_opt_str(val, ctx)?,
            Some("mapper") => mapper = as_opt_str(val, ctx)?,
            Some("version") => version = as_opt_str(val, ctx)?,
            Some("source") => source = Some(as_str(val, ctx)?),
            Some("tags") => tags = Some(as_str(val, ctx)?),
            Some("stats") => stats = Some(val),
            _ => {}
        }
    }
    let time = time.ok_or_else(|| missing(ctx, "time"))?;
    let status = status.ok_or_else(|| missing(ctx, "status"))?;
    let stats = parse_stats(stats.ok_or_else(|| missing(ctx, "stats"))?, ctx)?;

    out.extend_from_slice(b"{\"isKiai\":");
    push_bool(out, is_kiai.ok_or_else(|| missing(ctx, "isKiai"))?);
    out.extend_from_slice(b",\"isBreak\":");
    push_bool(out, is_break.ok_or_else(|| missing(ctx, "isBreak"))?);
    out.extend_from_slice(b",\"isConvert\":");
    push_bool(out, is_convert.ok_or_else(|| missing(ctx, "isConvert"))?);
    out.extend_from_slice(b",\"time\":{\"live\":");
    push_js_number(out, time.live);
    out.extend_from_slice(b",\"firstObject\":");
    write_i64(out, time.first_object);
    out.extend_from_slice(b",\"lastObject\":");
    push_js_number(out, time.last_object);
    if let Some(mp3) = time.mp3_length {
        out.extend_from_slice(b",\"mp3Length\":");
        push_js_number(out, mp3);
    }
    out.extend_from_slice(b"},\"status\":");
    if let Some(num) = status.number {
        out.extend_from_slice(b"{\"number\":");
        push_js_number(out, num);
        out.extend_from_slice(b",\"name\":");
    } else {
        out.extend_from_slice(b"{\"name\":");
    }
    push_str(out, status.name);
    out.push(b'}');
    if let Some(v) = checksum {
        out.extend_from_slice(b",\"checksum\":");
        push_str(out, v);
    }
    if let Some(v) = id {
        out.extend_from_slice(b",\"id\":");
        write_i64(out, v);
    }
    if let Some(v) = set {
        out.extend_from_slice(b",\"set\":");
        write_i64(out, v);
    }
    out.extend_from_slice(b",\"mode\":");
    push_number_name(out, mode.ok_or_else(|| missing(ctx, "mode"))?);
    if let Some(v) = artist {
        out.extend_from_slice(b",\"artist\":");
        push_str(out, v);
    }
    if let Some(v) = artist_unicode {
        out.extend_from_slice(b",\"artistUnicode\":");
        push_str(out, v);
    }
    if let Some(v) = title {
        out.extend_from_slice(b",\"title\":");
        push_str(out, v);
    }
    if let Some(v) = title_unicode {
        out.extend_from_slice(b",\"titleUnicode\":");
        push_str(out, v);
    }
    if let Some(v) = mapper {
        out.extend_from_slice(b",\"mapper\":");
        push_str(out, v);
    }
    if let Some(v) = version {
        out.extend_from_slice(b",\"version\":");
        push_str(out, v);
    }
    out.extend_from_slice(b",\"source\":");
    push_str(out, source.ok_or_else(|| missing(ctx, "source"))?);
    out.extend_from_slice(b",\"tags\":");
    push_str(out, tags.ok_or_else(|| missing(ctx, "tags"))?);
    out.extend_from_slice(b",\"stats\":");
    push_stats(out, &stats);
    out.push(b'}');
    Ok(time)
}

pub(super) struct PlayFields<'a> {
    pub failed: bool,
    pub player_name: &'a str,
    pub mode: NumberNameRef<'a>,
    pub score: f64,
    pub accuracy: f64,
    pub health_bar: HealthBar,
    pub hits: PlayHits,
    pub hit_errors: &'a [Value],
    pub combo: Combo,
    pub mods_val: &'a Value,
    pub rank_cur: &'a str,
    pub rank_max: &'a str,
    pub pp: Pp,
    pub unstable_rate: f64,
}

fn write_top_play(
    out: &mut Vec<u8>,
    slots: &RegularSlots<'_>,
    hit_errors: &[Value],
    perfect_pp: f64,
    state_number: i64,
    live_time: f64,
    first_object: i64,
) -> Result<(), Error> {
    let failed = as_bool(slots.get(Slot::PlayFailed)?, "/play/failed")?;
    let player_name = as_str(slots.get(Slot::PlayPlayerName)?, "/play/playerName")?;
    let mode = parse_number_name(slots.get(Slot::PlayMode)?, "/play/mode")?;
    let score = as_f64(slots.get(Slot::PlayScore)?, "/play/score")?;
    let accuracy = as_f64(
        slots.get(Slot::PlayAccuracyPercent)?,
        "/play/accuracyPercent",
    )?;
    let raw = parse_health_raw(slots.get(Slot::PlayHealthRaw)?, "/play/healthRaw")?;
    let health_bar = HealthBar {
        normal: (raw.normal / 200.0) * 100.0,
        smooth: (raw.smooth / 200.0) * 100.0,
    };
    let h = parse_judgements(slots.get(Slot::PlayJudgements)?, "/play/judgements")?;
    let s = parse_statistics(slots.get(Slot::PlayStatistics)?, "/play/statistics")?;
    let hits = PlayHits::Normal(Hits {
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
    });
    let combo = parse_combo(slots.get(Slot::PlayCombo)?, "/play/combo")?;
    let mods_val = slots.get(Slot::PlayMods)?;
    let (rank_cur, rank_max) = parse_rank_ref(slots.get(Slot::PlayRank)?, "/play/rank")?;
    let mut pp = parse_pp(slots.get(Slot::PlayPp)?, "/play/pp")?;
    pp.fc = vosu_adapter_support::full_combo_pp(
        pp.fc,
        perfect_pp,
        state_number,
        live_time,
        first_object,
    );
    pp.detailed.fc.total = pp.fc;
    let unstable_rate = as_f64(slots.get(Slot::PlayUnstableRate)?, "/play/unstableRate")?;

    write_play_fields(
        out,
        &PlayFields {
            failed,
            player_name,
            mode,
            score,
            accuracy,
            health_bar,
            hits,
            hit_errors,
            combo,
            mods_val,
            rank_cur,
            rank_max,
            pp,
            unstable_rate,
        },
        "/play",
    )
}

pub(super) fn write_play_fields(
    out: &mut Vec<u8>,
    f: &PlayFields<'_>,
    ctx: &str,
) -> Result<(), Error> {
    out.extend_from_slice(b"{\"failed\":");
    push_bool(out, f.failed);
    out.extend_from_slice(b",\"playerName\":");
    push_str(out, f.player_name);
    out.extend_from_slice(b",\"mode\":");
    push_number_name(out, f.mode);
    out.extend_from_slice(b",\"score\":");
    push_js_number(out, f.score);
    out.extend_from_slice(b",\"accuracy\":");
    push_js_number(out, f.accuracy);
    out.extend_from_slice(b",\"healthBar\":{\"normal\":");
    push_js_number(out, f.health_bar.normal);
    out.extend_from_slice(b",\"smooth\":");
    push_js_number(out, f.health_bar.smooth);
    out.extend_from_slice(b"},\"hits\":");
    push_play_hits(out, &f.hits);
    out.extend_from_slice(b",\"hitErrorArray\":");
    write_f64_cbor_array(out, f.hit_errors, ctx)?;
    out.extend_from_slice(b",\"combo\":{\"current\":");
    write_i64(out, f.combo.current);
    out.extend_from_slice(b",\"max\":");
    write_i64(out, f.combo.max);
    out.extend_from_slice(b"},\"mods\":");
    write_mods_cbor(out, f.mods_val, ctx)?;
    out.extend_from_slice(b",\"rank\":{\"current\":");
    push_str(out, f.rank_cur);
    out.extend_from_slice(b",\"maxThisPlay\":");
    push_str(out, f.rank_max);
    out.extend_from_slice(b"},\"pp\":{\"current\":");
    push_js_number(out, f.pp.current);
    out.extend_from_slice(b",\"fc\":");
    push_js_number(out, f.pp.fc);
    out.extend_from_slice(b",\"maxAchieved\":");
    push_js_number(out, f.pp.max_achieved);
    out.extend_from_slice(b",\"maxAchievable\":");
    push_js_number(out, f.pp.max_achievable);
    out.extend_from_slice(b",\"detailed\":{\"current\":");
    push_detailed_pp_attrs(out, &f.pp.detailed.current);
    out.extend_from_slice(b",\"fc\":");
    push_detailed_pp_attrs(out, &f.pp.detailed.fc);
    out.extend_from_slice(b"}},\"unstableRate\":");
    push_js_number(out, f.unstable_rate);
    out.push(b'}');
    Ok(())
}

fn push_play_hits(out: &mut Vec<u8>, hits: &PlayHits) {
    match hits {
        PlayHits::Normal(h) => {
            out.extend_from_slice(b"{\"0\":");
            write_i64(out, h.h0);
            out.extend_from_slice(b",\"50\":");
            write_i64(out, h.h50);
            out.extend_from_slice(b",\"100\":");
            write_i64(out, h.h100);
            out.extend_from_slice(b",\"300\":");
            write_i64(out, h.h300);
            out.extend_from_slice(b",\"geki\":");
            write_i64(out, h.geki);
            out.extend_from_slice(b",\"katu\":");
            write_i64(out, h.katu);
            out.extend_from_slice(b",\"sliderBreaks\":");
            write_i64(out, h.slider_breaks);
            out.extend_from_slice(b",\"sliderEndHits\":");
            write_i64(out, h.slider_end_hits);
            out.extend_from_slice(b",\"smallTickHits\":");
            write_i64(out, h.small_tick_hits);
            out.extend_from_slice(b",\"largeTickHits\":");
            write_i64(out, h.large_tick_hits);
            out.push(b'}');
        }
        PlayHits::Lazer(h) => {
            out.extend_from_slice(b"{\"0\":");
            write_i64(out, h.h0);
            out.extend_from_slice(b",\"50\":");
            write_i64(out, h.h50);
            out.extend_from_slice(b",\"100\":");
            write_i64(out, h.h100);
            out.extend_from_slice(b",\"300\":");
            write_i64(out, h.h300);
            out.extend_from_slice(b",\"geki\":");
            write_i64(out, h.geki);
            out.extend_from_slice(b",\"katu\":");
            write_i64(out, h.katu);
            out.extend_from_slice(b",\"sliderEndHits\":");
            write_i64(out, h.slider_end_hits);
            out.extend_from_slice(b",\"smallTickHits\":");
            write_i64(out, h.small_tick_hits);
            out.extend_from_slice(b",\"largeTickHits\":");
            write_i64(out, h.large_tick_hits);
            out.extend_from_slice(b",\"sliderBreaks\":");
            write_i64(out, h.slider_breaks);
            out.push(b'}');
        }
    }
}

#[inline]
fn push_detailed_pp_attrs(out: &mut Vec<u8>, a: &DetailedPpAttributes) {
    out.extend_from_slice(b"{\"aim\":");
    push_js_number(out, a.aim);
    out.extend_from_slice(b",\"speed\":");
    push_js_number(out, a.speed);
    out.extend_from_slice(b",\"accuracy\":");
    push_js_number(out, a.accuracy);
    out.extend_from_slice(b",\"difficulty\":");
    push_js_number(out, a.difficulty);
    out.extend_from_slice(b",\"flashlight\":");
    push_js_number(out, a.flashlight);
    out.extend_from_slice(b",\"total\":");
    push_js_number(out, a.total);
    out.push(b'}');
}

fn push_folders(out: &mut Vec<u8>, paths: &PathsRef<'_>) {
    out.extend_from_slice(b"{\"game\":");
    push_str(out, paths.game);
    out.extend_from_slice(b",\"skin\":");
    push_str(out, paths.skin);
    out.extend_from_slice(b",\"songs\":");
    push_str(out, paths.songs);
    if let Some(bm) = paths.current.beatmap_folder {
        out.extend_from_slice(b",\"beatmap\":");
        push_str(out, bm);
    }
    out.push(b'}');
}

fn push_files(out: &mut Vec<u8>, c: &CurrentPathsRef<'_>) {
    let open = out.len();
    if let Some(v) = c.beatmap_file {
        push_opt_field_sep(out, open, b"\"beatmap\":");
        push_str(out, v);
    }
    if let Some(v) = c.background {
        push_opt_field_sep(out, open, b"\"background\":");
        push_str(out, v);
    }
    if let Some(v) = c.audio {
        push_opt_field_sep(out, open, b"\"audio\":");
        push_str(out, v);
    }
    if out.len() == open {
        out.extend_from_slice(b"{}");
    } else {
        out.push(b'}');
    }
}

fn push_direct_path(out: &mut Vec<u8>, paths: &PathsRef<'_>) {
    let c = &paths.current;
    out.extend_from_slice(b"{\"beatmapFile\":");
    crate::paths::push_direct_join(out, c.beatmap_folder, c.beatmap_file);
    out.extend_from_slice(b",\"beatmapBackground\":");
    crate::paths::push_direct_join(out, c.beatmap_folder, c.background);
    out.extend_from_slice(b",\"beatmapAudio\":");
    crate::paths::push_direct_join(out, c.beatmap_folder, c.audio);
    if let Some(bm) = c.beatmap_folder {
        out.extend_from_slice(b",\"beatmapFolder\":");
        push_str(out, bm);
    }
    out.extend_from_slice(b",\"skinFolder\":");
    push_str(out, paths.skin);
    out.push(b'}');
}

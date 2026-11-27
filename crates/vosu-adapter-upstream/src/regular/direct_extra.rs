//! Direct CBOR -> JSON writers for rankedPlay, leaderboard, performance, results, tourney, and room.
use super::{
    direct::{
        PlayFields, push_number_name, push_stats, write_f64_cbor_array, write_mods_cbor,
        write_play_fields,
    },
    parse::*,
    parse_settings::*,
};
use crate::{
    Error,
    dto::*,
    view::{as_array, as_bool, as_f64, as_i64, as_map, as_opt_str, as_str, missing},
};
use ciborium::Value;
use vosu_adapter_support::json::{push_bool, push_js_number, push_str, write_i64};

pub(super) fn write_ranked_play(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut stage = None;
    let mut current_round = None;
    let mut star_rating = None;
    let mut damage_multiplier = None;
    let mut active_user_id = None;
    let mut winning_user_id = None;
    let mut users = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("stage") => stage = Some(parse_ranked_play_stage(val, ctx)?),
            Some("currentRound") => current_round = Some(as_i64(val, ctx)?),
            Some("starRating") => star_rating = Some(as_f64(val, ctx)?),
            Some("damageMultiplier") => damage_multiplier = Some(as_f64(val, ctx)?),
            Some("activeUserId") => active_user_id = Some(as_i64(val, ctx)?),
            Some("winningUserId") => winning_user_id = Some(as_i64(val, ctx)?),
            Some("users") => users = Some(as_array(val, ctx)?),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"stage\":");
    write_i64(
        out,
        i64::from(stage.ok_or_else(|| missing(ctx, "stage"))?.value()),
    );
    out.extend_from_slice(b",\"currentRound\":");
    write_i64(
        out,
        current_round.ok_or_else(|| missing(ctx, "currentRound"))?,
    );
    out.extend_from_slice(b",\"starRating\":");
    push_js_number(out, star_rating.ok_or_else(|| missing(ctx, "starRating"))?);
    out.extend_from_slice(b",\"damageMultiplier\":");
    push_js_number(
        out,
        damage_multiplier.ok_or_else(|| missing(ctx, "damageMultiplier"))?,
    );
    out.extend_from_slice(b",\"activeUserId\":");
    write_i64(
        out,
        active_user_id.ok_or_else(|| missing(ctx, "activeUserId"))?,
    );
    out.extend_from_slice(b",\"winningUserId\":");
    write_i64(
        out,
        winning_user_id.ok_or_else(|| missing(ctx, "winningUserId"))?,
    );
    out.extend_from_slice(b",\"users\":[");
    for (i, item) in users
        .ok_or_else(|| missing(ctx, "users"))?
        .iter()
        .enumerate()
    {
        if i != 0 {
            out.push(b',');
        }
        let mut id = None;
        let mut rating = None;
        let mut life = None;
        let mut rounds_won = None;
        let mut dm = None;
        for (uk, uv) in as_map(item, ctx)? {
            match uk.as_text() {
                Some("id") => id = Some(as_i64(uv, ctx)?),
                Some("rating") => rating = Some(as_f64(uv, ctx)?),
                Some("life") => life = Some(as_f64(uv, ctx)?),
                Some("roundsWon") => rounds_won = Some(as_i64(uv, ctx)?),
                Some("damageMultiplier") => dm = Some(as_f64(uv, ctx)?),
                _ => {}
            }
        }
        out.extend_from_slice(b"{\"id\":");
        write_i64(out, id.ok_or_else(|| missing(ctx, "users.id"))?);
        out.extend_from_slice(b",\"rating\":");
        push_js_number(out, rating.ok_or_else(|| missing(ctx, "users.rating"))?);
        out.extend_from_slice(b",\"life\":");
        push_js_number(out, life.ok_or_else(|| missing(ctx, "users.life"))?);
        out.extend_from_slice(b",\"roundsWon\":");
        write_i64(
            out,
            rounds_won.ok_or_else(|| missing(ctx, "users.roundsWon"))?,
        );
        out.extend_from_slice(b",\"damageMultiplier\":");
        push_js_number(
            out,
            dm.ok_or_else(|| missing(ctx, "users.damageMultiplier"))?,
        );
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
    Ok(())
}

pub(super) fn write_leaderboard(
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
        let mut is_failed = None;
        let mut position = None;
        let mut team = None;
        let mut id = None;
        let mut name = None;
        let mut score = None;
        let mut accuracy = None;
        let mut hits = None;
        let mut combo = None;
        let mut mods = None;
        let mut rank = None;
        for (k, val) in as_map(item, ctx)? {
            match k.as_text() {
                Some("isFailed") => is_failed = Some(as_bool(val, ctx)?),
                Some("position") => position = Some(as_i64(val, ctx)?),
                Some("team") => team = Some(as_i64(val, ctx)?),
                Some("id") => id = Some(as_i64(val, ctx)?),
                Some("name") => name = Some(as_str(val, ctx)?),
                Some("score") => score = Some(as_f64(val, ctx)?),
                Some("accuracy") => accuracy = Some(as_f64(val, ctx)?),
                Some("hits" | "statistics") => hits = Some(parse_leaderboard_hits(val, ctx)?),
                Some("combo") => combo = Some(parse_combo(val, ctx)?),
                Some("mods") => mods = Some(val),
                Some("rank") => rank = Some(as_str(val, ctx)?),
                _ => {}
            }
        }
        let hits = hits.ok_or_else(|| missing(ctx, "hits"))?;
        let combo = combo.ok_or_else(|| missing(ctx, "combo"))?;
        out.extend_from_slice(b"{\"isFailed\":");
        push_bool(out, is_failed.ok_or_else(|| missing(ctx, "isFailed"))?);
        out.extend_from_slice(b",\"position\":");
        write_i64(out, position.ok_or_else(|| missing(ctx, "position"))?);
        out.extend_from_slice(b",\"team\":");
        write_i64(out, team.ok_or_else(|| missing(ctx, "team"))?);
        out.extend_from_slice(b",\"id\":");
        write_i64(out, id.ok_or_else(|| missing(ctx, "id"))?);
        out.extend_from_slice(b",\"name\":");
        push_str(out, name.ok_or_else(|| missing(ctx, "name"))?);
        out.extend_from_slice(b",\"score\":");
        push_js_number(out, score.ok_or_else(|| missing(ctx, "score"))?);
        out.extend_from_slice(b",\"accuracy\":");
        push_js_number(out, accuracy.ok_or_else(|| missing(ctx, "accuracy"))?);
        out.extend_from_slice(b",\"hits\":{\"0\":");
        write_i64(out, hits.h0);
        out.extend_from_slice(b",\"50\":");
        write_i64(out, hits.h50);
        out.extend_from_slice(b",\"100\":");
        write_i64(out, hits.h100);
        out.extend_from_slice(b",\"300\":");
        write_i64(out, hits.h300);
        out.extend_from_slice(b",\"geki\":");
        write_i64(out, hits.geki);
        out.extend_from_slice(b",\"katu\":");
        write_i64(out, hits.katu);
        out.extend_from_slice(b"},\"combo\":{\"current\":");
        write_i64(out, combo.current);
        out.extend_from_slice(b",\"max\":");
        write_i64(out, combo.max);
        out.extend_from_slice(b"},\"mods\":");
        write_mods_cbor(out, mods.ok_or_else(|| missing(ctx, "mods"))?, ctx)?;
        out.extend_from_slice(b",\"rank\":");
        push_str(out, rank.ok_or_else(|| missing(ctx, "rank"))?);
        out.push(b'}');
    }
    out.push(b']');
    Ok(())
}

pub(super) fn parse_performance_ref<'a>(
    v: &'a Value,
    ctx: &str,
) -> Result<(Accuracy, &'a [Value], &'a [Value]), Error> {
    let mut accuracy = None;
    let mut series = None;
    let mut xaxis = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("accuracy") => accuracy = Some(parse_accuracy(val, ctx)?),
            Some("graph") => {
                for (gk, gv) in as_map(val, ctx)? {
                    match gk.as_text() {
                        Some("series") => series = Some(as_array(gv, ctx)?),
                        Some("xaxis") => xaxis = Some(as_array(gv, ctx)?),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok((
        accuracy.ok_or_else(|| missing(ctx, "accuracy"))?,
        series.ok_or_else(|| missing(ctx, "graph.series"))?,
        xaxis.ok_or_else(|| missing(ctx, "graph.xaxis"))?,
    ))
}

pub(super) fn write_performance(
    out: &mut Vec<u8>,
    acc: &Accuracy,
    series: &[Value],
    xaxis: &[Value],
    ctx: &str,
) -> Result<(), Error> {
    out.extend_from_slice(b"{\"accuracy\":{\"90\":");
    push_js_number(out, acc.acc_90);
    out.extend_from_slice(b",\"91\":");
    push_js_number(out, acc.acc_91);
    out.extend_from_slice(b",\"92\":");
    push_js_number(out, acc.acc_92);
    out.extend_from_slice(b",\"93\":");
    push_js_number(out, acc.acc_93);
    out.extend_from_slice(b",\"94\":");
    push_js_number(out, acc.acc_94);
    out.extend_from_slice(b",\"95\":");
    push_js_number(out, acc.acc_95);
    out.extend_from_slice(b",\"96\":");
    push_js_number(out, acc.acc_96);
    out.extend_from_slice(b",\"97\":");
    push_js_number(out, acc.acc_97);
    out.extend_from_slice(b",\"98\":");
    push_js_number(out, acc.acc_98);
    out.extend_from_slice(b",\"99\":");
    push_js_number(out, acc.acc_99);
    out.extend_from_slice(b",\"100\":");
    push_js_number(out, acc.acc_100);
    out.extend_from_slice(b"},\"graph\":{\"series\":[");
    for (i, item) in series.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        let mut name = None;
        let mut data = None;
        for (sk, sv) in as_map(item, ctx)? {
            match sk.as_text() {
                Some("name") => name = Some(as_str(sv, ctx)?),
                Some("data") => data = Some(as_array(sv, ctx)?),
                _ => {}
            }
        }
        out.extend_from_slice(b"{\"name\":");
        push_str(out, name.ok_or_else(|| missing(ctx, "series.name"))?);
        out.extend_from_slice(b",\"data\":");
        write_f64_cbor_array(out, data.ok_or_else(|| missing(ctx, "series.data"))?, ctx)?;
        out.push(b'}');
    }
    out.extend_from_slice(b"],\"xaxis\":");
    write_f64_cbor_array(out, xaxis, ctx)?;
    out.extend_from_slice(b"}}");
    Ok(())
}

pub(super) fn write_results(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut score_id = None;
    let mut player_name = None;
    let mut mode = None;
    let mut score = None;
    let mut accuracy = None;
    let mut name = None;
    let mut hits = None;
    let mut mods = None;
    let mut max_combo = None;
    let mut rank = None;
    let mut pp = None;
    let mut created_at = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("scoreId") => score_id = Some(as_f64(val, ctx)?),
            Some("playerName") => player_name = Some(as_str(val, ctx)?),
            Some("mode") => mode = Some(parse_number_name(val, ctx)?),
            Some("score") => score = Some(as_f64(val, ctx)?),
            Some("accuracy") => accuracy = Some(as_f64(val, ctx)?),
            Some("name") => name = Some(as_str(val, ctx)?),
            Some("hits" | "statistics") => hits = Some(parse_results_hits(val, ctx)?),
            Some("mods") => mods = Some(val),
            Some("maxCombo") => max_combo = Some(as_i64(val, ctx)?),
            Some("rank") => rank = Some(as_str(val, ctx)?),
            Some("pp") => pp = Some(parse_results_pp(val, ctx)?),
            Some("createdAt") => created_at = Some(as_str(val, ctx)?),
            _ => {}
        }
    }
    let hits = hits.ok_or_else(|| missing(ctx, "hits"))?;
    let pp = pp.ok_or_else(|| missing(ctx, "pp"))?;
    out.extend_from_slice(b"{\"scoreId\":");
    push_js_number(out, score_id.ok_or_else(|| missing(ctx, "scoreId"))?);
    out.extend_from_slice(b",\"playerName\":");
    push_str(out, player_name.ok_or_else(|| missing(ctx, "playerName"))?);
    out.extend_from_slice(b",\"mode\":");
    push_number_name(out, mode.ok_or_else(|| missing(ctx, "mode"))?);
    out.extend_from_slice(b",\"score\":");
    push_js_number(out, score.ok_or_else(|| missing(ctx, "score"))?);
    out.extend_from_slice(b",\"accuracy\":");
    push_js_number(out, accuracy.ok_or_else(|| missing(ctx, "accuracy"))?);
    out.extend_from_slice(b",\"name\":");
    push_str(out, name.ok_or_else(|| missing(ctx, "name"))?);
    out.extend_from_slice(b",\"hits\":{\"0\":");
    write_i64(out, hits.h0);
    out.extend_from_slice(b",\"50\":");
    write_i64(out, hits.h50);
    out.extend_from_slice(b",\"100\":");
    write_i64(out, hits.h100);
    out.extend_from_slice(b",\"300\":");
    write_i64(out, hits.h300);
    out.extend_from_slice(b",\"geki\":");
    write_i64(out, hits.geki);
    out.extend_from_slice(b",\"katu\":");
    write_i64(out, hits.katu);
    out.extend_from_slice(b",\"sliderEndHits\":");
    write_i64(out, hits.slider_end_hits);
    out.extend_from_slice(b",\"smallTickHits\":");
    write_i64(out, hits.small_tick_hits);
    out.extend_from_slice(b",\"largeTickHits\":");
    write_i64(out, hits.large_tick_hits);
    out.extend_from_slice(b"},\"mods\":");
    write_mods_cbor(out, mods.ok_or_else(|| missing(ctx, "mods"))?, ctx)?;
    out.extend_from_slice(b",\"maxCombo\":");
    write_i64(out, max_combo.ok_or_else(|| missing(ctx, "maxCombo"))?);
    out.extend_from_slice(b",\"rank\":");
    push_str(out, rank.ok_or_else(|| missing(ctx, "rank"))?);
    out.extend_from_slice(b",\"pp\":{\"current\":");
    push_js_number(out, pp.current);
    out.extend_from_slice(b",\"fc\":");
    push_js_number(out, pp.fc);
    out.extend_from_slice(b"},\"createdAt\":");
    push_str(out, created_at.ok_or_else(|| missing(ctx, "createdAt"))?);
    out.push(b'}');
    Ok(())
}

pub(super) fn write_tourney(
    out: &mut Vec<u8>,
    v: &Value,
    lazer: bool,
    ctx: &str,
) -> Result<(), Error> {
    let mut score_visible = None;
    let mut stars_visible = None;
    let mut ipc_state = None;
    let mut best_of = None;
    let mut team = None;
    let mut points = None;
    let mut chat = None;
    let mut total_score = None;
    let mut clients = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("scoreVisible") => score_visible = Some(as_bool(val, ctx)?),
            Some("starsVisible") => stars_visible = Some(as_bool(val, ctx)?),
            Some("ipcState") => ipc_state = Some(as_i64(val, ctx)?),
            Some("bestOf" | "bestOF") => best_of = Some(as_i64(val, ctx)?),
            Some("team") => team = Some(parse_sides_str(val, ctx)?),
            Some("points") => points = Some(parse_sides_i64(val, ctx)?),
            Some("chat") => chat = Some(as_array(val, ctx)?),
            Some("totalScore") => total_score = Some(parse_sides_f64(val, ctx)?),
            Some("clients") => clients = Some(as_array(val, ctx)?),
            _ => {}
        }
    }
    let (team_left, team_right) = team.ok_or_else(|| missing(ctx, "team"))?;
    let points = points.ok_or_else(|| missing(ctx, "points"))?;
    let total_score = total_score.ok_or_else(|| missing(ctx, "totalScore"))?;
    out.extend_from_slice(b"{\"scoreVisible\":");
    push_bool(
        out,
        score_visible.ok_or_else(|| missing(ctx, "scoreVisible"))?,
    );
    out.extend_from_slice(b",\"starsVisible\":");
    push_bool(
        out,
        stars_visible.ok_or_else(|| missing(ctx, "starsVisible"))?,
    );
    out.extend_from_slice(b",\"ipcState\":");
    write_i64(out, ipc_state.ok_or_else(|| missing(ctx, "ipcState"))?);
    out.extend_from_slice(b",\"bestOF\":");
    write_i64(out, best_of.ok_or_else(|| missing(ctx, "bestOf"))?);
    out.extend_from_slice(b",\"team\":{\"left\":");
    push_str(out, team_left);
    out.extend_from_slice(b",\"right\":");
    push_str(out, team_right);
    out.extend_from_slice(b"},\"points\":{\"left\":");
    write_i64(out, points.left);
    out.extend_from_slice(b",\"right\":");
    write_i64(out, points.right);
    out.extend_from_slice(b"},\"chat\":[");
    for (i, item) in chat.ok_or_else(|| missing(ctx, "chat"))?.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        let mut m_team = None;
        let mut timestamp = None;
        let mut name = None;
        let mut message = None;
        for (ck, cv) in as_map(item, ctx)? {
            match ck.as_text() {
                Some("team") => m_team = Some(as_str(cv, ctx)?),
                Some("timestamp") => timestamp = Some(as_str(cv, ctx)?),
                Some("name") => name = Some(as_str(cv, ctx)?),
                Some("message") => message = Some(as_str(cv, ctx)?),
                _ => {}
            }
        }
        let m_team = m_team.ok_or_else(|| missing(ctx, "chat.team"))?;
        let timestamp = timestamp.ok_or_else(|| missing(ctx, "chat.timestamp"))?;
        let name = name.ok_or_else(|| missing(ctx, "chat.name"))?;
        let message = message.ok_or_else(|| missing(ctx, "chat.message"))?;
        out.extend_from_slice(b"{\"team\":");
        push_str(out, m_team);
        if lazer {
            out.extend_from_slice(b",\"timestamp\":");
            push_str(out, timestamp);
            out.extend_from_slice(b",\"name\":");
            push_str(out, name);
            out.extend_from_slice(b",\"message\":");
            push_str(out, message);
        } else {
            out.extend_from_slice(b",\"name\":");
            push_str(out, name);
            out.extend_from_slice(b",\"message\":");
            push_str(out, message);
            out.extend_from_slice(b",\"timestamp\":");
            push_str(out, timestamp);
        }
        out.push(b'}');
    }
    out.extend_from_slice(b"],\"totalScore\":{\"left\":");
    push_js_number(out, total_score.left);
    out.extend_from_slice(b",\"right\":");
    push_js_number(out, total_score.right);
    out.extend_from_slice(b"},\"clients\":[");
    for (i, item) in clients
        .ok_or_else(|| missing(ctx, "clients"))?
        .iter()
        .enumerate()
    {
        if i != 0 {
            out.push(b',');
        }
        write_tourney_client(out, item, lazer, ctx)?;
    }
    out.extend_from_slice(b"]}");
    Ok(())
}

fn write_tourney_client(out: &mut Vec<u8>, v: &Value, lazer: bool, ctx: &str) -> Result<(), Error> {
    let mut ipc_id = None;
    let mut team = None;
    let mut scroll_speed = None;
    let mut u_id = None;
    let mut u_name = None;
    let mut u_country = None;
    let mut u_accuracy = None;
    let mut u_ranked_score = None;
    let mut u_play_count = None;
    let mut u_global_rank = None;
    let mut u_total_pp = None;
    let mut stats = None;
    let mut play_val = None;
    for (ck, cv) in as_map(v, ctx)? {
        match ck.as_text() {
            Some("ipcId") => ipc_id = Some(as_f64(cv, ctx)?),
            Some("team") => team = Some(parse_tourney_team(cv, ctx)?),
            Some("settings") => {
                for (sk, sv) in as_map(cv, ctx)? {
                    if sk.as_text() == Some("mania") {
                        for (mk, mv) in as_map(sv, ctx)? {
                            if mk.as_text() == Some("scrollSpeed") {
                                scroll_speed = Some(as_i64(mv, ctx)?);
                            }
                        }
                    }
                }
            }
            Some("user") => {
                for (uk, uv) in as_map(cv, ctx)? {
                    match uk.as_text() {
                        Some("id") => u_id = Some(as_i64(uv, ctx)?),
                        Some("name") => u_name = Some(as_str(uv, ctx)?),
                        Some("country") => u_country = Some(as_str(uv, ctx)?),
                        Some("accuracy") => u_accuracy = Some(as_f64(uv, ctx)?),
                        Some("rankedScore") => u_ranked_score = Some(as_f64(uv, ctx)?),
                        Some("playCount") => u_play_count = Some(as_i64(uv, ctx)?),
                        Some("globalRank") => u_global_rank = Some(as_i64(uv, ctx)?),
                        Some("totalPP") => u_total_pp = Some(as_f64(uv, ctx)?),
                        _ => {}
                    }
                }
            }
            Some("beatmap") => {
                for (bk, bv) in as_map(cv, ctx)? {
                    if bk.as_text() == Some("stats") {
                        stats = Some(parse_stats(bv, ctx)?);
                    }
                }
            }
            Some("play") => play_val = Some(cv),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"ipcId\":");
    push_js_number(out, ipc_id.ok_or_else(|| missing(ctx, "clients.ipcId"))?);
    out.extend_from_slice(match team.ok_or_else(|| missing(ctx, "clients.team"))? {
        TourneyTeam::Left => b",\"team\":\"left\",\"settings\":{\"mania\":{\"scrollSpeed\":",
        TourneyTeam::Right => b",\"team\":\"right\",\"settings\":{\"mania\":{\"scrollSpeed\":",
    });
    write_i64(
        out,
        scroll_speed.ok_or_else(|| missing(ctx, "clients.settings.mania.scrollSpeed"))?,
    );
    out.extend_from_slice(b"}},\"user\":{\"id\":");
    write_i64(out, u_id.ok_or_else(|| missing(ctx, "user.id"))?);
    out.extend_from_slice(b",\"name\":");
    push_str(out, u_name.ok_or_else(|| missing(ctx, "user.name"))?);
    out.extend_from_slice(b",\"country\":");
    push_str(out, u_country.ok_or_else(|| missing(ctx, "user.country"))?);
    out.extend_from_slice(b",\"accuracy\":");
    push_js_number(
        out,
        u_accuracy.ok_or_else(|| missing(ctx, "user.accuracy"))?,
    );
    out.extend_from_slice(b",\"rankedScore\":");
    push_js_number(
        out,
        u_ranked_score.ok_or_else(|| missing(ctx, "user.rankedScore"))?,
    );
    out.extend_from_slice(b",\"playCount\":");
    write_i64(
        out,
        u_play_count.ok_or_else(|| missing(ctx, "user.playCount"))?,
    );
    out.extend_from_slice(b",\"globalRank\":");
    write_i64(
        out,
        u_global_rank.ok_or_else(|| missing(ctx, "user.globalRank"))?,
    );
    out.extend_from_slice(b",\"totalPP\":");
    push_js_number(out, u_total_pp.ok_or_else(|| missing(ctx, "user.totalPP"))?);
    out.extend_from_slice(b"},\"beatmap\":{\"stats\":");
    push_stats(
        out,
        &stats.ok_or_else(|| missing(ctx, "clients.beatmap.stats"))?,
    );
    out.extend_from_slice(b"},\"play\":");
    write_source_play(
        out,
        play_val.ok_or_else(|| missing(ctx, "clients.play"))?,
        lazer,
        ctx,
    )?;
    out.push(b'}');
    Ok(())
}

fn write_source_play(out: &mut Vec<u8>, v: &Value, lazer: bool, ctx: &str) -> Result<(), Error> {
    let mut failed = None;
    let mut player_name = None;
    let mut mode = None;
    let mut score = None;
    let mut accuracy = None;
    let mut health_bar = None;
    let mut judgements = None;
    let mut statistics = None;
    let mut hit_errors = None;
    let mut combo = None;
    let mut mods = None;
    let mut rank = None;
    let mut pp = None;
    let mut unstable_rate = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("failed") => failed = Some(as_bool(val, ctx)?),
            Some("playerName") => player_name = Some(as_str(val, ctx)?),
            Some("mode") => mode = Some(parse_number_name(val, ctx)?),
            Some("score") => score = Some(as_f64(val, ctx)?),
            Some("accuracy" | "accuracyPercent") => accuracy = Some(as_f64(val, ctx)?),
            Some("healthBar") => health_bar = Some(parse_health_bar(val, ctx)?),
            Some("judgements") => judgements = Some(parse_judgements(val, ctx)?),
            Some("statistics") => statistics = Some(parse_statistics(val, ctx)?),
            Some("hitErrors") => {
                for (hk, hv) in as_map(val, ctx)? {
                    if hk.as_text() == Some("all") {
                        hit_errors = Some(as_array(hv, ctx)?);
                    }
                }
            }
            Some("combo") => combo = Some(parse_combo(val, ctx)?),
            Some("mods") => mods = Some(val),
            Some("rank") => rank = Some(parse_rank_ref(val, ctx)?),
            Some("pp") => pp = Some(parse_pp(val, ctx)?),
            Some("unstableRate") => unstable_rate = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    let h = judgements.ok_or_else(|| missing(ctx, "judgements"))?;
    let s = statistics.ok_or_else(|| missing(ctx, "statistics"))?;
    let hits = if lazer {
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
    let (rank_cur, rank_max) = rank.ok_or_else(|| missing(ctx, "rank"))?;
    write_play_fields(
        out,
        &PlayFields {
            failed: failed.ok_or_else(|| missing(ctx, "failed"))?,
            player_name: player_name.ok_or_else(|| missing(ctx, "playerName"))?,
            mode: mode.ok_or_else(|| missing(ctx, "mode"))?,
            score: score.ok_or_else(|| missing(ctx, "score"))?,
            accuracy: accuracy.ok_or_else(|| missing(ctx, "accuracy"))?,
            health_bar: health_bar.ok_or_else(|| missing(ctx, "healthBar"))?,
            hits,
            hit_errors: hit_errors.ok_or_else(|| missing(ctx, "hitErrors.all"))?,
            combo: combo.ok_or_else(|| missing(ctx, "combo"))?,
            mods_val: mods.ok_or_else(|| missing(ctx, "mods"))?,
            rank_cur,
            rank_max,
            pp: pp.ok_or_else(|| missing(ctx, "pp"))?,
            unstable_rate: unstable_rate.ok_or_else(|| missing(ctx, "unstableRate"))?,
        },
        ctx,
    )
}

pub(super) fn write_room(out: &mut Vec<u8>, v: &Value, ctx: &str) -> Result<(), Error> {
    let mut room_id = None;
    let mut users = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("roomID" | "roomId") => room_id = Some(as_i64(val, ctx)?),
            Some("users") => users = Some(as_array(val, ctx)?),
            _ => {}
        }
    }
    out.extend_from_slice(b"{\"roomID\":");
    write_i64(out, room_id.ok_or_else(|| missing(ctx, "roomID"))?);
    out.extend_from_slice(b",\"users\":[");
    for (i, item) in users
        .ok_or_else(|| missing(ctx, "users"))?
        .iter()
        .enumerate()
    {
        if i != 0 {
            out.push(b',');
        }
        let mut id = None;
        let mut username = None;
        let mut country_code = None;
        let mut avatar_url = None;
        for (uk, uv) in as_map(item, ctx)? {
            match uk.as_text() {
                Some("id") => id = Some(as_i64(uv, ctx)?),
                Some("username") => username = Some(as_str(uv, ctx)?),
                Some("countryCode") => country_code = Some(as_i64(uv, ctx)?),
                Some("avatarUrl") => avatar_url = as_opt_str(uv, ctx)?,
                _ => {}
            }
        }
        out.extend_from_slice(b"{\"id\":");
        write_i64(out, id.ok_or_else(|| missing(ctx, "users.id"))?);
        out.extend_from_slice(b",\"username\":");
        push_str(out, username.ok_or_else(|| missing(ctx, "users.username"))?);
        out.extend_from_slice(b",\"countryCode\":");
        write_i64(
            out,
            country_code.ok_or_else(|| missing(ctx, "users.countryCode"))?,
        );
        if let Some(url) = avatar_url {
            out.extend_from_slice(b",\"avatarUrl\":");
            push_str(out, url);
        }
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
    Ok(())
}

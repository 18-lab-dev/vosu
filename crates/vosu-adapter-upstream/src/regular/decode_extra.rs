//! Owned DTO decoders for rankedPlay, leaderboard, performance, results, tourney, and room.
use super::{decode::decode_mods, parse::*, parse_settings::*};
use crate::{
    Error,
    dto::*,
    source::{HitErrors, SourcePlay},
    view::{as_array, as_bool, as_f64, as_i64, as_map, as_opt_str, as_str, missing},
};
use ciborium::Value;
use vosu_osu_model::memory::{RankedPlay, RankedPlayUserInfo};

pub(super) fn decode_ranked_play(v: &Value, ctx: &str) -> Result<RankedPlay, Error> {
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
            Some("users") => {
                let items = as_array(val, ctx)?;
                let mut list = Vec::with_capacity(items.len());
                for item in items {
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
                    list.push(RankedPlayUserInfo {
                        id: id.ok_or_else(|| missing(ctx, "users.id"))?,
                        rating: rating.ok_or_else(|| missing(ctx, "users.rating"))?,
                        life: life.ok_or_else(|| missing(ctx, "users.life"))?,
                        rounds_won: rounds_won.ok_or_else(|| missing(ctx, "users.roundsWon"))?,
                        damage_multiplier: dm
                            .ok_or_else(|| missing(ctx, "users.damageMultiplier"))?,
                    });
                }
                users = Some(list);
            }
            _ => {}
        }
    }
    Ok(RankedPlay {
        stage: stage.ok_or_else(|| missing(ctx, "stage"))?,
        current_round: current_round.ok_or_else(|| missing(ctx, "currentRound"))?,
        star_rating: star_rating.ok_or_else(|| missing(ctx, "starRating"))?,
        damage_multiplier: damage_multiplier.ok_or_else(|| missing(ctx, "damageMultiplier"))?,
        active_user_id: active_user_id.ok_or_else(|| missing(ctx, "activeUserId"))?,
        winning_user_id: winning_user_id.ok_or_else(|| missing(ctx, "winningUserId"))?,
        users: users.ok_or_else(|| missing(ctx, "users"))?,
    })
}

pub(super) fn decode_leaderboard(v: &Value, ctx: &str) -> Result<Vec<Leaderboard>, Error> {
    let items = as_array(v, ctx)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
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
                Some("name") => name = Some(as_str(val, ctx)?.to_owned()),
                Some("score") => score = Some(as_f64(val, ctx)?),
                Some("accuracy") => accuracy = Some(as_f64(val, ctx)?),
                Some("hits" | "statistics") => hits = Some(parse_leaderboard_hits(val, ctx)?),
                Some("combo") => combo = Some(parse_combo(val, ctx)?),
                Some("mods") => mods = Some(decode_mods(val, ctx)?),
                Some("rank") => rank = Some(as_str(val, ctx)?.to_owned()),
                _ => {}
            }
        }
        out.push(Leaderboard {
            is_failed: is_failed.ok_or_else(|| missing(ctx, "isFailed"))?,
            position: position.ok_or_else(|| missing(ctx, "position"))?,
            team: team.ok_or_else(|| missing(ctx, "team"))?,
            id: id.ok_or_else(|| missing(ctx, "id"))?,
            name: name.ok_or_else(|| missing(ctx, "name"))?,
            score: score.ok_or_else(|| missing(ctx, "score"))?,
            accuracy: accuracy.ok_or_else(|| missing(ctx, "accuracy"))?,
            hits: hits.ok_or_else(|| missing(ctx, "hits"))?,
            combo: combo.ok_or_else(|| missing(ctx, "combo"))?,
            mods: mods.ok_or_else(|| missing(ctx, "mods"))?,
            rank: rank.ok_or_else(|| missing(ctx, "rank"))?,
        });
    }
    Ok(out)
}

pub(super) fn decode_performance(v: &Value, ctx: &str) -> Result<Performance, Error> {
    let mut accuracy = None;
    let mut graph = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("accuracy") => accuracy = Some(parse_accuracy(val, ctx)?),
            Some("graph") => {
                let mut series = None;
                let mut xaxis = None;
                for (gk, gv) in as_map(val, ctx)? {
                    match gk.as_text() {
                        Some("series") => {
                            let items = as_array(gv, ctx)?;
                            let mut list = Vec::with_capacity(items.len());
                            for item in items {
                                let mut name = None;
                                let mut data = None;
                                for (sk, sv) in as_map(item, ctx)? {
                                    match sk.as_text() {
                                        Some("name") => name = Some(as_str(sv, ctx)?.to_owned()),
                                        Some("data") => data = Some(parse_f64_vec(sv, ctx)?),
                                        _ => {}
                                    }
                                }
                                list.push(Series {
                                    name: name.ok_or_else(|| missing(ctx, "series.name"))?,
                                    data: data.ok_or_else(|| missing(ctx, "series.data"))?,
                                });
                            }
                            series = Some(list);
                        }
                        Some("xaxis") => xaxis = Some(parse_f64_vec(gv, ctx)?),
                        _ => {}
                    }
                }
                graph = Some(Graph {
                    series: series.ok_or_else(|| missing(ctx, "graph.series"))?,
                    xaxis: xaxis.ok_or_else(|| missing(ctx, "graph.xaxis"))?,
                });
            }
            _ => {}
        }
    }
    Ok(Performance {
        accuracy: accuracy.ok_or_else(|| missing(ctx, "accuracy"))?,
        graph: graph.ok_or_else(|| missing(ctx, "graph"))?,
    })
}

pub(super) fn decode_results(v: &Value, ctx: &str) -> Result<ResultsScreen, Error> {
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
            Some("playerName") => player_name = Some(as_str(val, ctx)?.to_owned()),
            Some("mode") => mode = Some(parse_number_name(val, ctx)?.into_owned()),
            Some("score") => score = Some(as_f64(val, ctx)?),
            Some("accuracy") => accuracy = Some(as_f64(val, ctx)?),
            Some("name") => name = Some(as_str(val, ctx)?.to_owned()),
            Some("hits" | "statistics") => hits = Some(parse_results_hits(val, ctx)?),
            Some("mods") => mods = Some(decode_mods(val, ctx)?),
            Some("maxCombo") => max_combo = Some(as_i64(val, ctx)?),
            Some("rank") => rank = Some(as_str(val, ctx)?.to_owned()),
            Some("pp") => pp = Some(parse_results_pp(val, ctx)?),
            Some("createdAt") => created_at = Some(as_str(val, ctx)?.to_owned()),
            _ => {}
        }
    }
    Ok(ResultsScreen {
        score_id: score_id.ok_or_else(|| missing(ctx, "scoreId"))?,
        player_name: player_name.ok_or_else(|| missing(ctx, "playerName"))?,
        mode: mode.ok_or_else(|| missing(ctx, "mode"))?,
        score: score.ok_or_else(|| missing(ctx, "score"))?,
        accuracy: accuracy.ok_or_else(|| missing(ctx, "accuracy"))?,
        name: name.ok_or_else(|| missing(ctx, "name"))?,
        hits: hits.ok_or_else(|| missing(ctx, "hits"))?,
        mods: mods.ok_or_else(|| missing(ctx, "mods"))?,
        max_combo: max_combo.ok_or_else(|| missing(ctx, "maxCombo"))?,
        rank: rank.ok_or_else(|| missing(ctx, "rank"))?,
        pp: pp.ok_or_else(|| missing(ctx, "pp"))?,
        created_at: created_at.ok_or_else(|| missing(ctx, "createdAt"))?,
    })
}

fn decode_source_play(v: &Value, lazer: bool, ctx: &str) -> Result<Play, Error> {
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
            Some("playerName") => player_name = Some(as_str(val, ctx)?.to_owned()),
            Some("mode") => mode = Some(parse_number_name(val, ctx)?.into_owned()),
            Some("score") => score = Some(as_f64(val, ctx)?),
            Some("accuracy" | "accuracyPercent") => accuracy = Some(as_f64(val, ctx)?),
            Some("healthBar") => health_bar = Some(parse_health_bar(val, ctx)?),
            Some("judgements") => judgements = Some(parse_judgements(val, ctx)?),
            Some("statistics") => statistics = Some(parse_statistics(val, ctx)?),
            Some("hitErrors") => {
                let mut all = None;
                for (hk, hv) in as_map(val, ctx)? {
                    if hk.as_text() == Some("all") {
                        all = Some(parse_f64_vec(hv, ctx)?);
                    }
                }
                hit_errors = Some(HitErrors {
                    all: all.ok_or_else(|| missing(ctx, "hitErrors.all"))?,
                });
            }
            Some("combo") => combo = Some(parse_combo(val, ctx)?),
            Some("mods") => mods = Some(decode_mods(val, ctx)?),
            Some("rank") => {
                let (current, max_this_play) = parse_rank_ref(val, ctx)?;
                rank = Some(Rank {
                    current: current.to_owned(),
                    max_this_play: max_this_play.to_owned(),
                });
            }
            Some("pp") => pp = Some(parse_pp(val, ctx)?),
            Some("unstableRate") => unstable_rate = Some(as_f64(val, ctx)?),
            _ => {}
        }
    }
    Ok(SourcePlay {
        failed: failed.ok_or_else(|| missing(ctx, "failed"))?,
        player_name: player_name.ok_or_else(|| missing(ctx, "playerName"))?,
        mode: mode.ok_or_else(|| missing(ctx, "mode"))?,
        score: score.ok_or_else(|| missing(ctx, "score"))?,
        accuracy: accuracy.ok_or_else(|| missing(ctx, "accuracy"))?,
        health_bar: health_bar.ok_or_else(|| missing(ctx, "healthBar"))?,
        judgements: judgements.ok_or_else(|| missing(ctx, "judgements"))?,
        statistics: statistics.ok_or_else(|| missing(ctx, "statistics"))?,
        hit_errors: hit_errors.ok_or_else(|| missing(ctx, "hitErrors"))?,
        combo: combo.ok_or_else(|| missing(ctx, "combo"))?,
        mods: mods.ok_or_else(|| missing(ctx, "mods"))?,
        rank: rank.ok_or_else(|| missing(ctx, "rank"))?,
        pp: pp.ok_or_else(|| missing(ctx, "pp"))?,
        unstable_rate: unstable_rate.ok_or_else(|| missing(ctx, "unstableRate"))?,
    }
    .into_wire(lazer))
}

pub(super) fn decode_tourney(v: &Value, lazer: bool, ctx: &str) -> Result<Tourney, Error> {
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
            Some("team") => {
                let (left, right) = parse_sides_str(val, ctx)?;
                team = Some(Sides {
                    left: left.to_owned(),
                    right: right.to_owned(),
                });
            }
            Some("points") => points = Some(parse_sides_i64(val, ctx)?),
            Some("chat") => {
                let items = as_array(val, ctx)?;
                let mut list = Vec::with_capacity(items.len());
                for item in items {
                    let mut m_team = None;
                    let mut timestamp = None;
                    let mut name = None;
                    let mut message = None;
                    for (ck, cv) in as_map(item, ctx)? {
                        match ck.as_text() {
                            Some("team") => m_team = Some(as_str(cv, ctx)?.to_owned()),
                            Some("timestamp") => timestamp = Some(as_str(cv, ctx)?.to_owned()),
                            Some("name") => name = Some(as_str(cv, ctx)?.to_owned()),
                            Some("message") => message = Some(as_str(cv, ctx)?.to_owned()),
                            _ => {}
                        }
                    }
                    let m_team = m_team.ok_or_else(|| missing(ctx, "chat.team"))?;
                    let timestamp = timestamp.ok_or_else(|| missing(ctx, "chat.timestamp"))?;
                    let name = name.ok_or_else(|| missing(ctx, "chat.name"))?;
                    let message = message.ok_or_else(|| missing(ctx, "chat.message"))?;
                    list.push(if lazer {
                        TourneyChatMessage::Lazer(LazerTourneyChatMessage {
                            team: m_team,
                            timestamp,
                            name,
                            message,
                        })
                    } else {
                        TourneyChatMessage::Stable(StableTourneyChatMessage {
                            team: m_team,
                            name,
                            message,
                            timestamp,
                        })
                    });
                }
                chat = Some(list);
            }
            Some("totalScore") => total_score = Some(parse_sides_f64(val, ctx)?),
            Some("clients") => {
                let items = as_array(val, ctx)?;
                let mut list = Vec::with_capacity(items.len());
                for item in items {
                    list.push(decode_tourney_client(item, lazer, ctx)?);
                }
                clients = Some(list);
            }
            _ => {}
        }
    }
    Ok(Tourney {
        score_visible: score_visible.ok_or_else(|| missing(ctx, "scoreVisible"))?,
        stars_visible: stars_visible.ok_or_else(|| missing(ctx, "starsVisible"))?,
        ipc_state: ipc_state.ok_or_else(|| missing(ctx, "ipcState"))?,
        best_of: best_of.ok_or_else(|| missing(ctx, "bestOf"))?,
        team: team.ok_or_else(|| missing(ctx, "team"))?,
        points: points.ok_or_else(|| missing(ctx, "points"))?,
        chat: chat.ok_or_else(|| missing(ctx, "chat"))?,
        total_score: total_score.ok_or_else(|| missing(ctx, "totalScore"))?,
        clients: clients.ok_or_else(|| missing(ctx, "clients"))?,
    })
}

fn decode_tourney_client(item: &Value, lazer: bool, ctx: &str) -> Result<TourneyClient, Error> {
    let mut ipc_id = None;
    let mut c_team = None;
    let mut settings = None;
    let mut user = None;
    let mut beatmap = None;
    let mut play = None;
    for (ck, cv) in as_map(item, ctx)? {
        match ck.as_text() {
            Some("ipcId") => ipc_id = Some(as_f64(cv, ctx)?),
            Some("team") => c_team = Some(parse_tourney_team(cv, ctx)?),
            Some("settings") => {
                let mut scroll_speed = None;
                for (sk, sv) in as_map(cv, ctx)? {
                    if sk.as_text() == Some("mania") {
                        for (mk, mv) in as_map(sv, ctx)? {
                            if mk.as_text() == Some("scrollSpeed") {
                                scroll_speed = Some(as_i64(mv, ctx)?);
                            }
                        }
                    }
                }
                settings = Some(TourneyClientSettings {
                    mania: TourneyManiaSettings {
                        scroll_speed: scroll_speed
                            .ok_or_else(|| missing(ctx, "clients.settings.mania.scrollSpeed"))?,
                    },
                });
            }
            Some("user") => {
                let mut id = None;
                let mut name = None;
                let mut country = None;
                let mut accuracy = None;
                let mut ranked_score = None;
                let mut play_count = None;
                let mut global_rank = None;
                let mut total_pp = None;
                for (uk, uv) in as_map(cv, ctx)? {
                    match uk.as_text() {
                        Some("id") => id = Some(as_i64(uv, ctx)?),
                        Some("name") => name = Some(as_str(uv, ctx)?.to_owned()),
                        Some("country") => country = Some(as_str(uv, ctx)?.to_owned()),
                        Some("accuracy") => accuracy = Some(as_f64(uv, ctx)?),
                        Some("rankedScore") => ranked_score = Some(as_f64(uv, ctx)?),
                        Some("playCount") => play_count = Some(as_i64(uv, ctx)?),
                        Some("globalRank") => global_rank = Some(as_i64(uv, ctx)?),
                        Some("totalPP") => total_pp = Some(as_f64(uv, ctx)?),
                        _ => {}
                    }
                }
                user = Some(TourneyUser {
                    id: id.ok_or_else(|| missing(ctx, "user.id"))?,
                    name: name.ok_or_else(|| missing(ctx, "user.name"))?,
                    country: country.ok_or_else(|| missing(ctx, "user.country"))?,
                    accuracy: accuracy.ok_or_else(|| missing(ctx, "user.accuracy"))?,
                    ranked_score: ranked_score.ok_or_else(|| missing(ctx, "user.rankedScore"))?,
                    play_count: play_count.ok_or_else(|| missing(ctx, "user.playCount"))?,
                    global_rank: global_rank.ok_or_else(|| missing(ctx, "user.globalRank"))?,
                    total_pp: total_pp.ok_or_else(|| missing(ctx, "user.totalPP"))?,
                });
            }
            Some("beatmap") => {
                let mut stats = None;
                for (bk, bv) in as_map(cv, ctx)? {
                    if bk.as_text() == Some("stats") {
                        stats = Some(parse_stats(bv, ctx)?);
                    }
                }
                beatmap = Some(TourneyBeatmap {
                    stats: stats.ok_or_else(|| missing(ctx, "clients.beatmap.stats"))?,
                });
            }
            Some("play") => play = Some(decode_source_play(cv, lazer, ctx)?),
            _ => {}
        }
    }
    Ok(TourneyClient {
        ipc_id: ipc_id.ok_or_else(|| missing(ctx, "clients.ipcId"))?,
        team: c_team.ok_or_else(|| missing(ctx, "clients.team"))?,
        settings: settings.ok_or_else(|| missing(ctx, "clients.settings"))?,
        user: user.ok_or_else(|| missing(ctx, "clients.user"))?,
        beatmap: beatmap.ok_or_else(|| missing(ctx, "clients.beatmap"))?,
        play: play.ok_or_else(|| missing(ctx, "clients.play"))?,
    })
}

pub(super) fn decode_room(v: &Value, ctx: &str) -> Result<Room, Error> {
    let mut room_id = None;
    let mut users = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("roomID" | "roomId") => room_id = Some(as_i64(val, ctx)?),
            Some("users") => {
                let items = as_array(val, ctx)?;
                let mut list = Vec::with_capacity(items.len());
                for item in items {
                    let mut id = None;
                    let mut username = None;
                    let mut country_code = None;
                    let mut avatar_url = None;
                    for (uk, uv) in as_map(item, ctx)? {
                        match uk.as_text() {
                            Some("id") => id = Some(as_i64(uv, ctx)?),
                            Some("username") => username = Some(as_str(uv, ctx)?.to_owned()),
                            Some("countryCode") => country_code = Some(as_i64(uv, ctx)?),
                            Some("avatarUrl") => {
                                avatar_url = as_opt_str(uv, ctx)?.map(str::to_owned);
                            }
                            _ => {}
                        }
                    }
                    list.push(RoomUser {
                        id: id.ok_or_else(|| missing(ctx, "users.id"))?,
                        username: username.ok_or_else(|| missing(ctx, "users.username"))?,
                        country_code: country_code
                            .ok_or_else(|| missing(ctx, "users.countryCode"))?,
                        avatar_url,
                    });
                }
                users = Some(list);
            }
            _ => {}
        }
    }
    Ok(Room {
        room_id: room_id.ok_or_else(|| missing(ctx, "roomID"))?,
        users: users.ok_or_else(|| missing(ctx, "users"))?,
    })
}

use crate::{Error, View, model::*, view};
use std::io;
use vosu_adapter_support::json::{With, wire_object};
use vosu_osu_model::source::TourneyAvailability;

pub(super) fn check(v: &View) -> Result<(), Error> {
    if v.tourney_availability == TourneyAvailability::MissingScore {
        return Err(Error::Serialization(
            "lazer tournament spectator has no score".into(),
        ));
    }
    if v.tourney_availability == TourneyAvailability::Ready && v.tourney.is_none() {
        return Err(Error::NotReady);
    }
    Ok(())
}
/// Pending source data omits the wire key; a ready empty aggregate is an object.
pub(super) fn source(v: &View) -> Option<&view::Tourney> {
    if v.tourney_availability == TourneyAvailability::Pending {
        None
    } else {
        v.tourney.as_ref()
    }
}
fn chat_team<'a>(t: &'a view::Tourney, name: &str) -> &'a str {
    t.clients
        .iter()
        .find(|c| c.user.name == name)
        .map(|c| c.team.as_str())
        .unwrap_or(if name == "BanchoBot" {
            "bot"
        } else {
            "unknown"
        })
}
fn gameplay(p: &view::ClientPlay, lazer: bool) -> TourneyGameplay {
    let h = &p.raw_judgements;
    TourneyGameplay {
        game_mode: p.mode.number,
        name: p.player_name.clone(),
        score: p.score,
        accuracy: p.accuracy,
        combo: Combo {
            current: p.combo.current,
            max: p.combo.max,
        },
        hp: Health {
            normal: p.health_raw.normal,
            smooth: p.health_raw.smooth,
        },
        hits: TourneyHits {
            h300: h.great,
            geki: h.perfect,
            h100: h.ok,
            katu: h.good,
            h50: h.meh,
            h0: h.miss,
            slider_breaks: p.statistics.slider_breaks,
            grade: GameplayGrade {
                current: p.rank.current.clone(),
                max_this_play: if lazer {
                    String::new()
                } else {
                    p.rank.max_this_play.clone()
                },
            },
            unstable_rate: p.unstable_rate,
            hit_error_array: p.hit_errors.all.to_vec(),
        },
        mods: crate::mods(&p.mods),
    }
}
pub(super) fn build(v: &View) -> Result<Option<Tourney>, Error> {
    check(v)?;
    let Some(t) = source(v) else {
        return Ok(None);
    };
    Ok(Some(Tourney {
        manager: TourneyManager {
            ipc_state: t.ipc_state,
            best_of: t.best_of,
            team_name: TourneyName {
                left: t.team.left.clone(),
                right: t.team.right.clone(),
            },
            stars: TourneyStars {
                left: t.points.left,
                right: t.points.right,
            },
            bools: TourneyBools {
                score_visible: t.score_visible,
                stars_visible: t.stars_visible,
            },
            chat: t
                .chat
                .iter()
                .map(|m| TourneyMessage {
                    team: chat_team(t, &m.name).into(),
                    time: m.timestamp.clone(),
                    name: m.name.clone(),
                    message_body: m.message.clone(),
                })
                .collect(),
            gameplay: TourneyManagerGameplay {
                score: TourneyScore {
                    left: t.total_score.left,
                    right: t.total_score.right,
                },
            },
        },
        ipc_clients: t
            .clients
            .iter()
            .map(|c| TourneyIpcClient {
                team: c.team.clone(),
                spectating: TourneyIpcSpec {
                    name: c.user.name.clone(),
                    country: c.user.country.clone(),
                    user_id: c.user.id,
                    accuracy: c.user.accuracy,
                    ranked_score: c.user.ranked_score,
                    play_count: c.user.play_count,
                    global_rank: c.user.global_rank,
                    total_pp: c.user.total_pp,
                },
                gameplay: gameplay(&c.play, v.client == "lazer"),
            })
            .collect(),
    }))
}
pub(super) fn write(out: &mut Vec<u8>, v: &View, source: &view::Tourney) -> io::Result<()> {
    let t = Some(source);
    wire_object!(out {
        "manager": {
            "ipcState": t.map_or(0, |t| t.ipc_state), "bestOF": t.map_or(0, |t| t.best_of),
            "teamName": { "left": t.map_or("", |t| t.team.left.as_str()), "right": t.map_or("", |t| t.team.right.as_str()) },
            "stars": { "left": t.map_or(0, |t| t.points.left), "right": t.map_or(0, |t| t.points.right) },
            "bools": { "scoreVisible": t.is_some_and(|t| t.score_visible), "starsVisible": t.is_some_and(|t| t.stars_visible) },
            "chat": With(|out: &mut Vec<u8>| {
                out.push(b'[');
                if let Some(t) = t {
                    for (i, m) in t.chat.iter().enumerate() {
                        if i != 0 { out.push(b','); }
                        wire_object!(out { "team": chat_team(t, &m.name), "time": m.timestamp, "name": m.name, "messageBody": m.message });
                    }
                }
                out.push(b']'); Ok(())
            }), "gameplay": { "score": { "left": t.map_or(0.0, |t| t.total_score.left), "right": t.map_or(0.0, |t| t.total_score.right) } },
        },
        "ipcClients": With(|out: &mut Vec<u8>| {
            out.push(b'[');
            if let Some(t) = t {
                for (i, c) in t.clients.iter().enumerate() {
                    if i != 0 { out.push(b','); }
                    let u = &c.user; let p = &c.play; let h = &p.raw_judgements;
                    wire_object!(out {
                        "team": c.team, "spectating": { "name": u.name, "country": u.country, "userID": u.id, "accuracy": u.accuracy,
                            "rankedScore": u.ranked_score, "playCount": u.play_count, "globalRank": u.global_rank, "totalPP": u.total_pp },
                        "gameplay": {
                            "gameMode": p.mode.number, "name": p.player_name, "score": p.score, "accuracy": p.accuracy,
                            "combo": { "current": p.combo.current, "max": p.combo.max }, "hp": { "normal": p.health_raw.normal, "smooth": p.health_raw.smooth },
                            "hits": { "0": h.miss, "50": h.meh, "100": h.ok, "300": h.great, "geki": h.perfect, "katu": h.good,
                                "sliderBreaks": p.statistics.slider_breaks,
                                "grade": { "current": p.rank.current, "maxThisPlay": if v.client == "lazer" { "" } else { p.rank.max_this_play.as_str() } },
                                "unstableRate": p.unstable_rate, "hitErrorArray": p.hit_errors.all.as_ref(),
                            }, "mods": { "num": p.mods.number, "str": p.mods.name },
                        },
                    });
                }
            }
            out.push(b']'); Ok(())
        }),
    });
    Ok(())
}

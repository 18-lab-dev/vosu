//! Direct gosuMemory `/json` (v1) wire writer.
//!
//! [`build_with`](super::build_with) materializes the whole [`Response`] DTO,
//! cloning every string, only for the cold encode to serialize and drop it.
//! `encode` writes the same bytes straight from the state. The tournament
//! subtree is rare and nested, so it keeps its DTO builders and serde
//! serialization. `build` stays the contract for filtered projection; debug
//! builds check every direct encode against it.

use std::io::{self, Write};

use super::*;
use crate::api::json::{Serde, Wire, With, wire_object};

/// Encodes the unfiltered v1 document exactly as `build_with` serializes
/// through [`JsFormatter`](crate::api::json::JsFormatter).
pub(super) fn encode(snapshot: &CombinedSnapshot) -> Result<Vec<u8>, ApiError> {
    let focused = snapshot.focused().ok_or(ApiError::NotReady)?;
    let regular = &focused.regular;
    let precise = &focused.precise;
    // The only fallible step of `build_with`, in the same position.
    let tourney = if focused.source == ClientType::Lazer
        && precise.status == i64::from(GameState::Lobby.value())
    {
        build_lazer_tourney(regular, precise.status)?
    } else {
        Some(build_stable_tourney(snapshot))
    };
    let graph = &regular.beatmap.graph;
    let hit_errors = &precise.hit_errors;
    let mut out = Vec::with_capacity(
        3 * 1024
            + 256 * regular.gameplay.leaderboard_scores.len()
            + 24 * (graph.primary.len()
                + graph.x_axis.len()
                + graph.series.iter().map(|s| s.data.len()).sum::<usize>()
                + hit_errors.all.len()
                + hit_errors.press.len()
                + hit_errors.release.len()),
    );
    write_document(&mut out, focused.source, regular, precise, tourney.as_ref())
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(out)
}

/// `{"num":…,"str":…}` as [`mods`] projects it.
struct ModsWire<'a>(&'a CalculateMods);

impl Wire for ModsWire<'_> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out { "num": self.0.number, "str": self.0.name });
        Ok(())
    }
}

/// A [`LeaderboardPlayer`] as [`leaderboard_fields`] projects it; the two
/// state player types share these fields.
macro_rules! leaderboard_player {
    ($out:ident, $player:expr) => {{
        let player = $player;
        let statistics = &player.statistics;
        wire_object!($out {
            "name": player.name,
            "score": player.score,
            "combo": player.combo,
            "maxCombo": player.max_combo,
            "mods": player.mods.name,
            "h300": statistics.great,
            "geki": statistics.perfect,
            "h100": statistics.ok,
            "katu": statistics.good,
            "h50": statistics.meh,
            "h0": statistics.miss,
            "team": player.team,
            "position": player.position,
            "isPassing": i64::from(player.is_passing),
        });
    }};
}

fn write_document(
    out: &mut Vec<u8>,
    source: ClientType,
    regular: &RegularState,
    precise: &PreciseState,
    tourney: Option<&Tourney>,
) -> io::Result<()> {
    let current_mods = selected_calculate_mods(precise.status, regular);
    let parsed = regular.beatmap.parsed.as_deref();
    let progressive = regular.beatmap.attributes.unwrap_or_default();
    let graph = &regular.beatmap.graph;
    let menu = &regular.menu;
    let gameplay = &regular.gameplay;
    let result = &regular.result_screen;
    let legacy_hits = to_legacy_hits(gameplay.mode as i32, &gameplay.statistics);

    let default_difficulty = crate::calculation::beatmap::DifficultyValues::default();
    let (original, converted, counts, full_stars, max_combo, times, bpm) = if let Some(map) = parsed
    {
        (
            &map.engine.original_difficulty,
            &map.engine.converted_difficulty,
            map.object_counts,
            map.engine.difficulty.stars,
            i64::from(map.engine.difficulty.max_combo),
            map.times,
            map.bpm,
        )
    } else {
        (
            &default_difficulty,
            &default_difficulty,
            Default::default(),
            0.0,
            0,
            Default::default(),
            Default::default(),
        )
    };
    let pp = |accuracy: u8| {
        fix_decimals(
            regular
                .beatmap
                .pp_acc
                .get(&accuracy)
                .copied()
                .unwrap_or(0.0),
        )
    };
    // Four decimals are always valid, so `build_with`'s error arm is unreachable.
    let round4 = |value: f64| fix_decimals_to(value, 4).expect("four decimals are valid");
    let key = |index: usize| {
        let button = precise.key_overlay.get(index);
        With(move |out: &mut Vec<u8>| {
            wire_object!(out {
                "isPressed": button.is_some_and(|button| button.is_pressed),
                "count": button.map_or(0, |button| button.count),
            });
            Ok(())
        })
    };

    // Too many members for one macro expansion; written in groups.
    wire_object!(@open first);
    wire_object!(@members out first {
        "client": match source {
            ClientType::Stable => "stable",
            ClientType::Lazer => "lazer",
        },
        "settings": {
            "showInterface": regular.global.show_interface,
            "folders": {
                "game": regular.global.game_folder,
                "skin": regular.global.skin_folder,
                "songs": regular.global.songs_folder,
            },
        },
        "menu": With(|out: &mut Vec<u8>| {
            wire_object!(@open first);
            wire_object!(@members out first {
                "mainMenu": {
                    "bassDensity": regular.bass_density.density,
                },
                "state": precise.status,
                "gameMode"?: menu.gamemode,
                "isChatEnabled": i64::from(regular.global.chat_status != 0),
                "bm": {
                    "time": {
                        "firstObj": i64::from(times.first_object),
                        "current": precise.play_time,
                        "full": times.full,
                        "mp3"?: menu.mp3_length,
                    },
                    "id"?: menu.map_id,
                    "set"?: menu.set_id,
                    "md5"?: menu.checksum.as_ref(),
                    "rankedStatus"?: menu.ranked_status,
                    "metadata": {
                        "artist"?: menu.artist.as_ref(),
                        "artistOriginal"?: menu.artist_original.as_ref(),
                        "title"?: menu.title.as_ref(),
                        "titleOriginal"?: menu.title_original.as_ref(),
                        "mapper"?: menu.creator.as_ref(),
                        "difficulty"?: menu.difficulty.as_ref(),
                    },
                    "stats": With(|out: &mut Vec<u8>| {
                        wire_object!(out {
                            "AR": fix_decimals(converted.approach_rate),
                            "CS": fix_decimals(converted.circle_size),
                            "OD": fix_decimals(converted.overall_difficulty),
                            "HP": fix_decimals(converted.drain_rate),
                            "SR": fix_decimals(progressive.stars),
                            "BPM": {
                                "realtime": round4(regular.beatmap.event_status.bpm),
                                "common": round4(bpm.common),
                                "min": round4(bpm.minimum),
                                "max": round4(bpm.maximum),
                            },
                            "circles": i64::from(counts.circles),
                            "sliders": i64::from(counts.sliders),
                            "spinners": i64::from(counts.spinners),
                            "holds": i64::from(counts.holds),
                            "maxCombo": max_combo,
                            "fullSR": fix_decimals(full_stars),
                            "memoryAR": fix_decimals(original.approach_rate),
                            "memoryCS": fix_decimals(original.circle_size),
                            "memoryOD": fix_decimals(original.overall_difficulty),
                            "memoryHP": fix_decimals(original.drain_rate),
                        });
                        Ok(())
                    }),
                    "path": {
                        "full": joined_path(
                            menu.folder.as_deref().unwrap_or(""),
                            menu.background_filename.as_deref().unwrap_or(""),
                        ),
                        "folder"?: menu.folder.as_ref(),
                        "file"?: menu.filename.as_ref(),
                        "bg"?: menu.background_filename.as_ref(),
                        "audio"?: menu.audio_filename.as_ref(),
                    },
                },
                "mods": ModsWire(current_mods),
            });
            wire_object!(@members out first {
                "pp": {
                    "90": pp(90),
                    "91": pp(91),
                    "92": pp(92),
                    "93": pp(93),
                    "94": pp(94),
                    "95": pp(95),
                    "96": pp(96),
                    "97": pp(97),
                    "98": pp(98),
                    "99": pp(99),
                    "100": pp(100),
                    "strains": graph.primary.as_slice(),
                    "strainsAll": {
                        "series": With(|out: &mut Vec<u8>| {
                            out.push(b'[');
                            for (index, series) in graph.series.iter().enumerate() {
                                if index > 0 {
                                    out.push(b',');
                                }
                                wire_object!(out {
                                    "name": series.name,
                                    "data": series.data.as_slice(),
                                });
                            }
                            out.push(b']');
                            Ok(())
                        }),
                        "xaxis": graph.x_axis.as_slice(),
                    },
                },
            });
            wire_object!(@close out first);
            Ok(())
        }),
    });
    wire_object!(@members out first {
        "gameplay": With(|out: &mut Vec<u8>| {
            wire_object!(out {
                "gameMode": gameplay.mode,
                "name": gameplay.player_name,
                "score": gameplay.score,
                "accuracy": gameplay.accuracy,
                "combo": {
                    "current": gameplay.combo,
                    "max": gameplay.max_combo,
                },
                "hp": {
                    "normal": gameplay.player_hp,
                    "smooth": gameplay.player_hp_smooth,
                },
                "hits": {
                    "0": legacy_hits.n0,
                    "50": legacy_hits.n50,
                    "100": legacy_hits.n100,
                    "300": legacy_hits.n300,
                    "geki": legacy_hits.geki,
                    "katu": legacy_hits.katu,
                    "sliderEndHits": legacy_hits.slider_end_hits,
                    "smallTickHits": legacy_hits.small_tick_hits,
                    "largeTickHits": legacy_hits.large_tick_hits,
                    "sliderBreaks": gameplay.slider_breaks,
                    "grade": {
                        "current": gameplay.grade_current,
                        "maxThisPlay": gameplay.grade_expected,
                    },
                    "unstableRate": gameplay.unstable_rate,
                    "hitErrorArray": precise.hit_errors.all.as_slice(),
                    "pressHitErrors": precise.hit_errors.press.as_slice(),
                    "releaseHitErrors": precise.hit_errors.release.as_slice(),
                },
                "pp": {
                    "current": fix_decimals(progressive.pp),
                    "fc": fix_decimals(progressive.fc_pp),
                    "maxThisPlay": fix_decimals(progressive.max_achievable),
                },
                "keyOverlay": {
                    "k1": key(0),
                    "k2": key(1),
                    "m1": key(2),
                    "m2": key(3),
                },
                "leaderboard": With(|out: &mut Vec<u8>| {
                    wire_object!(out {
                        "hasLeaderboard": !gameplay.leaderboard_scores.is_empty(),
                        "isVisible": gameplay.leaderboard_visible,
                        "ourplayer": With(|out: &mut Vec<u8>| {
                            leaderboard_player!(out, &gameplay.leaderboard_player);
                            Ok(())
                        }),
                        "slots": With(|out: &mut Vec<u8>| {
                            out.push(b'[');
                            for (index, player) in gameplay.leaderboard_scores.iter().enumerate() {
                                if index > 0 {
                                    out.push(b',');
                                }
                                leaderboard_player!(out, player);
                            }
                            out.push(b']');
                            Ok(())
                        }),
                    });
                    Ok(())
                }),
                "_isReplayUiHidden": regular.global.is_replay_ui_hidden,
            });
            Ok(())
        }),
        "resultsScreen": {
            "0": result.raw.statistics.miss,
            "50": result.raw.statistics.meh,
            "100": result.raw.statistics.ok,
            "300": result.raw.statistics.great,
            "mode": gameplay.mode,
            "name": result.raw.player_name,
            "score": result.raw.score,
            "accuracy": result.raw.accuracy,
            "maxCombo": result.raw.max_combo,
            "mods": ModsWire(&result.raw.mods),
            "geki": result.raw.statistics.perfect,
            "katu": result.raw.statistics.good,
            "grade": result.grade,
            "createdAt": result.raw.date,
        },
        "userProfile": With(|out: &mut Vec<u8>| write_user_profile(out, regular)),
        "tourney"?: tourney.map(|tourney| With(move |out: &mut Vec<u8>| write_tourney(out, tourney))),
    });
    wire_object!(@close out first);
    Ok(())
}

/// Writes the tournament DTO; the manager is always present, while the rare
/// IPC client list keeps its serde serialization.
fn write_tourney(out: &mut Vec<u8>, tourney: &Tourney) -> io::Result<()> {
    let manager = &tourney.manager;
    wire_object!(out {
        "manager": {
            "ipcState": manager.ipc_state,
            "bestOF": manager.best_of,
            "teamName": {
                "left": manager.team_name.left,
                "right": manager.team_name.right,
            },
            "stars": {
                "left": manager.stars.left,
                "right": manager.stars.right,
            },
            "bools": {
                "scoreVisible": manager.bools.score_visible,
                "starsVisible": manager.bools.stars_visible,
            },
            "chat": With(|out: &mut Vec<u8>| {
                out.push(b'[');
                for (index, message) in manager.chat.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    wire_object!(out {
                        "team": message.team,
                        "time": message.time,
                        "name": message.name,
                        "messageBody": message.message_body,
                    });
                }
                out.push(b']');
                Ok(())
            }),
            "gameplay": {
                "score": {
                    "left": manager.gameplay.score.left,
                    "right": manager.gameplay.score.right,
                },
            },
        },
        "ipcClients": Serde(&tourney.ipc_clients),
    });
    Ok(())
}

/// Mirrors [`user_profile`].
fn write_user_profile(out: &mut Vec<u8>, state: &RegularState) -> io::Result<()> {
    let Some(user) = state.user.profile.as_ref() else {
        out.extend_from_slice(b"{}");
        return Ok(());
    };
    wire_object!(out {
        "rawLoginStatus": user.raw_login_status,
        "name": user.name,
        "accuracy": user.accuracy,
        "rankedScore": user.ranked_score,
        "id": user.id,
        "level": user.level,
        "playCount": user.play_count,
        "playMode": user.play_mode,
        "rank": user.rank,
        "countryCode": user.country_code,
        "performancePoints": user.performance_points,
        "rawBanchoStatus": user.raw_bancho_status,
        "backgroundColour": With(|out: &mut Vec<u8>| {
            // Hex digits never need escaping.
            if user.background_colour < 0 {
                write!(out, "\"-{:x}\"", user.background_colour.unsigned_abs())
            } else {
                write!(out, "\"{:x}\"", user.background_colour)
            }
        }),
    });
    Ok(())
}

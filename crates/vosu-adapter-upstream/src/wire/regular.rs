//! Direct `/json/v2` wire writer for [`Response`].
//!
//! Writes every DTO member in exact Serde declaration order using
//! `vosu_adapter_support::json` primitives, matching `serde_json` + `JsFormatter`
//! byte-for-byte without Serde serializer dispatch overhead.

use std::io;

use bytes::Bytes;
use vosu_adapter_support::json::{Wire, With, wire_object};
use vosu_osu_model::memory::{RankedPlay, RankedPlayUserInfo};

use super::invalid;
use crate::{Error, dto::*};

pub(crate) fn regular(r: &Response) -> Result<Bytes, Error> {
    let capacity = 4096
        + r.play.hit_error_array.len() * 8
        + r.leaderboard.len() * 256
        + r.performance.graph.xaxis.len() * 8
        + r.performance
            .graph
            .series
            .iter()
            .map(|s| 32 + s.data.len() * 8)
            .sum::<usize>()
        + r.tourney
            .as_ref()
            .map_or(0, |t| 256 + t.chat.len() * 96 + t.clients.len() * 1024);
    let mut out = Vec::with_capacity(capacity);
    write_response(&mut out, r).map_err(invalid)?;
    Ok(Bytes::from(out))
}

fn write_response(out: &mut Vec<u8>, r: &Response) -> io::Result<()> {
    wire_object!(@open out first);
    wire_object!(@members out first {
        "game": r.game,
        "client": r.client,
        "server": r.server,
        "state": r.state,
        "session": r.session,
        "rankedPlay"?: r.ranked_play.as_ref().map(RankedPlayWire),
        "settings": r.settings,
        "profile": r.profile,
        "beatmap": r.beatmap,
    });
    wire_object!(@members out first {
        "play": r.play,
        "leaderboard": r.leaderboard,
        "performance": r.performance,
        "resultsScreen": r.results_screen,
        "folders": r.folders,
        "files": r.files,
        "directPath": r.direct_path,
        "tourney"?: r.tourney.as_ref(),
        "room"?: r.room.as_ref(),
    });
    wire_object!(@close out first);
    Ok(())
}

impl Wire for NumberName {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "number"?: self.number,
            "name": self.name,
        });
        Ok(())
    }
}

impl Wire for Game {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "focused"?: self.focused,
            "paused"?: self.paused,
        });
        Ok(())
    }
}

impl Wire for Session {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "playTime": self.play_time,
            "playCount": self.play_count,
        });
        Ok(())
    }
}

struct RankedPlayWire<'a>(&'a RankedPlay);

impl Wire for RankedPlayWire<'_> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        let rp = self.0;
        wire_object!(out {
            "stage": i64::from(rp.stage.value()),
            "currentRound": rp.current_round,
            "starRating": rp.star_rating,
            "damageMultiplier": rp.damage_multiplier,
            "activeUserId": rp.active_user_id,
            "winningUserId": rp.winning_user_id,
            "users": With(|out: &mut Vec<u8>| {
                out.push(b'[');
                for (i, user) in rp.users.iter().enumerate() {
                    if i != 0 {
                        out.push(b',');
                    }
                    write_ranked_play_user(out, user)?;
                }
                out.push(b']');
                Ok(())
            }),
        });
        Ok(())
    }
}

fn write_ranked_play_user(out: &mut Vec<u8>, u: &RankedPlayUserInfo) -> io::Result<()> {
    wire_object!(out {
        "id": u.id,
        "rating": u.rating,
        "life": u.life,
        "roundsWon": u.rounds_won,
        "damageMultiplier": u.damage_multiplier,
    });
    Ok(())
}

impl Wire for Settings {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(@open out first);
        wire_object!(@members out first {
            "interfaceVisible": self.interface_visible,
            "replayUIVisible": self.replay_ui_visible,
            "chatVisibilityStatus": self.chat_visibility_status,
            "leaderboard": {
                "visible": self.leaderboard.visible,
                "type": self.leaderboard.kind,
            },
            "progressBar": self.progress_bar,
            "bassDensity": self.bass_density,
            "resolution": {
                "fullscreen": self.resolution.fullscreen,
                "width": self.resolution.width,
                "height": self.resolution.height,
                "widthFullscreen": self.resolution.width_fullscreen,
                "heightFullscreen": self.resolution.height_fullscreen,
            },
            "client": {
                "updateAvailable": self.client.update_available,
                "branch": self.client.branch,
                "version": self.client.version,
            },
            "scoreMeter": {
                "type": self.score_meter.kind,
                "size": self.score_meter.size,
            },
            "cursor": {
                "useSkinCursor": self.cursor.use_skin_cursor,
                "autoSize": self.cursor.auto_size,
                "size": self.cursor.size,
                "menuSize": self.cursor.menu_size,
            },
        });
        wire_object!(@members out first {
            "mouse": {
                "rawInput": self.mouse.raw_input,
                "disableButtons": self.mouse.disable_buttons,
                "disableWheel": self.mouse.disable_wheel,
                "sensitivity": self.mouse.sensitivity,
            },
            "tablet": {
                "enabled": self.tablet.enabled,
                "x": self.tablet.x,
                "y": self.tablet.y,
                "width": self.tablet.width,
                "height": self.tablet.height,
                "rotation": self.tablet.rotation,
                "pressureThreshold": self.tablet.pressure_threshold,
            },
            "mania": {
                "speedBPMScale": self.mania.speed_bpm_scale,
                "usePerBeatmapSpeedScale": self.mania.use_per_beatmap_speed_scale,
                "scrollSpeed": self.mania.scroll_speed,
                "scrollDirection": {
                    "number": self.mania.scroll_direction.number,
                    "name"?: self.mania.scroll_direction.name.as_ref(),
                },
            },
            "sort": self.sort,
            "group": self.group,
            "skin": {
                "useDefaultSkinInEditor": self.skin.use_default_skin_in_editor,
                "ignoreBeatmapSkins": self.skin.ignore_beatmap_skins,
                "tintSliderBall": self.skin.tint_slider_ball,
                "useTaikoSkin": self.skin.use_taiko_skin,
                "name": self.skin.name,
            },
            "mode": self.mode,
            "audio": {
                "ignoreBeatmapSounds": self.audio.ignore_beatmap_sounds,
                "useSkinSamples": self.audio.use_skin_samples,
                "volume": {
                    "masterInactive": self.audio.volume.master_inactive,
                    "master": self.audio.volume.master,
                    "music": self.audio.volume.music,
                    "effect": self.audio.volume.effect,
                },
                "offset": {
                    "universal": self.audio.offset.universal,
                },
            },
            "background": {
                "storyboard": self.background.storyboard,
                "video": self.background.video,
                "blur": self.background.blur,
                "dim": self.background.dim,
            },
            "keybinds": {
                "osu": {
                    "k1": self.keybinds.osu.k1,
                    "k2": self.keybinds.osu.k2,
                    "smokeKey": self.keybinds.osu.smoke_key,
                },
                "fruits": {
                    "k1": self.keybinds.fruits.k1,
                    "k2": self.keybinds.fruits.k2,
                    "Dash": self.keybinds.fruits.dash,
                },
                "taiko": {
                    "innerLeft": self.keybinds.taiko.inner_left,
                    "innerRight": self.keybinds.taiko.inner_right,
                    "outerLeft": self.keybinds.taiko.outer_left,
                    "outerRight": self.keybinds.taiko.outer_right,
                },
                "quickRetry": self.keybinds.quick_retry,
            },
        });
        wire_object!(@close out first);
        Ok(())
    }
}

impl Wire for MatchmakingStats {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "rating": self.rating,
            "rank": self.rank,
            "plays": self.plays,
            "wins": self.wins,
            "isProvisional": self.is_provisional,
        });
        Ok(())
    }
}

impl Wire for Profile {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(@open out first);
        wire_object!(@members out first {
            "userStatus"?: self.user_status.as_ref(),
            "banchoStatus"?: self.bancho_status.as_ref(),
            "id"?: self.id,
            "name"?: self.name.as_ref(),
            "mode"?: self.mode.as_ref(),
            "rankedScore"?: self.ranked_score,
            "level"?: self.level,
        });
        wire_object!(@members out first {
            "accuracy"?: self.accuracy,
            "pp"?: self.pp,
            "playCount"?: self.play_count,
            "globalRank"?: self.global_rank,
            "countryCode"?: self.country_code.as_ref(),
            "backgroundColour"?: self.background_colour.as_ref(),
            "matchmaking"?: self.matchmaking.as_ref(),
        });
        wire_object!(@close out first);
        Ok(())
    }
}

impl Wire for Stars {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(@open out first);
        wire_object!(@members out first {
            "live": self.live,
            "aim"?: self.aim,
            "speed"?: self.speed,
            "flashlight"?: self.flashlight,
            "sliderFactor"?: self.slider_factor,
            "stamina"?: self.stamina,
        });
        wire_object!(@members out first {
            "rhythm"?: self.rhythm,
            "color"?: self.color,
            "reading"?: self.reading,
            "hitWindow"?: self.hit_window,
            "total": self.total,
        });
        wire_object!(@close out first);
        Ok(())
    }
}

impl Wire for Stats {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "stars": self.stars,
            "ar": { "original": self.ar.original, "converted": self.ar.converted },
            "cs": { "original": self.cs.original, "converted": self.cs.converted },
            "od": { "original": self.od.original, "converted": self.od.converted },
            "hp": { "original": self.hp.original, "converted": self.hp.converted },
            "bpm": {
                "realtime": self.bpm.realtime,
                "common": self.bpm.common,
                "min": self.bpm.min,
                "max": self.bpm.max,
            },
            "objects": {
                "circles": self.objects.circles,
                "sliders": self.objects.sliders,
                "spinners": self.objects.spinners,
                "holds": self.objects.holds,
                "total": self.objects.total,
            },
            "hitWindow": {
                "miss"?: self.hit_window.miss,
                "meh"?: self.hit_window.meh,
                "ok"?: self.hit_window.ok,
                "good"?: self.hit_window.good,
                "great"?: self.hit_window.great,
                "perfect"?: self.hit_window.perfect,
            },
            "maxCombo": self.max_combo,
        });
        Ok(())
    }
}

impl Wire for Beatmap {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(@open out first);
        wire_object!(@members out first {
            "isKiai": self.is_kiai,
            "isBreak": self.is_break,
            "isConvert": self.is_convert,
            "time": {
                "live": self.time.live,
                "firstObject": self.time.first_object,
                "lastObject": self.time.last_object,
                "mp3Length"?: self.time.mp3_length,
            },
            "status": {
                "number"?: self.status.number,
                "name": self.status.name,
            },
            "checksum"?: self.checksum.as_ref(),
            "id"?: self.id,
            "set"?: self.set,
            "mode": self.mode,
        });
        wire_object!(@members out first {
            "artist"?: self.artist.as_ref(),
            "artistUnicode"?: self.artist_unicode.as_ref(),
            "title"?: self.title.as_ref(),
            "titleUnicode"?: self.title_unicode.as_ref(),
            "mapper"?: self.mapper.as_ref(),
            "version"?: self.version.as_ref(),
            "source": self.source,
            "tags": self.tags,
            "stats": self.stats,
        });
        wire_object!(@close out first);
        Ok(())
    }
}

impl Wire for PlayHits {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        match self {
            PlayHits::Normal(h) => {
                wire_object!(out {
                    "0": h.h0,
                    "50": h.h50,
                    "100": h.h100,
                    "300": h.h300,
                    "geki": h.geki,
                    "katu": h.katu,
                    "sliderBreaks": h.slider_breaks,
                    "sliderEndHits": h.slider_end_hits,
                    "smallTickHits": h.small_tick_hits,
                    "largeTickHits": h.large_tick_hits,
                });
            }
            PlayHits::Lazer(h) => {
                wire_object!(out {
                    "0": h.h0,
                    "50": h.h50,
                    "100": h.h100,
                    "300": h.h300,
                    "geki": h.geki,
                    "katu": h.katu,
                    "sliderEndHits": h.slider_end_hits,
                    "smallTickHits": h.small_tick_hits,
                    "largeTickHits": h.large_tick_hits,
                    "sliderBreaks": h.slider_breaks,
                });
            }
        }
        Ok(())
    }
}

impl Wire for DetailedPpAttributes {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "aim": self.aim,
            "speed": self.speed,
            "accuracy": self.accuracy,
            "difficulty": self.difficulty,
            "flashlight": self.flashlight,
            "total": self.total,
        });
        Ok(())
    }
}

impl<V: Wire> Wire for Play<V> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(@open out first);
        wire_object!(@members out first {
            "failed": self.failed,
            "playerName": self.player_name,
            "mode": self.mode,
            "score": self.score,
            "accuracy": self.accuracy,
            "healthBar": {
                "normal": self.health_bar.normal,
                "smooth": self.health_bar.smooth,
            },
            "hits": self.hits,
        });
        wire_object!(@members out first {
            "hitErrorArray": self.hit_error_array,
            "combo": {
                "current": self.combo.current,
                "max": self.combo.max,
            },
            "mods": self.mods,
            "rank": {
                "current": self.rank.current,
                "maxThisPlay": self.rank.max_this_play,
            },
            "pp": {
                "current": self.pp.current,
                "fc": self.pp.fc,
                "maxAchieved": self.pp.max_achieved,
                "maxAchievable": self.pp.max_achievable,
                "detailed": {
                    "current": self.pp.detailed.current,
                    "fc": self.pp.detailed.fc,
                },
            },
            "unstableRate": self.unstable_rate,
        });
        wire_object!(@close out first);
        Ok(())
    }
}

impl Wire for Leaderboard {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(@open out first);
        wire_object!(@members out first {
            "isFailed": self.is_failed,
            "position": self.position,
            "team": self.team,
            "id": self.id,
            "name": self.name,
            "score": self.score,
            "accuracy": self.accuracy,
        });
        wire_object!(@members out first {
            "hits": {
                "0": self.hits.h0,
                "50": self.hits.h50,
                "100": self.hits.h100,
                "300": self.hits.h300,
                "geki": self.hits.geki,
                "katu": self.hits.katu,
            },
            "combo": {
                "current": self.combo.current,
                "max": self.combo.max,
            },
            "mods": self.mods,
            "rank": self.rank,
        });
        wire_object!(@close out first);
        Ok(())
    }
}

impl Wire for Series {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "name": self.name,
            "data": self.data,
        });
        Ok(())
    }
}

impl Wire for Performance {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "accuracy": {
                "90": self.accuracy.acc_90,
                "91": self.accuracy.acc_91,
                "92": self.accuracy.acc_92,
                "93": self.accuracy.acc_93,
                "94": self.accuracy.acc_94,
                "95": self.accuracy.acc_95,
                "96": self.accuracy.acc_96,
                "97": self.accuracy.acc_97,
                "98": self.accuracy.acc_98,
                "99": self.accuracy.acc_99,
                "100": self.accuracy.acc_100,
            },
            "graph": {
                "series": self.graph.series,
                "xaxis": self.graph.xaxis,
            },
        });
        Ok(())
    }
}

impl Wire for ResultsScreen {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(@open out first);
        wire_object!(@members out first {
            "scoreId": self.score_id,
            "playerName": self.player_name,
            "mode": self.mode,
            "score": self.score,
            "accuracy": self.accuracy,
            "name": self.name,
        });
        wire_object!(@members out first {
            "hits": {
                "0": self.hits.h0,
                "50": self.hits.h50,
                "100": self.hits.h100,
                "300": self.hits.h300,
                "geki": self.hits.geki,
                "katu": self.hits.katu,
                "sliderEndHits": self.hits.slider_end_hits,
                "smallTickHits": self.hits.small_tick_hits,
                "largeTickHits": self.hits.large_tick_hits,
            },
            "mods": self.mods,
            "maxCombo": self.max_combo,
            "rank": self.rank,
            "pp": {
                "current": self.pp.current,
                "fc": self.pp.fc,
            },
            "createdAt": self.created_at,
        });
        wire_object!(@close out first);
        Ok(())
    }
}

impl Wire for Folders {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "game": self.game,
            "skin": self.skin,
            "songs": self.songs,
            "beatmap"?: self.beatmap.as_ref(),
        });
        Ok(())
    }
}

impl Wire for Files {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "beatmap"?: self.beatmap.as_ref(),
            "background"?: self.background.as_ref(),
            "audio"?: self.audio.as_ref(),
        });
        Ok(())
    }
}

impl Wire for DirectPath {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "beatmapFile": self.beatmap_file,
            "beatmapBackground": self.beatmap_background,
            "beatmapAudio": self.beatmap_audio,
            "beatmapFolder"?: self.beatmap_folder.as_ref(),
            "skinFolder": self.skin_folder,
        });
        Ok(())
    }
}

impl<T: Wire> Wire for Sides<T> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "left": self.left,
            "right": self.right,
        });
        Ok(())
    }
}

impl Wire for TourneyChatMessage {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        match self {
            TourneyChatMessage::Stable(m) => {
                wire_object!(out {
                    "team": m.team,
                    "name": m.name,
                    "message": m.message,
                    "timestamp": m.timestamp,
                });
            }
            TourneyChatMessage::Lazer(m) => {
                wire_object!(out {
                    "team": m.team,
                    "timestamp": m.timestamp,
                    "name": m.name,
                    "message": m.message,
                });
            }
        }
        Ok(())
    }
}

impl Wire for TourneyTeam {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        out.extend_from_slice(match self {
            TourneyTeam::Left => b"\"left\"",
            TourneyTeam::Right => b"\"right\"",
        });
        Ok(())
    }
}

impl Wire for TourneyClient {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "ipcId": self.ipc_id,
            "team": self.team,
            "settings": {
                "mania": {
                    "scrollSpeed": self.settings.mania.scroll_speed,
                },
            },
            "user": {
                "id": self.user.id,
                "name": self.user.name,
                "country": self.user.country,
                "accuracy": self.user.accuracy,
                "rankedScore": self.user.ranked_score,
                "playCount": self.user.play_count,
                "globalRank": self.user.global_rank,
                "totalPP": self.user.total_pp,
            },
            "beatmap": {
                "stats": self.beatmap.stats,
            },
            "play": self.play,
        });
        Ok(())
    }
}

impl Wire for Tourney {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "scoreVisible": self.score_visible,
            "starsVisible": self.stars_visible,
            "ipcState": self.ipc_state,
            "bestOF": self.best_of,
            "team": self.team,
            "points": self.points,
            "chat": self.chat,
            "totalScore": self.total_score,
            "clients": self.clients,
        });
        Ok(())
    }
}

impl Wire for RoomUser {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "id": self.id,
            "username": self.username,
            "countryCode": self.country_code,
            "avatarUrl"?: self.avatar_url.as_ref(),
        });
        Ok(())
    }
}

impl Wire for Room {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "roomID": self.room_id,
            "users": self.users,
        });
        Ok(())
    }
}

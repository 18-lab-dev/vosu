//! Direct `/json/v2` wire writer.
//!
//! The unfiltered V2 document is the largest cold encode. Materializing the
//! [`Response`] DTO first allocates every name and string only to serialize
//! and drop it, so `encode` writes the same bytes straight from the state.
//! [`build`](super::build) stays the owned contract for filtered projection
//! and subscriptions; debug builds check every direct encode against it.

use std::io::{self, Write};

use super::*;
use crate::api::json::{Serde, Wire, With, wire_object};
use crate::model::states::{Mod, Statistics};

/// `{"number":…,"name":…}` with the number omitted when absent.
struct NumberNameWire<'a>(Option<i64>, &'a str);

impl Wire for NumberNameWire<'_> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out { "number"?: self.0, "name": self.1 });
        Ok(())
    }
}

impl Wire for CalculateMods {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "checksum": self.checksum,
            "number": self.number,
            "name": self.name,
            "array": self.array,
            "rate": self.rate,
        });
        Ok(())
    }
}

impl Wire for Mod {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(out {
            "acronym": self.acronym,
            "settings"?: self.settings.as_ref().map(Serde),
        });
        Ok(())
    }
}

/// Encodes the unfiltered V2 document exactly as `build_internal(_, false)`
/// serializes through [`JsFormatter`](crate::api::json::JsFormatter).
pub(super) fn encode(snapshot: &CombinedSnapshot) -> Result<Vec<u8>, ApiError> {
    let focused = snapshot.focused().ok_or(ApiError::NotReady)?;
    let state = focused.regular.as_ref();
    let source = focused.source;
    let status = focused.precise.status;
    let manager_instance = snapshot
        .manager()
        .focused_instance()
        .ok_or(ApiError::NotReady)?;
    // Same fallible steps, in the same order, as `build_internal`.
    let graph = encode_graph_once(&state.beatmap.graph)?;
    let tourney = build_tourney(snapshot, source, state, status)?;

    let leaderboard = &state.gameplay.leaderboard_scores;
    let hit_errors = &focused.precise.hit_errors;
    let mut out = Vec::with_capacity(
        6 * 1024
            + graph.get().len()
            + 640 * leaderboard.len()
            + 96 * focused.precise.keypresses.len()
            + 24 * (hit_errors.all.len() + hit_errors.press.len() + hit_errors.release.len()),
    );
    let document = Document {
        snapshot,
        state,
        source,
        status,
        manager_instance,
        precise: &focused.precise,
        graph: graph.get(),
        tourney: tourney.as_ref(),
    };
    document
        .write(&mut out)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(out)
}

struct Document<'a> {
    snapshot: &'a CombinedSnapshot,
    state: &'a RegularState,
    source: ClientType,
    status: i64,
    manager_instance: &'a ManagerInstanceSnapshot,
    precise: &'a crate::state::PreciseState,
    graph: &'a str,
    tourney: Option<&'a Tourney>,
}

impl Document<'_> {
    fn write(&self, out: &mut Vec<u8>) -> io::Result<()> {
        let state = self.state;
        let precise = self.precise;
        let current_mods = select_mods(state, self.status);
        let current_mode = select_mode(state, self.status);
        let parsed = state.beatmap.parsed.as_deref();
        let menu = &state.menu;
        let global = &state.global;
        let server = self.manager_instance.settings().devserver_endpoint.as_ref();
        wire_object!(out {
            "game": {
                "focused"?: cfg!(target_os = "windows")
                    .then(|| self.snapshot.manager().game_focused()),
                "paused": global.paused,
            },
            "client": client_name(self.source),
            "server": server.map_or(std::borrow::Cow::Borrowed("ppy.sh"), |v| v.to_string_lossy()),
            "state": NumberNameWire(Some(self.status), game_state_name(self.status)),
            "session": {
                "playTime": global.game_time as f64,
                "playCount": 0_i64,
            },
            "rankedPlay"?: state.ranked_play.info.as_ref().map(Serde),
            "settings": With(|out: &mut Vec<u8>| write_settings(out, state, self.source)),
            "profile": With(|out: &mut Vec<u8>| write_profile(out, state.user.profile.as_ref())),
            "beatmap": With(|out: &mut Vec<u8>| {
                write_beatmap(out, state, parsed, current_mode, precise.play_time)
            }),
            "play": With(|out: &mut Vec<u8>| {
                write_play(out, &state.gameplay, state.beatmap.attributes.as_ref(), current_mods, precise, self.snapshot.runtime_settings().enable_mania_key_hit_errors, self.snapshot.runtime_settings().enable_hit_offsets)
            }),
            "keys": With(|out: &mut Vec<u8>| {
                crate::api::precise::write_keys(out, &precise.key_overlay, &precise.keypresses)
            }),
            "leaderboard": With(|out: &mut Vec<u8>| write_leaderboard(out, &state.gameplay.leaderboard_scores)),
            "performance": {
                "accuracy": With(|out: &mut Vec<u8>| write_accuracy(out, state)),
                "graph": RawJson(self.graph),
            },
            "resultsScreen": With(|out: &mut Vec<u8>| write_results(out, state)),
            "folders": {
                "game": global.game_folder,
                "skin": global.skin_folder,
                "songs": global.songs_folder,
                "beatmap"?: menu.folder.as_ref(),
            },
            "files": {
                "beatmap"?: menu.filename.as_ref(),
                "background"?: menu.background_filename.as_ref(),
                "audio"?: menu.audio_filename.as_ref(),
            },
            "directPath": {
                "beatmapFile": direct_join(menu.folder.as_deref(), menu.filename.as_deref()),
                "beatmapBackground": direct_join(
                    menu.folder.as_deref(),
                    menu.background_filename.as_deref(),
                ),
                "beatmapAudio": direct_join(menu.folder.as_deref(), menu.audio_filename.as_deref()),
                "beatmapFolder"?: menu.folder.as_ref(),
                "skinFolder": global.skin_folder,
            },
            "tourney"?: self.tourney.map(|tourney| With(move |out: &mut Vec<u8>| write_tourney(out, tourney))),
            "room"?: state.room.info.as_ref().map(|room| With(move |out: &mut Vec<u8>| {
                wire_object!(out {
                    "roomID": room.room_id,
                    "users": With(|out: &mut Vec<u8>| {
                        out.push(b'[');
                        for (index, user) in room.users.iter().enumerate() {
                            if index > 0 {
                                out.push(b',');
                            }
                            wire_object!(out {
                                "id": user.id,
                                "username": user.username,
                                "countryCode": user.country_code,
                                "avatarUrl"?: user.avatar_url.as_ref(),
                            });
                        }
                        out.push(b']');
                        Ok(())
                    }),
                });
                Ok(())
            })),
        });
        Ok(())
    }
}

/// Already encoded JSON copied verbatim, as `RawValue` serializes.
struct RawJson<'a>(&'a str);

impl Wire for RawJson<'_> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        out.extend_from_slice(self.0.as_bytes());
        Ok(())
    }
}

/// Maps up to this size are searched linearly; see [`child`].
const LINEAR_LOOKUP_LIMIT: usize = 32;

/// `Value::get(key)` for an object.
///
/// Settings objects are small (15 top-level keys, a handful per group) and
/// their keys are unique, so the first equal key in a scan is the entry `get`
/// finds without hashing each of the ~60 looked-up keys. Larger maps, which
/// the settings reader does not produce, keep the hashed lookup so the cost
/// stays bounded.
fn child<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    let map = value?.as_object()?;
    if map.len() <= LINEAR_LOOKUP_LIMIT {
        map.iter()
            .find_map(|(name, value)| (name == key).then_some(value))
    } else {
        map.get(key)
    }
}

fn setting_bool(value: Option<&Value>) -> bool {
    value.and_then(Value::as_bool).unwrap_or(false)
}

fn setting_i64(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_i64)
        .or_else(|| value.and_then(Value::as_f64).map(|v| v as i64))
        .unwrap_or(0)
}

fn setting_f64(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Null) => f64::NAN,
        Some(value) => value.as_f64().unwrap_or(0.0),
        None => 0.0,
    }
}

fn setting_str(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or("")
}

/// Mirrors `build_settings`; each nested group is looked up once, which is
/// the same path walk `get` performs per leaf.
fn write_settings(out: &mut Vec<u8>, state: &RegularState, source: ClientType) -> io::Result<()> {
    let s = &state.settings.values;
    let top = |key: &str| {
        if s.len() <= LINEAR_LOOKUP_LIMIT {
            s.iter()
                .find_map(|(name, value)| (name == key).then_some(value))
        } else {
            s.get(key)
        }
    };
    let leaderboard_type = setting_i64(top("leaderboardType"));
    let score_meter = top("scoreMeter");
    let score_meter_type = setting_i64(child(score_meter, "type"));
    let mania = top("mania");
    let scroll_direction = setting_i64(child(mania, "scrollDirection"));
    let sort = setting_i64(top("sortType"));
    let group = setting_i64(top("groupType"));
    let progress_bar = setting_i64(top("progressBarType"));
    let resolution = top("resolution");
    let client = top("client");
    let cursor = top("cursor");
    let mouse = top("mouse");
    let tablet = top("tablet");
    let skin = top("skin");
    let audio = top("audio");
    let volume = child(audio, "volume");
    let background = top("background");
    let keybinds = top("keybinds");
    let osu = child(keybinds, "osu");
    let fruits = child(keybinds, "fruits");
    let taiko = child(keybinds, "taiko");
    let lazer = source == ClientType::Lazer;
    let scroll_name = mania_scroll_name(scroll_direction);
    wire_object!(out {
        "interfaceVisible": state.global.show_interface,
        "replayUIVisible": !state.global.is_replay_ui_hidden,
        "chatVisibilityStatus": NumberNameWire(
            Some(state.global.chat_status),
            chat_status_name(state.global.chat_status),
        ),
        "leaderboard": {
            "visible": state.gameplay.leaderboard_visible,
            "type": NumberNameWire(
                Some(leaderboard_type),
                if lazer {
                    lazer_leaderboard_name(leaderboard_type)
                } else {
                    leaderboard_name(leaderboard_type)
                },
            ),
        },
        "progressBar": NumberNameWire(Some(progress_bar), progress_bar_name(progress_bar)),
        "bassDensity": state.bass_density.density,
        "resolution": {
            "fullscreen": setting_bool(child(resolution, "fullscreen")),
            "width": setting_i64(child(resolution, "width")),
            "height": setting_i64(child(resolution, "height")),
            "widthFullscreen": setting_i64(child(resolution, "widthFullscreen")),
            "heightFullscreen": setting_i64(child(resolution, "heightFullscreen")),
        },
        "client": {
            "updateAvailable": setting_bool(child(client, "updateAvailable")),
            "branch": setting_i64(child(client, "branch")),
            "version": setting_str(child(client, "version")),
        },
        "scoreMeter": {
            "type": NumberNameWire(Some(score_meter_type), score_meter_name(score_meter_type)),
            "size": setting_f64(child(score_meter, "size")),
        },
        "cursor": {
            "useSkinCursor": setting_bool(child(cursor, "useSkinCursor")),
            "autoSize": setting_bool(child(cursor, "autoSize")),
            "size": setting_f64(child(cursor, "size")),
            "menuSize": setting_f64(child(cursor, "menuSize")),
        },
        "mouse": {
            "rawInput": setting_bool(child(mouse, "rawInput")),
            "disableButtons": setting_bool(child(mouse, "disableButtons")),
            "disableWheel": setting_bool(child(mouse, "disableWheel")),
            "sensitivity": setting_f64(child(mouse, "sensitivity")),
        },
        "tablet": {
            "enabled": setting_bool(child(tablet, "enabled")),
            "x": setting_f64(child(tablet, "x")),
            "y": setting_f64(child(tablet, "y")),
            "width": setting_f64(child(tablet, "width")),
            "height": setting_f64(child(tablet, "height")),
            "rotation": setting_f64(child(tablet, "rotation")),
            "pressureThreshold": setting_f64(child(tablet, "pressureThreshold")),
        },
        "mania": {
            "speedBPMScale": setting_bool(child(mania, "speedBPMScale")),
            "usePerBeatmapSpeedScale": setting_bool(child(mania, "usePerBeatmapSpeedScale")),
            "scrollSpeed": setting_i64(child(mania, "scrollSpeed")),
            "scrollDirection": {
                "number": scroll_direction,
                "name"?: (!scroll_name.is_empty()).then_some(scroll_name),
            },
        },
        "sort": NumberNameWire(Some(sort), if lazer { lazer_sort_name(sort) } else { sort_name(sort) }),
        "group": NumberNameWire(
            Some(group),
            if lazer { lazer_group_name(group) } else { group_name(group) },
        ),
        "skin": {
            "useDefaultSkinInEditor": setting_bool(child(skin, "useDefaultSkinInEditor")),
            "ignoreBeatmapSkins": setting_bool(child(skin, "ignoreBeatmapSkins")),
            "tintSliderBall": setting_bool(child(skin, "tintSliderBall")),
            "useTaikoSkin": setting_bool(child(skin, "useTaikoSkin")),
            "name": setting_str(child(skin, "name")),
        },
        "mode": NumberNameWire(state.menu.gamemode, state.menu.gamemode.map_or("", ruleset_name)),
        "audio": {
            "ignoreBeatmapSounds": setting_bool(child(audio, "ignoreBeatmapSounds")),
            "useSkinSamples": setting_bool(child(audio, "useSkinSamples")),
            "volume": {
                "masterInactive": setting_f64(child(volume, "masterInactive")),
                "master": setting_f64(child(volume, "master")),
                "music": setting_f64(child(volume, "music")),
                "effect": setting_f64(child(volume, "effect")),
            },
            "offset": {
                "universal": setting_f64(child(child(audio, "offset"), "universal")),
            },
        },
        "background": {
            "storyboard": setting_bool(child(background, "storyboard")),
            "video": setting_bool(child(background, "video")),
            "blur": setting_f64(child(background, "blur")),
            "dim": setting_f64(child(background, "dim")),
        },
        "keybinds": {
            "osu": {
                "k1": setting_str(child(osu, "k1")),
                "k2": setting_str(child(osu, "k2")),
                "smokeKey": setting_str(child(osu, "smokeKey")),
            },
            "fruits": {
                "k1": setting_str(child(fruits, "k1")),
                "k2": setting_str(child(fruits, "k2")),
                "Dash": setting_str(child(fruits, "Dash")),
            },
            "taiko": {
                "innerLeft": setting_str(child(taiko, "innerLeft")),
                "innerRight": setting_str(child(taiko, "innerRight")),
                "outerLeft": setting_str(child(taiko, "outerLeft")),
                "outerRight": setting_str(child(taiko, "outerRight")),
            },
            "quickRetry": setting_str(child(keybinds, "quickRetry")),
        },
    });
    Ok(())
}

/// Mirrors `build_profile`.
fn write_profile(out: &mut Vec<u8>, user: Option<&User>) -> io::Result<()> {
    let Some(user) = user else {
        out.extend_from_slice(
            br#"{"userStatus":{"name":""},"banchoStatus":{"name":""},"mode":{"name":""},"countryCode":{"name":""}}"#,
        );
        return Ok(());
    };
    wire_object!(out {
        "userStatus": NumberNameWire(Some(user.raw_login_status), user_login_name(user.raw_login_status)),
        "banchoStatus": NumberNameWire(Some(user.raw_bancho_status), bancho_name(user.raw_bancho_status)),
        "id": user.id,
        "name": user.name,
        "mode": NumberNameWire(Some(user.play_mode), ruleset_name(user.play_mode)),
        "rankedScore": user.ranked_score,
        "level": user.level,
        "accuracy": user.accuracy,
        "pp": user.performance_points,
        "playCount": user.play_count,
        "globalRank": user.rank,
        "countryCode": NumberNameWire(Some(user.country_code), source_country_name(user.country_code)),
        "backgroundColour": With(|out: &mut Vec<u8>| {
            // Hex digits never need escaping.
            if user.background_colour < 0 {
                write!(out, "\"-{:x}\"", user.background_colour.unsigned_abs())
            } else {
                write!(out, "\"{:x}\"", user.background_colour)
            }
        }),
        "matchmaking": user.matchmaking.as_ref().map(|matchmaking| With(move |out: &mut Vec<u8>| {
            wire_object!(out {
                "rating": matchmaking.rating,
                "rank": matchmaking.rank,
                "plays": matchmaking.plays,
                "wins": matchmaking.wins,
                "isProvisional": matchmaking.is_provisional,
            });
            Ok(())
        })),
    });
    Ok(())
}

/// Mirrors `build_beatmap`.
fn write_beatmap(
    out: &mut Vec<u8>,
    state: &RegularState,
    parsed: Option<&ParsedBeatmap>,
    current_mode: Option<i64>,
    live_time: f64,
) -> io::Result<()> {
    let mode = Some(state.beatmap.mode.unwrap_or(0));
    let ranked_status = state.menu.ranked_status;
    let menu = &state.menu;
    wire_object!(out {
        "isKiai": state.beatmap.event_status.kiai,
        "isBreak": state.beatmap.event_status.break_time,
        "isConvert": mode == Some(0) && mode != current_mode,
        "time": {
            "live": live_time,
            "firstObject": state.beatmap.first_object_time.unwrap_or(0),
            "lastObject": parsed.map_or(0.0, |p| p.times.full),
            "mp3Length"?: menu.mp3_length,
        },
        "status": {
            "number"?: ranked_status,
            "name": ranked_status
                .filter(|value| value.is_finite() && *value != 0.0)
                .map_or("", |value| beatmap_status_name(value as i64)),
        },
        "checksum"?: menu.checksum.as_ref(),
        "id"?: menu.map_id,
        "set"?: menu.set_id,
        "mode": NumberNameWire(mode, mode.map_or("", ruleset_name)),
        "artist"?: menu.artist.as_ref(),
        "artistUnicode"?: menu.artist_original.as_ref(),
        "title"?: menu.title.as_ref(),
        "titleUnicode"?: menu.title_original.as_ref(),
        "mapper"?: menu.creator.as_ref(),
        "version"?: menu.difficulty.as_ref(),
        "source": parsed.map_or("", |p| p.metadata.source.as_str()),
        "tags": parsed.map_or("", |p| p.metadata.tags.as_str()),
        "stats": With(|out: &mut Vec<u8>| write_stats(out, state, parsed)),
    });
    Ok(())
}

/// Mirrors `build_stats`.
fn write_stats(
    out: &mut Vec<u8>,
    state: &RegularState,
    parsed: Option<&ParsedBeatmap>,
) -> io::Result<()> {
    let attrs = parsed.map(|p| &p.engine.difficulty);
    let original = parsed.map(|p| &p.engine.original_difficulty);
    let converted = parsed.map(|p| &p.engine.converted_difficulty);
    let counts = parsed.map(|p| p.object_counts);
    let circles = counts.map_or(0, |v| i64::from(v.circles));
    let sliders = counts.map_or(0, |v| i64::from(v.sliders));
    let spinners = counts.map_or(0, |v| i64::from(v.spinners));
    let holds = counts.map_or(0, |v| i64::from(v.holds));
    let windows = parsed.map(|p| &p.engine.hit_windows);
    wire_object!(out {
        "stars": {
            "live": fix_decimals(state.beatmap.attributes.map_or(0.0, |v| v.stars)),
            "aim"?: truthy(attrs.and_then(|v| v.aim)),
            "speed"?: truthy(attrs.and_then(|v| v.speed)),
            "flashlight"?: truthy(attrs.and_then(|v| v.flashlight)),
            "sliderFactor"?: truthy(attrs.and_then(|v| v.slider_factor)),
            "stamina"?: truthy(attrs.and_then(|v| v.stamina)),
            "rhythm"?: truthy(attrs.and_then(|v| v.rhythm)),
            "color"?: truthy(attrs.and_then(|v| v.color)),
            "reading"?: truthy(attrs.and_then(|v| v.reading)),
            "hitWindow"?: truthy(parsed.and_then(|p| p.engine.hit_windows.great)).map(fix_decimals),
            "total": fix_decimals(attrs.map_or(0.0, |v| v.stars)),
        },
        "ar": {
            "original": fix_decimals(original.map_or(0.0, |v| v.approach_rate)),
            "converted": fix_decimals(converted.map_or(0.0, |v| v.approach_rate)),
        },
        "cs": {
            "original": fix_decimals(original.map_or(0.0, |v| v.circle_size)),
            "converted": fix_decimals(converted.map_or(0.0, |v| v.circle_size)),
        },
        "od": {
            "original": fix_decimals(original.map_or(0.0, |v| v.overall_difficulty)),
            "converted": fix_decimals(converted.map_or(0.0, |v| v.overall_difficulty)),
        },
        "hp": {
            "original": fix_decimals(original.map_or(0.0, |v| v.drain_rate)),
            "converted": fix_decimals(converted.map_or(0.0, |v| v.drain_rate)),
        },
        "bpm": {
            "realtime": fix_decimals_to(state.beatmap.event_status.bpm, 4)
                .unwrap_or(state.beatmap.event_status.bpm),
            "common": fix_decimals_to(parsed.map_or(0.0, |p| p.bpm.common), 4).unwrap_or(0.0),
            "min": fix_decimals_to(parsed.map_or(0.0, |p| p.bpm.minimum), 4).unwrap_or(0.0),
            "max": fix_decimals_to(parsed.map_or(0.0, |p| p.bpm.maximum), 4).unwrap_or(0.0),
        },
        "objects": {
            "circles": circles,
            "sliders": sliders,
            "spinners": spinners,
            "holds": holds,
            "total": circles + sliders + spinners + holds,
        },
        "hitWindow": {
            "miss"?: windows.and_then(|w| w.miss),
            "meh"?: windows.and_then(|w| w.meh),
            "ok"?: windows.and_then(|w| w.ok),
            "good"?: windows.and_then(|w| w.good),
            "great"?: windows.and_then(|w| w.great),
            "perfect"?: windows.and_then(|w| w.perfect),
        },
        "maxCombo": parsed.map_or(0, |v| i64::from(v.engine.difficulty.max_combo)),
    });
    Ok(())
}

fn write_detailed(
    out: &mut Vec<u8>,
    a: crate::calculation::progressive::PerformanceAttributes,
    total: f64,
) -> io::Result<()> {
    wire_object!(out {
        "aim": fix_decimals(a.aim),
        "speed": fix_decimals(a.speed),
        "accuracy": fix_decimals(a.accuracy),
        "difficulty": fix_decimals(a.difficulty),
        "flashlight": fix_decimals(a.flashlight),
        "total": fix_decimals(total),
    });
    Ok(())
}

/// Mirrors `build_play` with borrowed hit-error slices.
fn write_play(
    out: &mut Vec<u8>,
    gameplay: &GameplayState,
    attrs: Option<&crate::calculation::progressive::ProgressiveAttributes>,
    mods: &CalculateMods,
    precise: &crate::state::PreciseState,
    enable_key_hits: bool,
    enable_hit_offsets: bool,
) -> io::Result<()> {
    let precise_hits = &precise.hit_errors;
    let a = attrs.copied().unwrap_or_default();
    let s: &Statistics = &gameplay.statistics;
    let empty = crate::model::states::KeyHitErrors::default();
    wire_object!(out {
        "failed": gameplay.failed,
        "playerName": gameplay.player_name,
        "mode": NumberNameWire(Some(gameplay.mode), ruleset_name(gameplay.mode)),
        "score": gameplay.score,
        "accuracy": gameplay.accuracy,
        "healthBar": {
            // Preserve upstream operation order: `/ 2` can differ by one ULP.
            "normal": (gameplay.player_hp / 200.0) * 100.0,
            "smooth": (gameplay.player_hp_smooth / 200.0) * 100.0,
        },
        "hits": {
            "0": s.miss,
            "50": s.meh,
            "100": s.ok,
            "300": s.great,
            "geki": s.perfect,
            "katu": s.good,
            "sliderBreaks": gameplay.slider_breaks,
            "sliderEndHits": s.slider_tail_hit,
            "smallTickHits": s.small_tick_hit,
            "largeTickHits": s.large_tick_hit,
        },
        "hitErrorArray": precise_hits.all.as_slice(),
        "pressHitErrors": precise_hits.press.as_slice(),
        "releaseHitErrors": precise_hits.release.as_slice(),
        "hitOffsets"?: enable_hit_offsets.then(|| precise.hit_offsets()),
        "keyHitErrors"?: enable_key_hits.then(|| Serde(precise_hits.key.as_ref().unwrap_or(&empty))),
        "catcher"?: precise.active_catcher(),
        "combo": {
            "current": gameplay.combo,
            "max": gameplay.max_combo,
        },
        "mods": mods,
        "rank": {
            "current": gameplay.grade_current,
            "maxThisPlay": gameplay.grade_expected,
        },
        "pp": {
            "current": fix_decimals(a.pp),
            "fc": fix_decimals(a.fc_pp),
            "maxAchieved": fix_decimals(a.max_achieved),
            "maxAchievable": fix_decimals(a.max_achievable),
            "detailed": {
                "current": With(|out: &mut Vec<u8>| write_detailed(out, a.current, a.pp)),
                "fc": With(|out: &mut Vec<u8>| write_detailed(out, a.fc, a.fc_pp)),
            },
        },
        "unstableRate": gameplay.unstable_rate,
    });
    Ok(())
}

/// Mirrors `build_leaderboard` for every player.
fn write_leaderboard(out: &mut Vec<u8>, players: &[LeaderboardPlayer]) -> io::Result<()> {
    out.push(b'[');
    for (index, p) in players.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        let s = &p.statistics;
        wire_object!(out {
            "isFailed": !p.is_passing,
            "position": p.position,
            "team": p.team,
            "id": p.user_id,
            "name": p.name,
            "score": p.score,
            "accuracy": p.accuracy,
            "hits": {
                "0": s.miss,
                "50": s.meh,
                "100": s.ok,
                "300": s.great,
                "geki": s.perfect,
                "katu": s.good,
            },
            "combo": {
                "current": p.combo,
                "max": p.max_combo,
            },
            "mods": p.mods,
            "rank": "",
        });
    }
    out.push(b']');
    Ok(())
}

/// Mirrors `build_performance` accuracy values.
fn write_accuracy(out: &mut Vec<u8>, state: &RegularState) -> io::Result<()> {
    let value = |n: u8| fix_decimals(state.beatmap.pp_acc.get(&n).copied().unwrap_or(0.0));
    wire_object!(out {
        "90": value(90),
        "91": value(91),
        "92": value(92),
        "93": value(93),
        "94": value(94),
        "95": value(95),
        "96": value(96),
        "97": value(97),
        "98": value(98),
        "99": value(99),
        "100": value(100),
    });
    Ok(())
}

/// Mirrors `build_results`.
fn write_results(out: &mut Vec<u8>, state: &RegularState) -> io::Result<()> {
    let r = &state.result_screen;
    let s = &r.raw.statistics;
    wire_object!(out {
        "scoreId": r.raw.online_id,
        "playerName": r.raw.player_name,
        "mode": NumberNameWire(Some(r.raw.mode), ruleset_name(r.raw.mode)),
        "score": r.raw.score,
        "accuracy": r.raw.accuracy,
        "name": r.raw.player_name,
        "hits": {
            "0": s.miss,
            "50": s.meh,
            "100": s.ok,
            "300": s.great,
            "geki": s.perfect,
            "katu": s.good,
            "sliderEndHits": s.slider_tail_hit,
            "smallTickHits": s.small_tick_hit,
            "largeTickHits": s.large_tick_hit,
        },
        "mods": r.raw.mods,
        "maxCombo": r.raw.max_combo,
        "rank": r.grade,
        "pp": {
            "current": r.pp,
            "fc": r.fc_pp,
        },
        "createdAt": r.raw.date,
    });
    Ok(())
}

/// Writes the tourney object; the rare chat and client lists keep their DTO
/// serialization.
fn write_tourney(out: &mut Vec<u8>, tourney: &Tourney) -> io::Result<()> {
    wire_object!(out {
        "scoreVisible": tourney.score_visible,
        "starsVisible": tourney.stars_visible,
        "ipcState": tourney.ipc_state,
        "bestOF": tourney.best_of,
        "team": {
            "left": tourney.team.left,
            "right": tourney.team.right,
        },
        "points": {
            "left": tourney.points.left,
            "right": tourney.points.right,
        },
        "chat": Serde(&tourney.chat),
        "totalScore": {
            "left": tourney.total_score.left,
            "right": tourney.total_score.right,
        },
        "clients": Serde(&tourney.clients),
    });
    Ok(())
}

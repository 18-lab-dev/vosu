//! Direct streamCompanion `/json/sc` wire writer.
//!
//! The flat document is mostly constants, `toFixed` numbers and strings that
//! [`build`](super::build) assembles with `format!`, so `encode` writes the
//! same bytes straight from the state instead of materializing the DTO and its
//! three nested JSON strings. `build` stays the owned contract for filtered
//! projection; debug builds check every direct encode against it.

use std::io::{self, Write};

use super::*;
use crate::api::json::{Serde, Wire, With, wire_object, write_js_number, write_str_content};
use crate::calculation::beatmap::ParsedBeatmap;

/// Encodes the unfiltered streamCompanion document exactly as
/// `build` serializes through [`JsFormatter`](crate::api::json::JsFormatter).
pub(super) fn encode(snapshot: &CombinedSnapshot) -> Result<Vec<u8>, ApiError> {
    let regular = snapshot.regular().ok_or(ApiError::NotReady)?;
    let precise = snapshot.precise().ok_or(ApiError::NotReady)?;
    let hit_errors = &precise.hit_errors;
    let leaderboard = regular.gameplay.leaderboard_scores.len();
    let mut out = Vec::with_capacity(
        3 * 1024
            + 256 * leaderboard
            + 24 * (hit_errors.all.len() + hit_errors.press.len() + hit_errors.release.len()),
    );
    write_document(&mut out, snapshot, regular, precise)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(out)
}

/// A JSON string assembled from parts; escaping is per character, so escaping
/// each part equals escaping their concatenation.
struct Joined<'a>(&'a [&'a str]);

impl Wire for Joined<'_> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        out.push(b'"');
        for part in self.0 {
            write_str_content(out, part);
        }
        out.push(b'"');
        Ok(())
    }
}

/// A JSON string whose content is written by a closure that never emits
/// characters needing escapes (digits, signs, letters and punctuation).
struct Text<F>(F);

impl<F: Fn(&mut Vec<u8>) -> io::Result<()>> Wire for Text<F> {
    fn write_wire(&self, out: &mut Vec<u8>) -> io::Result<()> {
        out.push(b'"');
        (self.0)(out)?;
        out.push(b'"');
        Ok(())
    }
}

/// JavaScript `String(number)`, as [`js_number_text`] spells it.
fn write_js_text(out: &mut Vec<u8>, value: f64) -> io::Result<()> {
    if value.is_nan() {
        out.extend_from_slice(b"NaN");
    } else if value == f64::INFINITY {
        out.extend_from_slice(b"Infinity");
    } else if value == f64::NEG_INFINITY {
        out.extend_from_slice(b"-Infinity");
    } else {
        write_js_number(out, value)?;
    }
    Ok(())
}

/// `pad_start_2(js_number_text(value))` without allocating.
fn write_js_text_padded(out: &mut Vec<u8>, value: f64) -> io::Result<()> {
    let start = out.len();
    write_js_text(out, value)?;
    if out.len() - start < 2 {
        out.insert(start, b'0');
    }
    Ok(())
}

/// [`format_milliseconds`] without allocating.
fn write_milliseconds(out: &mut Vec<u8>, milliseconds: f64) -> io::Result<()> {
    write_js_text_padded(out, (milliseconds / 3_600_000.0).floor())?;
    out.push(b':');
    write_js_text_padded(out, ((milliseconds % 3_600_000.0) / 60_000.0).floor())?;
    out.push(b':');
    write_js_text_padded(out, ((milliseconds % 60_000.0) / 1_000.0).floor())?;
    out.push(b'.');
    write_js_text(out, milliseconds % 1_000.0)
}

/// [`bpm_text`] without allocating.
fn write_bpm_text(out: &mut Vec<u8>, minimum: f64, maximum: f64, common: f64) -> io::Result<()> {
    if minimum == maximum {
        return write_js_text(out, maximum);
    }
    write_js_text(out, js_round(minimum))?;
    out.push(b'-');
    write_js_text(out, js_round(maximum))?;
    out.extend_from_slice(b" (");
    write_js_text(out, js_round(common))?;
    out.push(b')');
    Ok(())
}

/// Writes `format!("{value:?}")` re-cased, as [`country_name`] and
/// [`ruleset_name`] produce enum names, falling back to the Unicode-aware
/// conversion for non-ASCII spellings.
fn write_debug_name(out: &mut Vec<u8>, value: &dyn std::fmt::Debug, upper: bool) -> io::Result<()> {
    out.push(b'"');
    let start = out.len();
    write!(out, "{value:?}")?;
    if out[start..].is_ascii() {
        if upper {
            out[start..].make_ascii_uppercase();
        } else {
            out[start..].make_ascii_lowercase();
        }
    } else {
        let name = String::from_utf8_lossy(&out[start..]).into_owned();
        out.truncate(start);
        let cased = if upper {
            name.to_uppercase()
        } else {
            name.to_lowercase()
        };
        write_str_content(out, &cased);
    }
    out.push(b'"');
    Ok(())
}

/// Writes `value` JSON-encoded twice: the content of a JSON string that holds
/// the JSON string `value`, as the nested-document fields carry it.
fn write_double_escaped(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(b"\\\"");
    if value
        .bytes()
        .all(|byte| byte >= 0x20 && byte != b'"' && byte != b'\\')
    {
        out.extend_from_slice(value.as_bytes());
    } else {
        let mut once = Vec::with_capacity(value.len() + 8);
        write_str_content(&mut once, value);
        // Escaping preserves UTF-8 validity.
        write_str_content(out, std::str::from_utf8(&once).expect("escaped UTF-8"));
    }
    out.extend_from_slice(b"\\\"");
}

/// One leaderboard entry document, written already escaped as the content
/// of a JSON string (see the nested-document fields of [`Response`]).
///
/// Keys, numbers and booleans never contain characters that need escaping,
/// so only the username is escaped twice; the bytes equal serializing the
/// entry and then escaping that text. Shared by the main-player state and
/// the score list, whose element types differ but share these fields.
macro_rules! escaped_leaderboard_entry {
    ($out:ident, $visible:expr, $player:expr) => {{
        let player = $player;
        let bool_text = |value: bool| if value { &b"true"[..] } else { &b"false"[..] };
        $out.push(b'{');
        if let Some(visible) = $visible {
            $out.extend_from_slice(b"\\\"IsLeaderboardVisible\\\":");
            $out.extend_from_slice(bool_text(visible));
            $out.push(b',');
        }
        $out.extend_from_slice(b"\\\"Username\\\":");
        write_double_escaped($out, &player.name);
        $out.extend_from_slice(b",\\\"Score\\\":");
        write_js_number($out, player.score)?;
        $out.extend_from_slice(b",\\\"Combo\\\":");
        player.combo.write_wire($out)?;
        $out.extend_from_slice(b",\\\"MaxCombo\\\":");
        player.max_combo.write_wire($out)?;
        $out.extend_from_slice(
            b",\\\"Mods\\\":{\\\"ModsXor1\\\":-1,\\\"ModsXor2\\\":-1,\\\"Value\\\":",
        );
        player.mods.number.write_wire($out)?;
        $out.extend_from_slice(b"},\\\"Hit300\\\":");
        player.statistics.great.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Hit100\\\":");
        player.statistics.ok.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Hit50\\\":");
        player.statistics.meh.write_wire($out)?;
        $out.extend_from_slice(b",\\\"HitMiss\\\":");
        player.statistics.miss.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Team\\\":");
        player.team.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Position\\\":");
        player.position.write_wire($out)?;
        $out.extend_from_slice(b",\\\"IsPassing\\\":");
        $out.extend_from_slice(bool_text(player.is_passing));
        $out.push(b'}');
    }};
}

fn write_document(
    out: &mut Vec<u8>,
    snapshot: &CombinedSnapshot,
    regular: &RegularState,
    precise: &crate::state::PreciseState,
) -> io::Result<()> {
    let global = &regular.global;
    let menu = &regular.menu;
    let gameplay = &regular.gameplay;
    let beatmap = &regular.beatmap;
    let parsed: Option<&ParsedBeatmap> = beatmap.parsed.as_deref();
    let attributes = beatmap.attributes.unwrap_or_default();

    let raw_status = precise.status;
    let status = stream_companion_status(raw_status, global.is_watching_replay);
    let current_mods = current_mods(regular, raw_status);
    let current_mode = current_mode(regular, raw_status);
    let hits = to_legacy_hits(gameplay.mode as i32, &gameplay.statistics);

    let user = regular.user.profile.as_ref();
    let raw_login_status = user.map(|user| user.raw_login_status);
    let artist = js_undefined(menu.artist.as_deref());
    let artist_original = js_undefined(menu.artist_original.as_deref());
    let title = js_undefined(menu.title.as_deref());
    let title_original = js_undefined(menu.title_original.as_deref());
    let difficulty = js_undefined(menu.difficulty.as_deref());

    let original = parsed.map(|map| &map.engine.original_difficulty);
    let converted = parsed.map(|map| &map.engine.converted_difficulty);
    let map_difficulty = parsed.map(|map| &map.engine.difficulty);
    let bpm = parsed.map(|map| map.bpm).unwrap_or_default();
    let times = parsed.map(|map| map.times).unwrap_or_default();
    let objects = parsed.map(|map| map.object_counts).unwrap_or_default();
    let pp =
        |accuracy: u8| fix_decimals(beatmap.pp_acc.get(&accuracy).copied().unwrap_or_default());

    let common_bpm = round4(bpm.common);
    let max_bpm = round4(bpm.maximum);
    let min_bpm = round4(bpm.minimum);
    let first_object = i64::from(times.first_object);
    let play_time = precise.play_time;
    let full = times.full;
    let keys = &precise.key_overlay;
    let key = |index: usize| keys.get(index);
    let key_overlay_enabled = snapshot.runtime_settings().enable_key_overlay;
    // Too many members for one macro expansion; written in three groups.
    wire_object!(@open first);
    wire_object!(@members out first {
        "osuIsRunning": 1_i64,
        "chatIsEnabled": i64::from(global.chat_status > 0),
        "ingameInterfaceIsEnabled": i64::from(global.show_interface),
        "isBreakTime": 0_i64,
        "banchoIsConnected": i64::from(raw_login_status == Some(65_793)),
        "banchoId"?: user.map(|user| user.id),
        "banchoUsername"?: user.map(|user| &user.name),
        "banchoStatus"?: raw_login_status,
        "banchoCountry": With(|out: &mut Vec<u8>| {
            match user
                .and_then(|user| i32::try_from(user.country_code).ok())
                .and_then(CountryCodes::from_value)
            {
                Some(country) => write_debug_name(out, &country, true),
                None => {
                    out.extend_from_slice(b"\"\"");
                    Ok(())
                }
            }
        }),
        "artistRoman"?: menu.artist.as_ref(),
        "artistUnicode"?: menu.artist_original.as_ref(),
        "titleRoman"?: menu.title.as_ref(),
        "titleUnicode"?: menu.title_original.as_ref(),
        "mapArtistTitle": Joined(&[artist, " - ", title]),
        "mapArtistTitleUnicode": Joined(&[artist_original, " - ", title_original]),
        "diffName"?: menu.difficulty.as_ref(),
        "mapDiff": Joined(&["[", difficulty, "]"]),
        "creator"?: menu.creator.as_ref(),
        "liveStarRating": fix_decimals(attributes.stars),
        "mStars": fix_decimals(map_difficulty.map_or(0.0, |value| value.stars)),
        "ar": fix_decimals(original.map_or(0.0, |value| value.approach_rate)),
        "mAR": fix_decimals(converted.map_or(0.0, |value| value.approach_rate)),
        "od": fix_decimals(original.map_or(0.0, |value| value.overall_difficulty)),
        "mOD": fix_decimals(converted.map_or(0.0, |value| value.overall_difficulty)),
        "cs": fix_decimals(original.map_or(0.0, |value| value.circle_size)),
        "mCS": fix_decimals(converted.map_or(0.0, |value| value.circle_size)),
        "hp": fix_decimals(original.map_or(0.0, |value| value.drain_rate)),
        "mHP": fix_decimals(converted.map_or(0.0, |value| value.drain_rate)),
        "currentBpm": round4(beatmap.event_status.bpm),
        "bpm": common_bpm,
        "mainBpm": common_bpm,
        "maxBpm": max_bpm,
        "minBpm": min_bpm,
        "mMainBpm": common_bpm,
        "mMaxBpm": max_bpm,
        "mMinBpm": min_bpm,
        "mBpm": Text(|out: &mut Vec<u8>| write_bpm_text(out, bpm.minimum, bpm.maximum, bpm.common)),
        "md5"?: menu.checksum.as_ref(),
        "gameMode"?: current_mode
            .and_then(|value| i32::try_from(value).ok())
            .and_then(Rulesets::from_value)
            .map(|mode| With(move |out: &mut Vec<u8>| write_debug_name(out, &mode, false))),
        "mode"?: current_mode,
        "time": play_time / 1_000.0,
        "previewtime": i64::from(times.preview),
        "totaltime": full,
        "timeLeft": Text(|out: &mut Vec<u8>| write_milliseconds(out, full - play_time)),
        "drainingtime": full - first_object as f64,
        "totalAudioTime"?: menu.mp3_length,
        "firstHitObjectTime": first_object,
        "mapid"?: menu.map_id,
        "mapsetid"?: menu.set_id,
    });
    wire_object!(@members out first {
        "mapStrains": With(|out: &mut Vec<u8>| {
            if beatmap.graph.x_axis.is_empty() {
                out.extend_from_slice(b"{}");
                Ok(())
            } else {
                Serde(&map_strains(
                    &beatmap.graph.x_axis,
                    beatmap.graph.series.first().map(|s| s.data.as_slice()),
                ))
                .write_wire(out)
            }
        }),
        "mapBreaks": With(|out: &mut Vec<u8>| {
            out.push(b'[');
            for (index, range) in parsed.map_or(&[][..], |map| &map.breaks[..]).iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                wire_object!(out {
                    "startTime": range.start,
                    "endTime": range.end,
                    "hasEffect": range.has_effect,
                });
            }
            out.push(b']');
            Ok(())
        }),
        "mapKiaiPoints": With(|out: &mut Vec<u8>| {
            out.extend_from_slice(b"[]");
            Ok(())
        }),
        "mapPosition": Text(|out: &mut Vec<u8>| write_milliseconds(out, play_time)),
        "mapTimingPoints": With(|out: &mut Vec<u8>| {
            out.push(b'[');
            for (index, point) in parsed
                .map_or(&[][..], |map| &map.timing_points[..])
                .iter()
                .enumerate()
            {
                if index > 0 {
                    out.push(b',');
                }
                wire_object!(out {
                    "startTime": if point.start == 0.0 || point.start.is_nan() {
                        0.0
                    } else {
                        point.start
                    },
                    "beatLength": point.beat_length,
                });
            }
            out.push(b']');
            Ok(())
        }),
        "sliders": i64::from(objects.sliders),
        "circles": i64::from(objects.circles),
        "spinners": i64::from(objects.spinners),
        "maxCombo": parsed.map_or(0, |map| i64::from(map.engine.difficulty.max_combo)),
        "mp3Name"?: menu.audio_filename.as_ref(),
        "osuFileName"?: menu.filename.as_ref(),
        "backgroundImageFileName"?: menu.background_filename.as_ref(),
        "osuFileLocation": path_join(menu.folder.as_deref(), menu.filename.as_deref()),
        "backgroundImageLocation": path_join(
            menu.folder.as_deref(),
            menu.background_filename.as_deref(),
        ),
        "retries": gameplay.retries,
        "username": gameplay.player_name,
        "score": gameplay.score,
        "playerHp": gameplay.player_hp,
        "playerHpSmooth": gameplay.player_hp_smooth,
        "combo": gameplay.combo,
        "currentMaxCombo": gameplay.max_combo,
        // Key names and values never need escaping, so the nested document
        // is written in its escaped form directly.
        "keyOverlay": With(|out: &mut Vec<u8>| {
            let bool_text = |value: bool| if value { &b"true"[..] } else { &b"false"[..] };
            out.extend_from_slice(b"\"{\\\"Enabled\\\":");
            out.extend_from_slice(bool_text(key_overlay_enabled));
            for (index, name) in ["K1", "K2", "M1", "M2"].into_iter().enumerate() {
                out.extend_from_slice(b",\\\"");
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(b"Pressed\\\":");
                out.extend_from_slice(bool_text(key(index).is_some_and(|key| key.is_pressed)));
                out.extend_from_slice(b",\\\"");
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(b"Count\\\":");
                key(index).map_or(0, |key| key.count).write_wire(out)?;
            }
            out.extend_from_slice(b"}\"");
            Ok(())
        }),
        "geki": hits.geki,
        "c300": hits.n300,
        "katsu": hits.katu,
        "c100": hits.n100,
        "c50": hits.n50,
        "miss": hits.n0,
        "sliderBreaks": gameplay.slider_breaks,
        "acc": fix_decimals(gameplay.accuracy),
        "unstableRate": fix_decimals(precise.unstable_rate * current_mods.rate),
        "convertedUnstableRate": fix_decimals(precise.unstable_rate),
        "grade"?: grade_number(&gameplay.grade_current),
        "maxGrade"?: grade_number(&gameplay.grade_expected),
        "hitErrors": precise.hit_errors.all.as_slice(),
        "pressHitErrors": precise.hit_errors.press.as_slice(),
        "releaseHitErrors": precise.hit_errors.release.as_slice(),
        "mods": current_mods.name,
        "modsEnum": current_mods.number,
        "ppIfMapEndsNow": fix_decimals(attributes.pp),
        "ppIfRestFced": fix_decimals(attributes.fc_pp),
        "leaderBoardMainPlayer": With(|out: &mut Vec<u8>| {
            out.push(b'"');
            escaped_leaderboard_entry!(
                out,
                Some(gameplay.leaderboard_visible),
                &gameplay.leaderboard_player
            );
            out.push(b'"');
            Ok(())
        }),
        "leaderBoardPlayers": With(|out: &mut Vec<u8>| {
            out.extend_from_slice(b"\"[");
            for (index, player) in gameplay.leaderboard_scores.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                escaped_leaderboard_entry!(out, None::<bool>, player);
            }
            out.extend_from_slice(b"]\"");
            Ok(())
        }),
    });
    wire_object!(@members out first {
        "rankedStatus"?: menu.ranked_status,
        "songSelectionRankingType": regular
            .settings
            .values
            .get("leaderboardType")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
        "rawStatus": raw_status,
        "dir"?: menu.folder.as_ref(),
        "dl": With(|out: &mut Vec<u8>| {
            out.extend_from_slice(b"\"https://osu.ppy.sh/b/");
            match menu.map_id {
                Some(id) => id.write_wire(out)?,
                None => out.extend_from_slice(b"undefined"),
            }
            out.push(b'"');
            Ok(())
        }),
        "osu_90PP": pp(90),
        "osu_95PP": pp(95),
        "osu_96PP": pp(96),
        "osu_97PP": pp(97),
        "osu_98PP": pp(98),
        "osu_99PP": pp(99),
        "osu_SSPP": pp(100),
        "osu_m90PP": pp(90),
        "osu_m95PP": pp(95),
        "osu_m96PP": pp(96),
        "osu_m97PP": pp(97),
        "osu_m98PP": pp(98),
        "osu_m99PP": pp(99),
        "osu_mSSPP": pp(100),
        "accPpIfMapEndsNow": fix_decimals(attributes.current.accuracy),
        "aimPpIfMapEndsNow": fix_decimals(attributes.current.aim),
        "speedPpIfMapEndsNow": fix_decimals(attributes.current.speed),
        "strainPpIfMapEndsNow": fix_decimals(attributes.current.difficulty),
        "noChokePp": fix_decimals(attributes.fc_pp),
        "skin": global.skin_folder,
        "skinPath": global.skin_folder,
        "songSelectionScores": "[]",
        "songSelectionMainPlayerScore": "{}",
        "songSelectionTotalScores": -1_i64,
        "status": status,
        "osu_99_9PP": -1_i64,
        "osu_m99_9PP": -1_i64,
        "mania_1_000_000PP": -1_i64,
        "mania_m1_000_000PP": -1_i64,
        "simulatedPp": pp(100),
        "plays": 0_i64,
        "tags": "",
        "source": "",
        "starsNomod": -1_i64,
        "comboLeft": -1_i64,
        "localTime": 0_i64,
        "localTimeISO": "0",
        "sl": -1_i64,
        "sv": -1_i64,
        "threadid": 0_i64,
        "test": "",
    });
    wire_object!(@close out first);
    Ok(())
}

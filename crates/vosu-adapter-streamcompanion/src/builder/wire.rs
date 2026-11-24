//! Direct streamCompanion `/json/sc` wire writer.
//!
//! The flat document is mostly constants, `toFixed` numbers and strings that
//! [`build`](super::build) assembles with `format!`, so `encode` writes the
//! same bytes straight from the state instead of materializing the DTO and its
//! three nested JSON strings. `build` stays the owned contract for filtered
//! projection; unit tests check direct encoding against that contract.

use std::io::{self, Write};

use super::*;
use vosu_adapter_support::json::{
    Serde, Wire, With, wire_object, write_js_number, write_str_content,
};

/// Encodes the unfiltered streamCompanion document exactly as
/// `build` serializes through [`JsFormatter`](vosu_adapter_support::json::JsFormatter).
pub(super) fn encode(view: &View, chart: Option<&ChartFacts>) -> Result<Vec<u8>, ApiError> {
    let mut out =
        Vec::with_capacity(3 * 1024 + 256 * view.leaderboard.len() + 24 * view.hit_errors.len());
    write_document(&mut out, view, chart).map_err(|e| ApiError::serialization(e.to_string()))?;
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
    ($out:ident, $visible:expr, $player:expr, $combo:expr, $max_combo:expr, $statistics:expr, $passing:expr) => {{
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
        ($combo).write_wire($out)?;
        $out.extend_from_slice(b",\\\"MaxCombo\\\":");
        ($max_combo).write_wire($out)?;
        $out.extend_from_slice(
            b",\\\"Mods\\\":{\\\"ModsXor1\\\":-1,\\\"ModsXor2\\\":-1,\\\"Value\\\":",
        );
        player.mods.number.write_wire($out)?;
        $out.extend_from_slice(b"},\\\"Hit300\\\":");
        ($statistics).great.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Hit100\\\":");
        ($statistics).ok.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Hit50\\\":");
        ($statistics).meh.write_wire($out)?;
        $out.extend_from_slice(b",\\\"HitMiss\\\":");
        ($statistics).miss.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Team\\\":");
        player.team.write_wire($out)?;
        $out.extend_from_slice(b",\\\"Position\\\":");
        player.position.write_wire($out)?;
        $out.extend_from_slice(b",\\\"IsPassing\\\":");
        $out.extend_from_slice(bool_text($passing));
        $out.push(b'}');
    }};
}

fn write_document(out: &mut Vec<u8>, view: &View, parsed: Option<&ChartFacts>) -> io::Result<()> {
    let raw_status = view.raw_status;
    let status = stream_companion_status(raw_status, view.watching_replay);
    let current_mods = &view.play.mods;
    let current_mode = view.mode();
    let hits = to_legacy_hits(
        view.play.mode.number.unwrap_or_default() as i32,
        &view.statistics(),
    );

    let user = view.profile.id.map(|_| &view.profile);
    let raw_login_status = view.profile.user_status.number;
    let artist = js_undefined(view.metadata.artist.as_deref());
    let artist_original = js_undefined(view.metadata.artist_unicode.as_deref());
    let title = js_undefined(view.metadata.title.as_deref());
    let title_original = js_undefined(view.metadata.title_unicode.as_deref());
    let difficulty = js_undefined(view.metadata.version.as_deref());

    let original = parsed.map(|map| &map.original);
    let converted = parsed.map(|map| &map.converted);
    let map_difficulty = parsed.map(|map| &map.difficulty);
    let bpm = parsed.map(|map| map.bpm).unwrap_or_default();
    let times = parsed.map(|map| map.times).unwrap_or_default();
    let objects = parsed.map(|map| map.objects).unwrap_or_default();
    let pp =
        |accuracy: u8| fix_decimals(view.pp_accuracy.get(&accuracy).copied().unwrap_or_default());

    let common_bpm = round4(bpm.common);
    let max_bpm = round4(bpm.maximum);
    let min_bpm = round4(bpm.minimum);
    let first_object = i64::from(times.first_object);
    let play_time = view.play_time;
    let full = times.full;
    let keys = [
        view.overlay.k1,
        view.overlay.k2,
        view.overlay.m1,
        view.overlay.m2,
    ];
    let key = |index: usize| keys[index].as_ref();
    let key_overlay_enabled = true;
    // Too many members for one macro expansion; written in three groups.
    wire_object!(@open out first);
    wire_object!(@members out first {
        "osuIsRunning": 1_i64,
        "chatIsEnabled": i64::from(view.chat_status > 0),
        "ingameInterfaceIsEnabled": i64::from(view.interface_visible),
        "isBreakTime": 0_i64,
        "banchoIsConnected": i64::from(raw_login_status == Some(65_793)),
        "banchoId"?: user.and_then(|user| user.id),
        "banchoUsername"?: user.and_then(|user| user.name.as_ref()),
        "banchoStatus"?: raw_login_status,
        "banchoCountry": With(|out: &mut Vec<u8>| {
            match user
                .and_then(|user| user.country_code.number.and_then(|n| i32::try_from(n).ok()))
                .and_then(CountryCodes::from_value)
            {
                Some(country) => write_debug_name(out, &country, true),
                None => {
                    out.extend_from_slice(b"\"\"");
                    Ok(())
                }
            }
        }),
        "artistRoman"?: view.metadata.artist.as_ref(),
        "artistUnicode"?: view.metadata.artist_unicode.as_ref(),
        "titleRoman"?: view.metadata.title.as_ref(),
        "titleUnicode"?: view.metadata.title_unicode.as_ref(),
        "mapArtistTitle": Joined(&[artist, " - ", title]),
        "mapArtistTitleUnicode": Joined(&[artist_original, " - ", title_original]),
        "diffName"?: view.metadata.version.as_ref(),
        "mapDiff": Joined(&["[", difficulty, "]"]),
        "creator"?: view.metadata.mapper.as_ref(),
        "liveStarRating": fix_decimals(view.live_stars),
        "mStars": fix_decimals(map_difficulty.map_or(0.0, |value| value.stars)),
        "ar": fix_decimals(original.map_or(0.0, |value| value.approach_rate)),
        "mAR": fix_decimals(converted.map_or(0.0, |value| value.approach_rate)),
        "od": fix_decimals(original.map_or(0.0, |value| value.overall_difficulty)),
        "mOD": fix_decimals(converted.map_or(0.0, |value| value.overall_difficulty)),
        "cs": fix_decimals(original.map_or(0.0, |value| value.circle_size)),
        "mCS": fix_decimals(converted.map_or(0.0, |value| value.circle_size)),
        "hp": fix_decimals(original.map_or(0.0, |value| value.drain_rate)),
        "mHP": fix_decimals(converted.map_or(0.0, |value| value.drain_rate)),
        "currentBpm": round4(view.current_bpm),
        "bpm": common_bpm,
        "mainBpm": common_bpm,
        "maxBpm": max_bpm,
        "minBpm": min_bpm,
        "mMainBpm": common_bpm,
        "mMaxBpm": max_bpm,
        "mMinBpm": min_bpm,
        "mBpm": Text(|out: &mut Vec<u8>| write_bpm_text(out, bpm.minimum, bpm.maximum, bpm.common)),
        "md5"?: view.metadata.checksum.as_ref(),
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
        "totalAudioTime"?: view.metadata.time.mp3_length,
        "firstHitObjectTime": first_object,
        "mapid"?: view.metadata.id,
        "mapsetid"?: view.metadata.set,
    });
    wire_object!(@members out first {
        "mapStrains": With(|out: &mut Vec<u8>| { if let Some(map) = parsed { Serde(&map.strains).write_wire(out) } else { out.extend_from_slice(b"{}"); Ok(()) } }),
        "mapBreaks": Serde(&parsed.map_or(&[][..], |map| map.breaks.as_slice())),
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
                    "startTime": if point.start_time == 0.0 || point.start_time.is_nan() {
                        0.0
                    } else {
                        point.start_time
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
        "maxCombo": parsed.map_or(0, |map| i64::from(map.difficulty.max_combo)),
        "mp3Name"?: view.paths.current.as_ref().and_then(|p| p.audio.as_ref()),
        "osuFileName"?: view.paths.current.as_ref().and_then(|p| p.beatmap_file.as_ref()),
        "backgroundImageFileName"?: view.paths.current.as_ref().and_then(|p| p.background.as_ref()),
        "osuFileLocation": path_join(view.paths.current.as_ref().and_then(|p| p.beatmap_folder.as_deref()), view.paths.current.as_ref().and_then(|p| p.beatmap_file.as_deref())),
        "backgroundImageLocation": path_join(
            view.paths.current.as_ref().and_then(|p| p.beatmap_folder.as_deref()),
            view.paths.current.as_ref().and_then(|p| p.background.as_deref()),
        ),
        "retries": view.play.retries,
        "username": view.play.player_name,
        "score": view.play.score,
        "playerHp": view.play.health_raw.normal,
        "playerHpSmooth": view.play.health_raw.smooth,
        "combo": view.play.combo.current,
        "currentMaxCombo": view.play.combo.max,
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
        "sliderBreaks": view.play.statistics.slider_breaks,
        "acc": fix_decimals(view.play.accuracy_percent),
        "unstableRate": fix_decimals(view.play.precise_unstable_rate * current_mods.rate),
        "convertedUnstableRate": fix_decimals(view.play.precise_unstable_rate),
        "grade"?: grade_number(&view.play.rank.current),
        "maxGrade"?: grade_number(&view.play.rank.max_this_play),
        "hitErrors": view.hit_errors.as_ref(),
        "mods": current_mods.name,
        "modsEnum": current_mods.number,
        "ppIfMapEndsNow": fix_decimals(view.pp.current),
        "ppIfRestFced": fix_decimals(view.pp.fc),
        "leaderBoardMainPlayer": With(|out: &mut Vec<u8>| {
            out.push(b'"');
            escaped_leaderboard_entry!(
                out,
                Some(view.leaderboard_state.visible),
                &view.leaderboard_state.player, view.leaderboard_state.player.combo, view.leaderboard_state.player.max_combo, view.leaderboard_state.player.judgements, view.leaderboard_state.player.is_passing
            );
            out.push(b'"');
            Ok(())
        }),
        "leaderBoardPlayers": With(|out: &mut Vec<u8>| {
            out.extend_from_slice(b"\"[");
            for (index, player) in view.leaderboard.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                escaped_leaderboard_entry!(out, None::<bool>, player, player.combo.current, player.combo.max, player.statistics, !player.is_failed);
            }
            out.extend_from_slice(b"]\"");
            Ok(())
        }),
    });
    wire_object!(@members out first {
        "rankedStatus"?: view.metadata.status.number,
        "songSelectionRankingType": view.ranking_type,
        "rawStatus": raw_status,
        "dir"?: view.paths.current.as_ref().and_then(|p| p.beatmap_folder.as_ref()),
        "dl": With(|out: &mut Vec<u8>| {
            out.extend_from_slice(b"\"https://osu.ppy.sh/b/");
            match view.metadata.id {
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
        "accPpIfMapEndsNow": fix_decimals(view.pp.detailed.current.accuracy),
        "aimPpIfMapEndsNow": fix_decimals(view.pp.detailed.current.aim),
        "speedPpIfMapEndsNow": fix_decimals(view.pp.detailed.current.speed),
        "strainPpIfMapEndsNow": fix_decimals(view.pp.detailed.current.difficulty),
        "noChokePp": fix_decimals(view.pp.fc),
        "skin": view.paths.skin_folder.as_deref().unwrap_or_default(),
        "skinPath": view.paths.skin_folder.as_deref().unwrap_or_default(),
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

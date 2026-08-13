use crate::{Error, HitResult, ParsedBeatmap, Ruleset};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HitWindows {
    pub miss: Option<f64>,
    pub meh: Option<f64>,
    pub ok: Option<f64>,
    pub good: Option<f64>,
    pub great: Option<f64>,
    pub perfect: Option<f64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatchSpatialWindow {
    pub half_width: f64,
}
fn range(od: f64, min: f64, mid: f64, max: f64) -> f64 {
    if od > 5.0 {
        mid + (max - mid) * (od - 5.0) / 5.0
    } else if od < 5.0 {
        mid + (mid - min) * (od - 5.0) / 5.0
    } else {
        mid
    }
}

fn floor_minus_half(value: f64) -> f64 {
    value.floor() - 0.5
}

pub fn hit_window(mode: Ruleset, od: f64, result: HitResult) -> Result<Option<f64>, Error> {
    let value = match mode {
        Ruleset::Catch => return Ok(None),
        Ruleset::Osu => match result {
            HitResult::Miss => 400.0,
            HitResult::Meh => floor_minus_half(range(od, 200.0, 150.0, 100.0)),
            HitResult::Ok => floor_minus_half(range(od, 140.0, 100.0, 60.0)),
            HitResult::Great => floor_minus_half(range(od, 80.0, 50.0, 20.0)),
            _ => return Err(Error::HitWindow { mode, result }),
        },
        Ruleset::Taiko => match result {
            HitResult::Miss => floor_minus_half(range(od, 135.0, 95.0, 70.0)),
            HitResult::Ok => floor_minus_half(range(od, 120.0, 80.0, 50.0)),
            HitResult::Great => floor_minus_half(range(od, 50.0, 35.0, 20.0)),
            _ => return Err(Error::HitWindow { mode, result }),
        },
        Ruleset::Mania => {
            let (a, b, c) = match result {
                HitResult::Perfect => (22.4, 19.4, 13.9),
                HitResult::Great => (64.0, 49.0, 34.0),
                HitResult::Good => (97.0, 82.0, 67.0),
                HitResult::Ok => (127.0, 112.0, 97.0),
                HitResult::Meh => (151.0, 136.0, 121.0),
                HitResult::Miss => (188.0, 173.0, 158.0),
                _ => return Err(Error::HitWindow { mode, result }),
            };
            range(od, a, b, c).floor() + 0.5
        }
    };
    Ok(Some(value))
}
pub fn hit_window_numeric(mode: u8, od: f64, result: i32) -> Result<Option<f64>, Error> {
    hit_window(
        Ruleset::from_mode_id(mode).ok_or_else(|| Error::Calculation("invalid mode".into()))?,
        od,
        HitResult::from_i32(result)
            .ok_or_else(|| Error::Calculation("invalid hit result".into()))?,
    )
}
pub fn all_hit_windows(mode: Ruleset, od: f64) -> Vec<(HitResult, f64)> {
    [
        HitResult::Miss,
        HitResult::Meh,
        HitResult::Ok,
        HitResult::Good,
        HitResult::Great,
        HitResult::Perfect,
    ]
    .into_iter()
    .filter_map(|r| hit_window(mode, od, r).ok().flatten().map(|v| (r, v)))
    .collect()
}
pub fn great_hit_window(mode: Ruleset, od: f64) -> f64 {
    hit_window(mode, od, HitResult::Great)
        .ok()
        .flatten()
        .unwrap_or(0.0)
}
pub fn hit_windows_for_map(map: &ParsedBeatmap) -> Result<HitWindows, Error> {
    map_windows(map, true)
}
pub fn hit_windows_for_map_without_speed_adjustment(
    map: &ParsedBeatmap,
) -> Result<HitWindows, Error> {
    map_windows(map, false)
}
pub fn hit_window_for_map(
    map: &ParsedBeatmap,
    od: f64,
    result: HitResult,
) -> Result<Option<f64>, Error> {
    if map.state.mode != Ruleset::Mania {
        return hit_window(map.state.mode, od, result);
    }
    if map.score_v2() {
        return hit_window(Ruleset::Mania, od, result);
    }

    let w = map_windows(map, true)?;
    Ok(match result {
        HitResult::Miss => w.miss,
        HitResult::Meh => w.meh,
        HitResult::Ok => w.ok,
        HitResult::Good => w.good,
        HitResult::Great => w.great,
        HitResult::Perfect => w.perfect,
        _ => {
            return Err(Error::HitWindow {
                mode: Ruleset::Mania,
                result,
            });
        }
    })
}
fn map_windows(map: &ParsedBeatmap, speed: bool) -> Result<HitWindows, Error> {
    let mode = map.state.mode;
    if mode == Ruleset::Catch {
        return Ok(HitWindows::default());
    }

    let mut builder = map.state.playable.attributes();
    builder.od(map.state.playable.od, true);
    let od = f64::from(builder.build().od());
    let rate = if mode == Ruleset::Mania && speed {
        clock_rate(map)
    } else {
        1.0
    };

    let classic = mode == Ruleset::Mania && map.classic_scoring() && !map.score_v2();
    let difficulty_multiplier = if mode == Ruleset::Mania {
        if has_mod(&map.state.mods, "EZ") {
            1.4
        } else if has_mod(&map.state.mods, "HR") {
            1.0 / 1.4
        } else {
            1.0
        }
    } else {
        1.0
    };

    let windows = if classic {
        classic_mania_windows(od, map.state.playable.is_convert)
    } else if mode == Ruleset::Mania {
        raw_mania_windows(od)
    } else {
        all_hit_windows(mode, od)
    };
    let mut out = HitWindows::default();
    for (result, window) in windows {
        let value = if classic {
            ((window - 0.5) * rate * difficulty_multiplier).floor() + 0.5
        } else if mode == Ruleset::Mania {
            (window * rate * difficulty_multiplier).floor() + 0.5
        } else {
            window
        };

        match result {
            HitResult::Miss => out.miss = Some(value),
            HitResult::Meh => out.meh = Some(value),
            HitResult::Ok => out.ok = Some(value),
            HitResult::Good => out.good = Some(value),
            HitResult::Great => out.great = Some(value),
            HitResult::Perfect => out.perfect = Some(value),
            _ => {}
        }
    }
    Ok(out)
}

fn classic_mania_windows(od: f64, is_convert: bool) -> Vec<(HitResult, f64)> {
    let values = if is_convert {
        let great = if od.round() > 4.0 { 34.0 } else { 47.0 };
        [
            (HitResult::Miss, 158.5),
            (HitResult::Meh, 121.5),
            (HitResult::Ok, 97.5),
            (HitResult::Good, if od.round() > 4.0 { 67.5 } else { 77.5 }),
            (HitResult::Great, great + 0.5),
            (HitResult::Perfect, 16.5),
        ]
    } else {
        let inverted_od = (10.0 - od).clamp(0.0, 10.0);
        [
            (HitResult::Miss, 158.0 + 3.0 * inverted_od + 0.5),
            (HitResult::Meh, 121.0 + 3.0 * inverted_od + 0.5),
            (HitResult::Ok, 97.0 + 3.0 * inverted_od + 0.5),
            (HitResult::Good, 67.0 + 3.0 * inverted_od + 0.5),
            (HitResult::Great, 34.0 + 3.0 * inverted_od + 0.5),
            (HitResult::Perfect, 16.5),
        ]
    };
    values.into_iter().collect()
}

fn raw_mania_windows(od: f64) -> Vec<(HitResult, f64)> {
    [
        (HitResult::Miss, range(od, 188.0, 173.0, 158.0)),
        (HitResult::Meh, range(od, 151.0, 136.0, 121.0)),
        (HitResult::Ok, range(od, 127.0, 112.0, 97.0)),
        (HitResult::Good, range(od, 97.0, 82.0, 67.0)),
        (HitResult::Great, range(od, 64.0, 49.0, 34.0)),
        (HitResult::Perfect, range(od, 22.4, 19.4, 13.9)),
    ]
    .into_iter()
    .collect()
}

fn has_mod(mods: &[crate::StructuredMod], acronym: &str) -> bool {
    mods.iter().any(|m| m.acronym.eq_ignore_ascii_case(acronym))
}

fn clock_rate(map: &ParsedBeatmap) -> f64 {
    let rate = map.state.game_mods.clone().into_core().clock_rate();
    // osu! rounds fixed rates to two decimal places before Mania's source
    // floor-plus-half operation.
    (rate * 100.0).round() / 100.0
}
pub fn catch_spatial_window_for_map(map: &ParsedBeatmap) -> Option<CatchSpatialWindow> {
    if map.state.mode != Ruleset::Catch {
        return None;
    }
    let mut builder = map.state.playable.attributes();
    builder.cs(map.state.playable.cs, true);
    let cs = builder.build().cs();
    let scale = ((1.0 - 0.7 * (f64::from(cs) - 5.0) / 5.0) as f32) / 2.0_f32;
    let half_width = 106.75_f32 * (scale * 2.0_f32).abs() * 0.8_f32 / 2.0_f32;
    Some(CatchSpatialWindow {
        half_width: f64::from(half_width),
    })
}

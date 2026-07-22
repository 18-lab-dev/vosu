#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScoreTotals {
    pub objects: u32,
    pub max_combo: u32,
    pub sliders: u32,
    pub slider_ends: u32,
    pub large_ticks: u32,
    pub tiny_droplets: u32,
    pub droplets: u32,
    pub fruits: u32,
    pub hold_notes: u32,
}

impl ScoreTotals {
    pub fn from_difficulty(attrs: &DifficultyAttributes) -> Self {
        match attrs {
            DifficultyAttributes::Osu(attrs) => Self {
                objects: attrs.n_objects(),
                max_combo: attrs.max_combo,
                sliders: attrs.n_sliders,
                slider_ends: attrs.n_sliders,
                large_ticks: attrs.n_large_ticks,
                ..Self::default()
            },
            DifficultyAttributes::Taiko(attrs) => Self {
                objects: attrs.max_combo,
                max_combo: attrs.max_combo,
                ..Self::default()
            },
            DifficultyAttributes::Catch(attrs) => Self {
                objects: attrs.max_combo(),
                max_combo: attrs.max_combo(),
                tiny_droplets: attrs.n_tiny_droplets,
                droplets: attrs.n_droplets,
                fruits: attrs.n_fruits,
                ..Self::default()
            },
            DifficultyAttributes::Mania(attrs) => Self {
                objects: attrs.n_objects,
                max_combo: attrs.max_combo,
                hold_notes: attrs.n_hold_notes,
                ..Self::default()
            },
        }
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HitResult {
    None,
    Miss,
    Meh,
    Ok,
    Good,
    Great,
    Perfect,
    SmallTickMiss,
    SmallTickHit,
    LargeTickMiss,
    LargeTickHit,
    SmallBonus,
    LargeBonus,
    IgnoreMiss,
    IgnoreHit,
    ComboBreak,
    SliderTailHit,
    LegacyComboIncrease = 99,
}

impl HitResult {
    pub const ALL: [Self; 18] = [
        Self::None,
        Self::Miss,
        Self::Meh,
        Self::Ok,
        Self::Good,
        Self::Great,
        Self::Perfect,
        Self::SmallTickMiss,
        Self::SmallTickHit,
        Self::LargeTickMiss,
        Self::LargeTickHit,
        Self::SmallBonus,
        Self::LargeBonus,
        Self::IgnoreMiss,
        Self::IgnoreHit,
        Self::ComboBreak,
        Self::SliderTailHit,
        Self::LegacyComboIncrease,
    ];
}

impl TryFrom<i32> for HitResult {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|result| *result as i32 == value)
            .ok_or(value)
    }
}

impl Serialize for HitResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for HitResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = i32::deserialize(deserializer)?;
        Self::try_from(value)
            .map_err(|value| serde::de::Error::custom(format_args!("invalid hit result {value}")))
    }
}

/// Returns the target osu! hit window in milliseconds.
///
/// Catch (and an unknown numeric ruleset through `hit_window_numeric`) has no
/// hit windows and returns `Ok(None)`. Asking a supported ruleset for a result
/// it does not expose is an error, matching `HitWindows.WindowFor`.
pub fn hit_window(mode: Ruleset, od: f64, result: HitResult) -> Result<Option<f64>, Error> {
    fn range(difficulty: f64, max: f64, mid: f64, min: f64) -> f64 {
        if difficulty > 5.0 {
            mid + (min - mid) * (difficulty - 5.0) / 5.0
        } else if difficulty < 5.0 {
            mid + (max - mid) * (5.0 - difficulty) / 5.0
        } else {
            mid
        }
    }

    let floor_minus_half = |max, mid, min| range(od, max, mid, min).floor() - 0.5;
    let floor_plus_half = |max, mid, min| range(od, max, mid, min).floor() + 0.5;

    let window = match (mode, result) {
        (Ruleset::Osu, HitResult::Great) => floor_minus_half(80.0, 50.0, 20.0),
        (Ruleset::Osu, HitResult::Ok) => floor_minus_half(140.0, 100.0, 60.0),
        (Ruleset::Osu, HitResult::Meh) => floor_minus_half(200.0, 150.0, 100.0),
        (Ruleset::Osu, HitResult::Miss) => 400.0,
        (Ruleset::Taiko, HitResult::Great) => floor_minus_half(50.0, 35.0, 20.0),
        (Ruleset::Taiko, HitResult::Ok) => floor_minus_half(120.0, 80.0, 50.0),
        (Ruleset::Taiko, HitResult::Miss) => floor_minus_half(135.0, 95.0, 70.0),
        (Ruleset::Mania, HitResult::Perfect) => floor_plus_half(22.4, 19.4, 13.9),
        (Ruleset::Mania, HitResult::Great) => floor_plus_half(64.0, 49.0, 34.0),
        (Ruleset::Mania, HitResult::Good) => floor_plus_half(97.0, 82.0, 67.0),
        (Ruleset::Mania, HitResult::Ok) => floor_plus_half(127.0, 112.0, 97.0),
        (Ruleset::Mania, HitResult::Meh) => floor_plus_half(151.0, 136.0, 121.0),
        (Ruleset::Mania, HitResult::Miss) => floor_plus_half(188.0, 173.0, 158.0),
        (Ruleset::Catch, _) => return Ok(None),
        _ => {
            return Err(Error::UnsupportedHitResult {
                mode,
                result: result as i32,
            });
        }
    };
    Ok(Some(window))
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HitWindows {
    pub miss: Option<f64>,
    pub meh: Option<f64>,
    pub ok: Option<f64>,
    pub good: Option<f64>,
    pub great: Option<f64>,
    pub perfect: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CatchSpatialWindow {
    pub half_width: f64,
}

/// Returns the hit window used by the playable beatmap's per-object scoring.
///
/// Mania Classic changes windows based on whether the map was converted,
/// unless ScoreV2 is also active.
pub fn hit_window_for_map(
    map: &ParsedBeatmap,
    od: f64,
    result: HitResult,
) -> Result<Option<f64>, Error> {
    if map.mode != Ruleset::Mania {
        return hit_window(map.mode, od, result);
    }

    let raw_window = if map.classic_scoring() && !map.mania_score_v2 {
        if map.map.is_convert {
            match result {
                HitResult::Perfect => 16.0,
                HitResult::Great if od.round_ties_even() > 4.0 => 34.0,
                HitResult::Great => 47.0,
                HitResult::Good if od.round_ties_even() > 4.0 => 67.0,
                HitResult::Good => 77.0,
                HitResult::Ok => 97.0,
                HitResult::Meh => 121.0,
                HitResult::Miss => 158.0,
                _ => return hit_window(map.mode, od, result),
            }
        } else {
            let inverted_od = (10.0 - od).clamp(0.0, 10.0);

            match result {
                HitResult::Perfect => 16.0,
                HitResult::Great => 34.0 + 3.0 * inverted_od,
                HitResult::Good => 67.0 + 3.0 * inverted_od,
                HitResult::Ok => 97.0 + 3.0 * inverted_od,
                HitResult::Meh => 121.0 + 3.0 * inverted_od,
                HitResult::Miss => 158.0 + 3.0 * inverted_od,
                _ => return hit_window(map.mode, od, result),
            }
        }
    } else {
        let range = |max: f64, mid: f64, min: f64| {
            if od > 5.0 {
                mid + (min - mid) * (od - 5.0) / 5.0
            } else if od < 5.0 {
                mid + (max - mid) * (5.0 - od) / 5.0
            } else {
                mid
            }
        };

        match result {
            HitResult::Perfect => range(22.4, 19.4, 13.9),
            HitResult::Great => range(64.0, 49.0, 34.0),
            HitResult::Good => range(97.0, 82.0, 67.0),
            HitResult::Ok => range(127.0, 112.0, 97.0),
            HitResult::Meh => range(151.0, 136.0, 121.0),
            HitResult::Miss => range(188.0, 173.0, 158.0),
            _ => return hit_window(map.mode, od, result),
        }
    };

    let (difficulty_multiplier, speed_multiplier) =
        map.structured_mods
            .iter()
            .fold((1.0, 1.0), |(difficulty, speed), game_mod| {
                let is_easy = game_mod.acronym.eq_ignore_ascii_case("EZ");
                let is_hard_rock = game_mod.acronym.eq_ignore_ascii_case("HR");
                let difficulty = if is_easy {
                    difficulty / 1.4
                } else if is_hard_rock {
                    difficulty * 1.4
                } else {
                    difficulty
                };
                let is_faster = game_mod.acronym.eq_ignore_ascii_case("DT")
                    || game_mod.acronym.eq_ignore_ascii_case("NC");
                let is_slower = game_mod.acronym.eq_ignore_ascii_case("HT")
                    || game_mod.acronym.eq_ignore_ascii_case("DC");
                let default_speed = if is_faster {
                    1.5
                } else if is_slower {
                    0.75
                } else {
                    return (difficulty, speed);
                };
                let configured = match game_mod.settings.get("speed_change") {
                    Some(ModValue::Number(value)) if value.is_finite() => *value,
                    Some(ModValue::String(value)) => value.parse().unwrap_or(default_speed),
                    _ => default_speed,
                };
                let (min, max) = if is_faster {
                    (1.01, 2.0)
                } else {
                    (0.5, 0.99)
                };
                let speed = (configured.clamp(min, max) * 100.0).round_ties_even() / 100.0;

                (difficulty, speed)
            });

    Ok(Some(
        (raw_window * speed_multiplier / difficulty_multiplier).floor() + 0.5,
    ))
}

/// Computes osu!catch's spatial half-width in playfield units from circle size.
///
/// Mirrors osu!catch's `Catcher.CalculateCatchWidth`, also used by rosu-pp.
fn catch_spatial_window(cs: f32) -> f64 {
    const BASE_SIZE: f32 = 106.75;
    const ALLOWED_CATCH_RANGE: f32 = 0.8;

    let scale = (f64::from(1.0_f32)
        - f64::from(0.7_f32) * ((f64::from(cs) - 5.0) / 5.0)) as f32;
    f64::from(BASE_SIZE * scale.abs() * ALLOWED_CATCH_RANGE * 0.5)
}

/// Returns osu!catch's spatial catch boundary for the playable beatmap.
///
/// The half-width uses post-difficulty-mod circle size and playfield units.
/// Clock rate does not affect it.
pub fn catch_spatial_window_for_map(map: &ParsedBeatmap) -> Option<CatchSpatialWindow> {
    (map.mode() == Ruleset::Catch).then(|| CatchSpatialWindow {
        half_width: catch_spatial_window(map.map.cs),
    })
}

/// Returns every temporal hit window supported by the playable beatmap.
///
/// Values use the playable map's post-difficulty-mod overall difficulty and
/// do not include any clock-rate adjustment.
pub fn hit_windows_for_map(map: &ParsedBeatmap) -> Result<HitWindows, Error> {
    let od = f64::from(map.map.od);
    let window = |result| hit_window_for_map(map, od, result);

    match map.mode() {
        Ruleset::Osu => Ok(HitWindows {
            miss: window(HitResult::Miss)?,
            meh: window(HitResult::Meh)?,
            ok: window(HitResult::Ok)?,
            great: window(HitResult::Great)?,
            ..HitWindows::default()
        }),
        Ruleset::Taiko => Ok(HitWindows {
            miss: window(HitResult::Miss)?,
            ok: window(HitResult::Ok)?,
            great: window(HitResult::Great)?,
            ..HitWindows::default()
        }),
        Ruleset::Catch => Ok(HitWindows::default()),
        Ruleset::Mania => Ok(HitWindows {
            miss: window(HitResult::Miss)?,
            meh: window(HitResult::Meh)?,
            ok: window(HitResult::Ok)?,
            good: window(HitResult::Good)?,
            great: window(HitResult::Great)?,
            perfect: window(HitResult::Perfect)?,
        }),
    }
}

pub fn hit_window_numeric(mode: i32, od: f64, result: i32) -> Result<Option<f64>, Error> {
    let Some(mode) = Ruleset::from_mode_id(mode) else {
        return Ok(None);
    };
    let result = HitResult::try_from(result)
        .map_err(|result| Error::UnsupportedHitResult { mode, result })?;

    hit_window(mode, od, result)
}

/// Return all available hit windows in the same result order as osu!.
pub fn all_hit_windows(mode: Ruleset, od: f64) -> Vec<(HitResult, f64)> {
    const RESULTS: [HitResult; 6] = [
        HitResult::Miss,
        HitResult::Meh,
        HitResult::Ok,
        HitResult::Good,
        HitResult::Great,
        HitResult::Perfect,
    ];

    RESULTS
        .into_iter()
        .filter_map(|result| {
            hit_window(mode, od, result)
                .ok()
                .flatten()
                .map(|window| (result, window))
        })
        .collect()
}

/// Return the great hit window, or zero for a ruleset without hit windows.
pub fn great_hit_window(mode: Ruleset, od: f64) -> f64 {
    hit_window(mode, od, HitResult::Great)
        .ok()
        .flatten()
        .unwrap_or(0.0)
}

pub fn calculate_accuracy(
    mode: Ruleset,
    score: &ScoreInfoData,
    classic: bool,
    totals: ScoreTotals,
) -> f64 {
    fn divide(numerator: f64, denominator: f64) -> f64 {
        numerator / denominator
    }

    match mode {
        Ruleset::Osu => {
            let mut total =
                6.0 * f64::from(score.greats) + 2.0 * f64::from(score.oks) + f64::from(score.mehs);
            let mut max = 6.0 * f64::from(score.greats + score.oks + score.mehs + score.misses);

            if !classic {
                total += 3.0 * f64::from(score.slider_end_hits);
                max += 3.0 * f64::from(totals.slider_ends);

                let inferred_large_tick_hits =
                    i64::from(totals.large_ticks) - i64::from(score.large_tick_misses);
                total += 0.6 * inferred_large_tick_hits as f64;
                max += 0.6 * f64::from(totals.large_ticks);
            }

            divide(total, max)
        }
        Ruleset::Taiko => {
            let total = 2 * score.greats + score.oks;
            let max = 2 * (score.greats + score.oks + score.misses);
            divide(f64::from(total), f64::from(max))
        }
        Ruleset::Catch => {
            let hits = score.greats + score.large_tick_hits + score.small_tick_hits;
            let total = hits + score.misses + score.small_tick_misses;
            divide(f64::from(hits), f64::from(total))
        }
        Ruleset::Mania => {
            let perfect_weight = if classic { 300 } else { 305 };
            let total = perfect_weight * score.perfects
                + 300 * score.greats
                + 200 * score.goods
                + 100 * score.oks
                + 50 * score.mehs;
            let max = perfect_weight
                * (score.perfects
                    + score.greats
                    + score.goods
                    + score.oks
                    + score.mehs
                    + score.misses);

            divide(f64::from(total), f64::from(max))
        }
    }
}

pub fn accuracy(
    mode: Ruleset,
    score: LiveScoreState,
    classic: bool,
    max_slider_ends: u32,
    max_large_ticks: u32,
) -> f64 {
    calculate_accuracy(
        mode,
        &ScoreInfoData::from(score),
        classic,
        ScoreTotals {
            slider_ends: max_slider_ends,
            large_ticks: max_large_ticks,
            ..ScoreTotals::default()
        },
    )
}

pub fn simulate_score(
    mode: Ruleset,
    totals: ScoreTotals,
    target: f64,
    classic: bool,
) -> Result<ScoreInfoData, Error> {
    if (mode == Ruleset::Osu && (target.is_nan() || totals.objects == 0))
        || (mode != Ruleset::Osu && !target.is_finite())
    {
        return Err(Error::NonFiniteAccuracy);
    }

    let mut score = ScoreInfoData::default();

    match mode {
        Ruleset::Osu => {
            let target = target.clamp(0.0, 1.0);
            let total = i64::from(totals.objects);
            let mut good = 0_i64;
            let meh: i64;
            let mut misses = 0_i64;

            if target >= 0.25 {
                let ratio_50_to_100 = (1.0 - (target - 0.25) / 0.75).powi(2);
                let count_100_estimate =
                    6.0 * total as f64 * (1.0 - target) / (5.0 * ratio_50_to_100 + 4.0);
                let count_50_estimate = count_100_estimate * ratio_50_to_100;

                good = count_100_estimate.round_ties_even() as i64;
                meh = (count_100_estimate + count_50_estimate).round_ties_even() as i64 - good;
            } else if target >= 1.0 / 6.0 {
                let count_100_estimate = 6.0 * total as f64 * target - total as f64;
                let count_50_estimate = total as f64 - count_100_estimate;

                good = count_100_estimate.round_ties_even() as i64;
                meh = (count_100_estimate + count_50_estimate).round_ties_even() as i64 - good;
            } else {
                meh = (6.0 * total as f64 * target).round_ties_even() as i64;
                misses = total - meh;
            }

            score.greats = (total - good - meh - misses) as i32;
            score.oks = good as i32;
            score.mehs = meh as i32;
            score.misses = misses as i32;
            score.large_tick_misses = 0;
            score.slider_end_hits = totals.slider_ends.min(i32::MAX as u32) as i32;
        }
        Ruleset::Taiko => {
            let total = i64::from(totals.max_combo);
            let target_total = i64::from(calculator_int(target * total as f64 * 2.0)?);
            let greats = target_total - total;

            score.greats = greats as i32;
            score.oks = (total - greats) as i32;
        }
        Ruleset::Catch => {
            let max_combo = i64::from(totals.max_combo);
            let max_droplets = i64::from(totals.droplets);
            let max_fruits = i64::from(totals.fruits);
            let max_tiny_droplets = i64::from(totals.tiny_droplets);
            let misses = 0_i64;
            let droplets = max_droplets;
            let fruits = max_fruits - (misses - (max_droplets - droplets));
            let tiny_droplets = i64::from(calculator_int(
                target * (max_combo + max_tiny_droplets) as f64,
            )?) - fruits
                - droplets;
            let tiny_misses = max_tiny_droplets - tiny_droplets;

            score.greats = fruits as i32;
            score.large_tick_hits = droplets as i32;
            score.small_tick_hits = tiny_droplets as i32;
            score.small_tick_misses = tiny_misses as i32;
            score.misses = misses as i32;
        }
        Ruleset::Mania => {
            let total = i64::from(totals.objects)
                + if classic {
                    0
                } else {
                    i64::from(totals.hold_notes)
                };
            let perfect_value = if classic { 60_i64 } else { 61_i64 };
            let target_total = i64::from(calculator_int(target * (total * perfect_value) as f64)?);
            let mut remaining = total;
            let mut delta = (target_total - 10 * remaining).max(0);

            let perfects = (delta / (perfect_value - 10)).min(remaining);
            delta -= perfects * (perfect_value - 10);
            remaining -= perfects;

            let greats = (delta / 50).min(remaining);
            delta -= greats * 50;
            remaining -= greats;

            let goods = (delta / 30).min(remaining);
            delta -= goods * 30;
            remaining -= goods;

            let oks = (delta / 10).min(remaining);
            remaining -= oks;

            score.perfects = perfects as i32;
            score.greats = greats as i32;
            score.goods = goods as i32;
            score.oks = oks as i32;
            score.mehs = remaining as i32;
        }
    }

    score.accuracy = calculate_accuracy(mode, &score, classic, totals);

    Ok(score)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalculatedScore {
    pub raw: f64,
    pub total_score_without_mods: i64,
    pub total_score: i64,
}

/// Apply lazer's score-v2 raw score and checked `.NET long` boundaries.
pub fn calculate_total_score(
    accuracy: f64,
    combo_progress: f64,
    accuracy_progress: f64,
    bonus: f64,
    multiplier: f64,
) -> Result<CalculatedScore, Error> {
    let raw = 500_000.0 * accuracy * combo_progress
        + 500_000.0 * accuracy.powi(5) * accuracy_progress
        + bonus;
    let total_score_without_mods = calculator_long(raw)?;
    let total_score = calculator_long(total_score_without_mods as f64 * multiplier)?;

    Ok(CalculatedScore {
        raw,
        total_score_without_mods,
        total_score,
    })
}

fn calculator_int(value: f64) -> Result<i32, Error> {
    let rounded = value.round_ties_even();

    if !rounded.is_finite() || rounded < i32::MIN as f64 || rounded > i32::MAX as f64 {
        return Err(Error::NonFiniteAccuracy);
    }

    Ok(rounded as i32)
}

/// Convert a rounded calculator value at the explicit C# `long` boundary.
pub fn calculator_long(value: f64) -> Result<i64, Error> {
    let rounded = value.round_ties_even();

    if !rounded.is_finite() || rounded < i64::MIN as f64 || rounded >= -(i64::MIN as f64) {
        return Err(Error::ScoreOutOfRange("totalScore"));
    }

    Ok(rounded as i64)
}

pub fn generate_score(
    mode: Ruleset,
    totals: ScoreTotals,
    target: f64,
    classic: bool,
) -> Result<LiveScoreState, Error> {
    simulate_score(mode, totals, target, classic)?.try_into()
}


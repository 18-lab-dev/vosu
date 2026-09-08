use serde::{Deserialize, Serialize};

use crate::{
    DifficultyAttrsData, Error, LiveLazerEngine, LiveScoreState, ParsedBeatmap, Ruleset,
    ScoreInfoData,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreTotals {
    pub objects: u32,
    pub max_combo: u32,
    pub sliders: u32,
    pub slider_ends: u32,
    pub large_ticks: u32,
    pub hold_notes: u32,
    pub fruits: u32,
    pub droplets: u32,
    pub tiny_droplets: u32,
}
impl ScoreTotals {
    pub fn from_difficulty(attrs: &rosu_pp::any::DifficultyAttributes) -> Self {
        match attrs {
            rosu_pp::any::DifficultyAttributes::Osu(a) => Self {
                objects: a.n_objects(),
                max_combo: a.max_combo,
                sliders: a.n_sliders,
                slider_ends: a.n_sliders,
                large_ticks: a.n_large_ticks,
                ..Self::default()
            },
            rosu_pp::any::DifficultyAttributes::Taiko(a) => Self {
                objects: a.max_combo,
                max_combo: a.max_combo,
                ..Self::default()
            },
            rosu_pp::any::DifficultyAttributes::Catch(a) => Self {
                objects: a.n_fruits + a.n_droplets,
                max_combo: a.max_combo(),
                fruits: a.n_fruits,
                droplets: a.n_droplets,
                tiny_droplets: a.n_tiny_droplets,
                ..Self::default()
            },
            rosu_pp::any::DifficultyAttributes::Mania(a) => Self {
                objects: a.n_objects,
                max_combo: a.max_combo,
                hold_notes: a.n_hold_notes,
                ..Self::default()
            },
        }
    }
    pub fn from_data(attrs: &DifficultyAttrsData) -> Self {
        Self {
            objects: attrs
                .n_objects
                .max(attrs.n_circles + attrs.n_sliders + attrs.n_spinners)
                .max(0) as u32,
            max_combo: attrs.max_combo.max(0) as u32,
            sliders: attrs.n_sliders.max(0) as u32,
            slider_ends: attrs.n_sliders.max(0) as u32,
            large_ticks: attrs.n_large_ticks.max(0) as u32,
            hold_notes: attrs.n_hold_notes.max(0) as u32,
            fruits: attrs.n_fruits.max(0) as u32,
            droplets: attrs.n_droplets.max(0) as u32,
            tiny_droplets: attrs.n_tiny_droplets.max(0) as u32,
        }
    }
}

pub fn simulate_score(
    mode: Ruleset,
    totals: ScoreTotals,
    target: f64,
    classic: bool,
) -> Result<ScoreInfoData, Error> {
    if target.is_nan() || totals.objects == 0 && mode != Ruleset::Catch {
        return Err(Error::InvalidAccuracy(target));
    }
    let target = if mode == Ruleset::Osu {
        if target.is_infinite() {
            if target.is_sign_positive() { 1.0 } else { 0.0 }
        } else {
            target.clamp(0.0, 1.0)
        }
    } else {
        if !target.is_finite() {
            return Err(Error::InvalidAccuracy(target));
        }
        target
    };

    let mut score = ScoreInfoData::default();
    match mode {
        Ruleset::Osu => {
            let n = i32::try_from(totals.objects)
                .map_err(|_| Error::Calculation("object count overflow".into()))?;
            let relevant_accuracy = target.clamp(0.0, 1.0);
            let (goods, mehs) = if relevant_accuracy >= 0.25 {
                let ratio = (1.0 - (relevant_accuracy - 0.25) / 0.75).powi(2);
                let good = (6.0 * f64::from(n) * (1.0 - relevant_accuracy) / (5.0 * ratio + 4.0))
                    .round() as i32;
                let meh =
                    ((f64::from(good) + f64::from(good) * ratio).round() as i32 - good).max(0);
                (good, meh)
            } else if relevant_accuracy >= 1.0 / 6.0 {
                let good = (6.0 * f64::from(n) * relevant_accuracy - f64::from(n)).round() as i32;
                let meh = n - good;
                (good, meh)
            } else {
                let meh = (6.0 * f64::from(n) * relevant_accuracy).round() as i32;
                (0, meh)
            };
            score.oks = goods;
            score.mehs = mehs;
            score.greats = n - goods - mehs;
            if relevant_accuracy < 1.0 / 6.0 {
                score.misses = n - score.mehs;
                score.greats = 0;
            }
            score.slider_end_hits = i32::try_from(totals.slider_ends)
                .map_err(|_| Error::Calculation("slider end count overflow".into()))?;
        }
        Ruleset::Taiko => {
            let n = i32::try_from(totals.max_combo)
                .map_err(|_| Error::Calculation("object count overflow".into()))?;
            let target_total = checked_round(target * f64::from(n) * 2.0)?;
            score.greats = target_total - n;
            score.oks = n - score.greats;
        }
        Ruleset::Catch => {
            let fruits = i32::try_from(totals.fruits)
                .map_err(|_| Error::Calculation("fruit count overflow".into()))?;
            let droplets = i32::try_from(totals.droplets)
                .map_err(|_| Error::Calculation("droplet count overflow".into()))?;
            let tiny = i32::try_from(totals.tiny_droplets)
                .map_err(|_| Error::Calculation("tiny droplet count overflow".into()))?;
            let total = totals
                .max_combo
                .checked_add(totals.tiny_droplets)
                .ok_or_else(|| Error::Calculation("catch object count overflow".into()))?;
            let tiny_hits = checked_round(target * f64::from(total as i32))? - fruits - droplets;
            score.greats = fruits;
            score.large_tick_hits = droplets;
            score.small_tick_hits = tiny_hits;
            score.small_tick_misses = tiny - tiny_hits;
        }
        Ruleset::Mania => {
            let n = totals
                .objects
                .checked_add(if classic { 0 } else { totals.hold_notes })
                .ok_or_else(|| Error::Calculation("object count overflow".into()))?;
            let n =
                i32::try_from(n).map_err(|_| Error::Calculation("object count overflow".into()))?;
            let perfect_weight = if classic { 60 } else { 61 };
            let target_total = checked_round(target * f64::from(n) * f64::from(perfect_weight))?;
            let misses = 0;
            let mut remaining = n - misses;
            let mut delta = (target_total - 10 * remaining).max(0);
            let perfects = (delta / (perfect_weight - 10)).min(remaining);
            delta -= perfects * (perfect_weight - 10);
            remaining -= perfects;
            let greats = (delta / 50).min(remaining);
            delta -= greats * 50;
            remaining -= greats;
            let goods = (delta / 30).min(remaining);
            delta -= goods * 30;
            remaining -= goods;
            let oks = (delta / 10).min(remaining);
            remaining -= oks;
            score.perfects = perfects;
            score.greats = greats;
            score.goods = goods;
            score.oks = oks;
            score.mehs = remaining;
            score.misses = misses;
        }
    }
    score.accuracy = accuracy_with_totals(mode, score, classic, Some(totals))?;
    Ok(score)
}

fn checked_round(value: f64) -> Result<i32, Error> {
    if !value.is_finite() || value < f64::from(i32::MIN) - 1.0 || value > f64::from(i32::MAX) + 1.0
    {
        return Err(Error::Calculation("score projection overflow".into()));
    }
    let rounded = value.round();
    i32::try_from(rounded as i64)
        .map_err(|_| Error::Calculation("score projection overflow".into()))
}

pub(crate) fn accuracy_with_score_totals(
    mode: Ruleset,
    score: ScoreInfoData,
    classic: bool,
    totals: ScoreTotals,
) -> Result<f64, Error> {
    accuracy_with_totals(mode, score, classic, Some(totals))
}

fn accuracy_with_totals(
    mode: Ruleset,
    score: ScoreInfoData,
    classic: bool,
    totals: Option<ScoreTotals>,
) -> Result<f64, Error> {
    let signed = |x: i32| f64::from(x);
    let (num, den) = match mode {
        Ruleset::Osu => {
            let total = signed(score.greats)
                + signed(score.oks)
                + signed(score.mehs)
                + signed(score.misses);
            let mut num = 6.0 * signed(score.greats) + 2.0 * signed(score.oks) + signed(score.mehs);
            let mut den = 6.0 * total;
            if !classic {
                if let Some(totals) = totals {
                    num += 3.0 * signed(score.slider_end_hits);
                    den += 3.0 * f64::from(totals.slider_ends);
                    let tick_hits = f64::from(totals.large_ticks) - signed(score.large_tick_misses);
                    num += 0.6 * tick_hits;
                    den += 0.6 * f64::from(totals.large_ticks);
                } else {
                    num += 3.0 * signed(score.slider_end_hits);
                    den += 3.0 * signed(score.slider_end_hits);
                    num += 0.6 * signed(score.large_tick_hits);
                    den += 0.6 * signed(score.large_tick_hits + score.large_tick_misses);
                }
            }
            (num, den)
        }
        Ruleset::Taiko => {
            let total = signed(score.greats) + signed(score.oks) + signed(score.misses);
            (2.0 * signed(score.greats) + signed(score.oks), 2.0 * total)
        }
        Ruleset::Catch => {
            let hits = signed(score.greats)
                + signed(score.large_tick_hits)
                + signed(score.small_tick_hits);
            let total = hits + signed(score.misses) + signed(score.small_tick_misses);
            (hits, total)
        }
        Ruleset::Mania => {
            let total = signed(score.perfects)
                + signed(score.greats)
                + signed(score.goods)
                + signed(score.oks)
                + signed(score.mehs)
                + signed(score.misses);
            let perfect = if classic { 300.0 } else { 305.0 };
            (
                perfect * signed(score.perfects)
                    + 300.0 * signed(score.greats)
                    + 200.0 * signed(score.goods)
                    + 100.0 * signed(score.oks)
                    + 50.0 * signed(score.mehs),
                perfect * total,
            )
        }
    };
    if den == 0.0 {
        Err(Error::Calculation("accuracy has no judged objects".into()))
    } else {
        Ok(num / den)
    }
}

impl LiveLazerEngine {
    pub fn create_score(&self, map: &ParsedBeatmap, target: f64) -> Result<ScoreInfoData, Error> {
        let attrs = self.difficulty(map, &Default::default())?;
        simulate_score(
            map.mode(),
            ScoreTotals::from_difficulty(&attrs),
            target,
            map.classic_scoring(),
        )
    }

    /// Generate a score from retained difficulty counts for this ruleset.
    pub fn create_score_from_attrs(
        &self,
        map: &ParsedBeatmap,
        attrs: &DifficultyAttrsData,
        target: f64,
    ) -> Result<ScoreInfoData, Error> {
        let inner = attrs.inner_for(map.mode())?;
        simulate_score(
            map.mode(),
            ScoreTotals::from_difficulty(&inner),
            target,
            map.classic_scoring(),
        )
    }

    pub fn generate_score(
        &self,
        mode: Ruleset,
        totals: ScoreTotals,
        target: f64,
        classic: bool,
    ) -> Result<LiveScoreState, Error> {
        simulate_score(mode, totals, target, classic)?.try_into()
    }

    pub fn calculate_accuracy(
        &self,
        map: &ParsedBeatmap,
        score: ScoreInfoData,
    ) -> Result<f64, Error> {
        let attrs = self.difficulty(map, &Default::default())?;
        accuracy_with_totals(
            map.mode(),
            score,
            map.classic_scoring(),
            Some(ScoreTotals::from_difficulty(&attrs)),
        )
    }

    pub fn calculate_accuracy_from_attrs(
        &self,
        map: &ParsedBeatmap,
        attrs: &DifficultyAttrsData,
        score: ScoreInfoData,
    ) -> Result<f64, Error> {
        attrs.inner_for(map.mode())?;
        accuracy_with_totals(
            map.mode(),
            score,
            map.classic_scoring(),
            Some(ScoreTotals::from_data(attrs)),
        )
    }

    pub fn progressive_accuracy(
        &self,
        mode: Ruleset,
        state: LiveScoreState,
        classic: bool,
        slider_ends: u32,
        ticks: u32,
    ) -> f64 {
        accuracy_with_totals(
            mode,
            ScoreInfoData::from(state),
            classic,
            Some(ScoreTotals {
                slider_ends,
                large_ticks: ticks,
                ..ScoreTotals::default()
            }),
        )
        .unwrap_or(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotalScoreResult {
    pub total_score_without_mods: i64,
    pub total_score: i64,
}

pub fn calculate_total_score(
    acc: f64,
    combo: f64,
    bonus: f64,
    legacy_bonus: f64,
    multiplier: f64,
) -> Result<TotalScoreResult, Error> {
    if ![acc, combo, bonus, legacy_bonus, multiplier]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(Error::Calculation("non-finite total score input".into()));
    }
    let without_f = 500_000.0 * acc * combo + 500_000.0 * acc.powi(5) * bonus + legacy_bonus;
    let without = checked_i64_round(without_f)?;
    let total = checked_i64_round(without as f64 * multiplier)?;
    if without < 0 || total < 0 {
        return Err(Error::Calculation(
            "total score must be non-negative".into(),
        ));
    }
    Ok(TotalScoreResult {
        total_score_without_mods: without,
        total_score: total,
    })
}

fn checked_i64_round(value: f64) -> Result<i64, Error> {
    if !value.is_finite() {
        return Err(Error::Calculation("total score overflow".into()));
    }
    let rounded = value.round();
    if rounded < i64::MIN as f64 || rounded >= 2_f64.powi(63) {
        return Err(Error::Calculation("total score overflow".into()));
    }
    Ok(rounded as i64)
}

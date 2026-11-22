use super::{DifficultyAttributes, Ruleset, ScoreInput};

pub(super) fn max_achievable(
    mode: Ruleset,
    combo: i64,
    current: ScoreInput,
    cur_diff: DifficultyAttributes,
    max: ScoreInput,
    full_diff: DifficultyAttributes,
) -> ScoreInput {
    let mut score = populate_max(mode, current, cur_diff, max);
    let remaining_combo = max.max_combo - cur_diff.max_combo;
    score.max_combo = score.max_combo.max(combo + remaining_combo);
    if current.is_legacy_score && mode == Ruleset::Osu {
        let combo_base =
            (cur_diff.max_combo - 1).max(0) as f64 + (max.max_combo - 1).max(0) as f64 - 1.0;
        let adjusted = (combo - 1).max(0) as f64 * 2.0 + remaining_combo as f64 - 1.0;
        let remaining_acc = std_max_acc(full_diff) - std_max_acc(cur_diff);
        let remaining_combo_score = (full_diff.maximum_legacy_combo_score
            - cur_diff.maximum_legacy_combo_score)
            * (adjusted / combo_base);
        // The source passes the resulting JS number to the calculator's Int64
        // score field, truncating only after adding both score components.
        score.total_score =
            (score.total_score as f64 + remaining_acc + remaining_combo_score) as i64;
    }
    score
}

pub(super) fn full_combo(
    mode: Ruleset,
    current: ScoreInput,
    cur_diff: DifficultyAttributes,
    max: ScoreInput,
) -> ScoreInput {
    let mut score = populate_max(mode, current, cur_diff, max);
    score.max_combo = max.max_combo;
    let max_hit = get_max(mode, score) + current.misses + current.combo_breaks;
    set_max(mode, &mut score, max_hit);
    score.combo_breaks = 0;
    score.misses = 0;
    score.large_tick_hits = max.large_tick_hits;
    score.large_tick_misses = 0;
    score.slider_end_hits = max.slider_end_hits;
    score
}

fn populate_max(
    mode: Ruleset,
    current: ScoreInput,
    cur_diff: DifficultyAttributes,
    max: ScoreInput,
) -> ScoreInput {
    let mut score = current;
    score.large_tick_hits = max.large_tick_hits - current.large_tick_misses;
    score.slider_end_hits = max.slider_end_hits - (cur_diff.n_sliders - current.slider_end_hits);
    score.small_tick_hits = max.small_tick_hits - current.small_tick_misses;
    set_max(
        mode,
        &mut score,
        get_max(mode, max) - current.goods - current.oks - current.mehs - current.misses,
    );
    score
}

fn set_max(mode: Ruleset, score: &mut ScoreInput, count: i64) {
    if mode == Ruleset::Mania {
        score.perfects = count;
    } else {
        score.greats = count;
    }
}
fn get_max(mode: Ruleset, score: ScoreInput) -> i64 {
    if mode == Ruleset::Mania {
        score.perfects
    } else {
        score.greats
    }
}
fn std_max_acc(diff: DifficultyAttributes) -> f64 {
    diff.n_objects as f64 * (300.0 + diff.nested_score_per_object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fc_replaces_misses_and_tick_misses_without_touching_bonus_counts() {
        let current = ScoreInput {
            max_combo: 12,
            greats: 8,
            misses: 2,
            combo_breaks: 1,
            large_tick_misses: 3,
            small_tick_misses: 4,
            ..ScoreInput::default()
        };
        let max = ScoreInput {
            max_combo: 40,
            greats: 40,
            large_tick_hits: 10,
            small_tick_hits: 12,
            slider_end_hits: 5,
            ..ScoreInput::default()
        };
        let fc = full_combo(
            Ruleset::Osu,
            current,
            DifficultyAttributes {
                n_sliders: 5,
                ..Default::default()
            },
            max,
        );
        assert_eq!(fc.max_combo, 40);
        assert_eq!(fc.greats, 41);
        assert_eq!(fc.misses, 0);
        assert_eq!(fc.combo_breaks, 0);
        assert_eq!(fc.large_tick_hits, 10);
        assert_eq!(fc.large_tick_misses, 0);
        assert_eq!(fc.small_tick_hits, 8);
        assert_eq!(fc.slider_end_hits, 5);
    }

    #[test]
    fn max_achievable_adds_nested_accuracy_and_legacy_combo_score() {
        let current = ScoreInput {
            is_legacy_score: true,
            max_combo: 2,
            ..Default::default()
        };
        let max = ScoreInput {
            max_combo: 10,
            greats: 10,
            ..Default::default()
        };
        let cur = DifficultyAttributes {
            max_combo: 2,
            n_objects: 2,
            nested_score_per_object: 1.0,
            maximum_legacy_combo_score: 600.0,
            ..Default::default()
        };
        let full = DifficultyAttributes {
            n_objects: 10,
            nested_score_per_object: 2.0,
            maximum_legacy_combo_score: 4000.0,
            ..Default::default()
        };
        let projected = max_achievable(Ruleset::Osu, 2, current, cur, max, full);
        assert_eq!(projected.max_combo, 10);
        assert_eq!(projected.total_score, 5_818);
    }
}

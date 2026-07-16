pub(crate) struct BeatmapAttributesExt;

impl BeatmapAttributesExt {
    pub(crate) fn difficulty_range(difficulty: f64, min: f64, mid: f64, max: f64) -> f64 {
        if difficulty > 5.0 {
            mid + (max - mid) * Self::difficulty_range_value(difficulty)
        } else if difficulty < 5.0 {
            mid + (mid - min) * Self::difficulty_range_value(difficulty)
        } else {
            mid
        }
    }

    pub(crate) fn difficulty_range_value(difficulty: f64) -> f64 {
        (difficulty - 5.0) / 5.0
    }

    pub(crate) fn inverse_difficulty_range(
        difficulty_value: f64,
        diff0: f64,
        diff5: f64,
        diff10: f64,
    ) -> f64 {
        if f64::signum(difficulty_value - diff5) == f64::signum(diff10 - diff5) {
            (difficulty_value - diff5) / (diff10 - diff5) * 5.0 + 5.0
        } else {
            (difficulty_value - diff5) / (diff5 - diff0) * 5.0 + 5.0
        }
    }

    pub(crate) const fn osu_great_hit_window_to_od(hit_window: f64) -> f64 {
        (79.5 - hit_window) / 6.0
    }
}

#[cfg(test)]
mod tests {
    use super::BeatmapAttributesExt;

    #[test]
    fn inverse_difficulty_range_uses_exact_signum_branches() {
        let midpoint = 50.0_f64;
        let just_above = midpoint.next_up();
        let just_below = midpoint.next_down();

        assert_eq!(
            BeatmapAttributesExt::inverse_difficulty_range(midpoint, 40.0, midpoint, 70.0),
            5.0
        );
        assert_eq!(
            BeatmapAttributesExt::inverse_difficulty_range(just_above, 40.0, midpoint, 70.0),
            (just_above - midpoint) / 20.0 * 5.0 + 5.0
        );
        assert_eq!(
            BeatmapAttributesExt::inverse_difficulty_range(just_below, 40.0, midpoint, 70.0),
            (just_below - midpoint) / 10.0 * 5.0 + 5.0
        );
    }
}

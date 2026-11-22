use super::{
    CalculationOutcome, ClientScoring, HitStatistics, ProgressiveBeatmap, Ruleset, ScoreInput,
    SkipReason,
};

#[derive(Debug, Clone)]
pub struct ResultFrame<'a> {
    pub beatmap_checksum: &'a str,
    pub mods_checksum: &'a str,
    pub mode: Ruleset,
    pub player_name: &'a str,
    /// Deliberately excluded from the source cache key. Tournament result users
    /// omit rank, and rank changes must not trigger a recalculation.
    pub tournament_rank: Option<i64>,
    pub client: ClientScoring,
    pub score: i64,
    pub max_combo: i64,
    pub statistics: HitStatistics,
}

impl ResultFrame<'_> {
    /// Exact concatenation from resultScreen.ts. Score, hit counts, date, online
    /// id, client type, and tournament rank are intentionally omitted.
    pub fn source_cache_key(&self) -> String {
        format!(
            "{}{}{}{}",
            self.beatmap_checksum, self.mods_checksum, self.mode as u8, self.player_name
        )
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ResultPerformance {
    pub pp: f64,
    pub fc_pp: f64,
}

#[derive(Default)]
pub struct ResultCalculator {
    previous_beatmap: Option<String>,
    performance: ResultPerformance,
}

impl ResultCalculator {
    pub fn performance(&self) -> ResultPerformance {
        self.performance
    }

    pub fn reset(&mut self) {
        self.previous_beatmap = None;
        self.performance = ResultPerformance::default();
    }

    pub fn update<B: ProgressiveBeatmap>(
        &mut self,
        beatmap: Option<&B>,
        frame: &ResultFrame<'_>,
    ) -> CalculationOutcome {
        let key = frame.source_cache_key();
        if self.previous_beatmap.as_deref() == Some(&key) {
            return CalculationOutcome::Unchanged;
        }
        let Some(beatmap) = beatmap else {
            return CalculationOutcome::Skipped(SkipReason::BeatmapUnavailable);
        };

        let mut score =
            ScoreInput::from_play(frame.score, frame.max_combo, frame.client, frame.statistics);
        score.accuracy = match beatmap.calculate_accuracy(&score) {
            Ok(accuracy) => accuracy,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };
        let current = match beatmap.calculate_performance(beatmap.full_difficulty(), &score) {
            Ok(performance) => performance,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };

        let mut fc = super::score::full_combo(
            frame.mode,
            score,
            beatmap.difficulty_summary(beatmap.full_difficulty()),
            *beatmap.max_score(),
        );
        fc.accuracy = match beatmap.calculate_accuracy(&fc) {
            Ok(accuracy) => accuracy,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };
        let fc_performance = match beatmap.calculate_performance(beatmap.full_difficulty(), &fc) {
            Ok(performance) => performance,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };

        self.performance = ResultPerformance {
            pp: current.pp,
            fc_pp: fc_performance.pp,
        };
        self.previous_beatmap = Some(key);
        CalculationOutcome::Updated
    }
}

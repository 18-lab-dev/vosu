use super::{
    CalculationOutcome, GameplayFrame, GameplayGate, GradualProgression, ProgressiveAttributes,
    ProgressiveBeatmap, ScoreInput, SkipReason,
};

pub struct GameplayCalculator<G> {
    gradual: Option<G>,
    previous_state: Option<String>,
    previous_play_time: f64,
    attributes: ProgressiveAttributes,
}

impl<G> Default for GameplayCalculator<G> {
    fn default() -> Self {
        Self {
            gradual: None,
            previous_state: None,
            previous_play_time: 0.0,
            attributes: ProgressiveAttributes::default(),
        }
    }
}

impl<G> GameplayCalculator<G> {
    pub fn attributes(&self) -> &ProgressiveAttributes {
        &self.attributes
    }

    pub fn reset_quick(&mut self) {
        self.previous_play_time = 0.0;
        self.gradual = None;
    }

    pub fn reset_for_retry(&mut self, fc_pp_100: f64) {
        self.reset_quick();
        self.clear_attributes();
        self.attributes.fc_pp = fc_pp_100;
    }

    pub fn clear_attributes(&mut self) {
        self.attributes = ProgressiveAttributes::default();
    }
}

impl<G> GameplayCalculator<G> {
    pub fn update<B>(&mut self, beatmap: Option<&B>, frame: GameplayFrame<'_>) -> CalculationOutcome
    where
        G: GradualProgression,
        B: ProgressiveBeatmap<Gradual = G, Error = G::Error, Difficulty = G::Difficulty>,
    {
        let skipped = match frame.gate {
            GameplayGate::Active => None,
            GameplayGate::CalculationDisabled => Some(SkipReason::CalculationDisabled),
            GameplayGate::MissingGameFolder => Some(SkipReason::MissingGameFolder),
            GameplayGate::BeatmapUnavailable => Some(SkipReason::BeatmapUnavailable),
            // Both source instance loops reset progression before calling
            // gameplay.updateState in the same transition; calculation still
            // runs and the gradual engine decides whether the prefix is empty.
            GameplayGate::BeforeFirstObject => {
                self.reset_quick();
                None
            }
            GameplayGate::Retry => {
                self.reset_for_retry(frame.reset_fc_pp);
                None
            }
            GameplayGate::SongSelectionReset => {
                self.reset_for_retry(frame.reset_fc_pp);
                Some(SkipReason::SongSelectionReset)
            }
            GameplayGate::NotPlaying => Some(SkipReason::NotPlaying),
            GameplayGate::Tournament => Some(SkipReason::Tournament),
        };
        if let Some(reason) = skipped {
            return CalculationOutcome::Skipped(reason);
        }

        let Some(beatmap) = beatmap else {
            return CalculationOutcome::Skipped(SkipReason::BeatmapUnavailable);
        };
        let state = frame.key.source_key();
        if self.previous_state.as_deref() != Some(&state) || self.gradual.is_none() {
            match beatmap.create_gradual() {
                Ok(gradual) => self.gradual = Some(gradual),
                Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
            }
            self.previous_state = Some(state);
        }

        let time_offset = frame.play_time - self.previous_play_time;
        if time_offset <= 0.0 {
            if time_offset == 0.0 {
                return CalculationOutcome::Unchanged;
            }
            match beatmap.create_gradual() {
                Ok(gradual) => self.gradual = Some(gradual),
                Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
            }
        }

        let gradual = self.gradual.as_mut().expect("gradual initialized above");
        let offset = match gradual.skip_to_time(frame.play_time) {
            Ok(offset) => offset,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };
        if offset == 0 {
            return CalculationOutcome::Unchanged;
        }
        let current_difficulty = match gradual.difficulty() {
            Ok(attrs) => attrs,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };
        let current_summary = beatmap.difficulty_summary(&current_difficulty);

        let mut score =
            ScoreInput::from_play(frame.score, frame.max_combo, frame.client, frame.statistics);
        score.accuracy = match gradual.progressive_accuracy(&score) {
            Ok(accuracy) => accuracy,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };
        let current = match gradual.calculate_performance(&score) {
            Ok(performance) => performance,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };

        let maximum = *beatmap.max_score();
        let achievable = super::score::max_achievable(
            frame.mode,
            frame.combo,
            score,
            current_summary,
            maximum,
            beatmap.difficulty_summary(beatmap.full_difficulty()),
        );
        let achievable_accuracy = match beatmap.calculate_accuracy(&achievable) {
            Ok(accuracy) => accuracy,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };
        let mut achievable = achievable;
        achievable.accuracy = achievable_accuracy;
        let max_achievable =
            match beatmap.calculate_performance(beatmap.full_difficulty(), &achievable) {
                Ok(performance) => performance,
                Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
            };

        let mut fc = super::score::full_combo(frame.mode, score, current_summary, maximum);
        fc.accuracy = match beatmap.calculate_accuracy(&fc) {
            Ok(accuracy) => accuracy,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };
        let fc_performance = match beatmap.calculate_performance(beatmap.full_difficulty(), &fc) {
            Ok(performance) => performance,
            Err(error) => return CalculationOutcome::RetainedError(error.to_string()),
        };

        self.attributes = ProgressiveAttributes {
            stars: current_summary.stars,
            pp: current.pp,
            max_achieved: self.attributes.max_achieved.max(current.pp),
            max_achievable: max_achievable.pp,
            fc_pp: fc_performance.pp,
            current,
            fc: fc_performance,
        };
        self.previous_play_time = frame.play_time;
        CalculationOutcome::Updated
    }
}

use crate::{
    any::difficulty::skills::HarmonicSkill,
    model::mods::GameMods,
    osu::difficulty::{evaluators::ReadingEvaluator, object::OsuDifficultyObject},
};

#[derive(Clone)]
pub struct Reading {
    harmonic: HarmonicSkill,
    object_start_times: Vec<f64>,
    current_strain: f64,
    has_hidden_mod: bool,
    has_touch_device_mod: bool,
    magnetised_strength: Option<f64>,
    has_relax_mod: bool,
    has_autopilot_mod: bool,
    overall_difficulty: f64,
    time_preempt: f64,
    clock_rate: f64,
}

impl Reading {
    const HARMONIC_SCALE: f64 = 1.0;
    const DECAY_EXPONENT: f64 = 0.9;
    const STRAIN_DECAY_BASE: f64 = 0.8;
    const SKILL_MULTIPLIER: f64 = 2.5;
    const REDUCED_DIFFICULTY_DURATION: f64 = 60_000.0;

    /// `overall_difficulty` must be the immediate OD derived from the full
    /// great hit window: `(79.5 - great_hit_window) / 6.0`, where rosu-pp's
    /// `great_hit_window` is the half-window attribute.
    ///
    /// `time_preempt` is the unadjusted preempt stored on `OsuObject`, while
    /// `clock_rate` is the actual calculation rate (including a custom rate).
    pub fn new(
        mods: &GameMods,
        overall_difficulty: f64,
        time_preempt: f64,
        clock_rate: f64,
        total_objects: usize,
    ) -> Self {
        let has_hidden_mod = mods.hd() && !mods.hd_only_fade_approach_circles().unwrap_or(false);

        Self {
            harmonic: HarmonicSkill::default(),
            object_start_times: Vec::with_capacity(total_objects.saturating_sub(1)),
            current_strain: 0.0,
            has_hidden_mod,
            has_touch_device_mod: mods.td(),
            magnetised_strength: mods.attraction_strength(),
            has_relax_mod: mods.rx(),
            has_autopilot_mod: mods.ap(),
            overall_difficulty,
            time_preempt,
            clock_rate,
        }
    }

    pub fn process<'a>(
        &mut self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) {
        self.object_start_times.push(curr.start_time);

        let decay = Self::STRAIN_DECAY_BASE.powf(curr.delta_time / 1000.0);
        self.current_strain *= decay;
        self.current_strain += self.calculate_adjusted_difficulty(curr, objects)
            * (1.0 - decay)
            * Self::SKILL_MULTIPLIER;

        self.harmonic.process(self.current_strain);
    }

    fn calculate_adjusted_difficulty<'a>(
        &self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        let mut difficulty = ReadingEvaluator::evaluate_diff_of(
            curr,
            objects,
            self.has_hidden_mod,
            self.time_preempt,
            self.clock_rate,
        );

        if self.has_touch_device_mod {
            difficulty = difficulty.powf(0.89);
        }

        if let Some(magnetised_strength) = self.magnetised_strength {
            difficulty *= 1.0 - magnetised_strength;
        }

        if self.has_relax_mod {
            difficulty *= 0.4;
        }

        if self.has_autopilot_mod {
            difficulty *= 0.1;
        }

        difficulty * (0.825 + self.overall_difficulty.max(0.0).powf(2.2) / 1125.0)
    }

    pub fn difficulty_value(&mut self) -> f64 {
        let object_start_times = &self.object_start_times;

        self.harmonic
            .difficulty_value(Self::HARMONIC_SCALE, Self::DECAY_EXPONENT, |difficulties| {
                Self::transform_difficulties(difficulties, object_start_times);
            })
    }

    pub fn count_top_weighted_object_difficulties(&self, difficulty_value: f64) -> f64 {
        let object_start_times = &self.object_start_times;

        self.harmonic.count_top_weighted_object_difficulties(
            difficulty_value,
            Self::HARMONIC_SCALE,
            Self::DECAY_EXPONENT,
            |difficulties| {
                Self::transform_difficulties(difficulties, object_start_times);
            },
            1.15,
            5.0,
        )
    }

    fn transform_difficulties(difficulties: &mut Vec<f64>, object_start_times: &[f64]) {
        // This filtering precedes the positional 60-second transform in the
        // target. The positional index below is therefore an index among
        // positive difficulties, while the denominator still counts all
        // processed objects in the first minute.
        difficulties.retain(|&difficulty| difficulty > 0.0);

        let Some(&first_start_time) = object_start_times.first() else {
            return;
        };

        let reduced_duration = first_start_time + Self::REDUCED_DIFFICULTY_DURATION;
        let reduced_note_count = object_start_times
            .iter()
            .take_while(|&&start_time| start_time <= reduced_duration)
            .count();

        for (index, difficulty) in difficulties.iter_mut().take(reduced_note_count).enumerate() {
            let position = (index as f64 / reduced_note_count as f64).clamp(0.0, 1.0);
            let scale = (1.0 + 9.0 * position).log10();

            // Lerp(0, 1, scale) is exactly `scale`.
            *difficulty *= scale;
        }
    }

    /// The target HarmonicSkill stores one value per processed difficulty
    /// object, including the current decayed carry-over strain.
    pub fn object_difficulties(&self) -> &[f64] {
        self.harmonic.object_difficulties()
    }
}

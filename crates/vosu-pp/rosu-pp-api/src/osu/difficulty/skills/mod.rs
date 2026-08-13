use crate::{any::difficulty::skills::StrainSkill, model::mods::GameMods};

use self::{aim::Aim, flashlight::Flashlight, reading::Reading, speed::Speed};

use super::{object::OsuDifficultyObject, scaling_factor::ScalingFactor};

pub mod aim;
pub mod flashlight;
pub mod reading;
pub mod speed;

pub struct OsuSkills {
    pub aim: Aim,
    pub aim_no_sliders: Aim,
    pub speed: Speed,
    pub flashlight: Flashlight,
    pub reading: Reading,
}

impl OsuSkills {
    pub fn new(
        mods: &GameMods,
        scaling_factor: &ScalingFactor,
        great_hit_window: f64,
        time_preempt: f64,
        total_objects: usize,
    ) -> Self {
        let hit_window = 2.0 * great_hit_window;
        let overall_difficulty = (79.5 - great_hit_window) / 6.0;

        let aim = Aim::new(true, mods, overall_difficulty);
        let aim_no_sliders = Aim::new(false, mods, overall_difficulty);
        let speed = Speed::new(hit_window, mods);
        let flashlight = Flashlight::new(
            mods,
            scaling_factor.radius,
            total_objects,
            overall_difficulty,
        );
        let reading = Reading::new(mods, time_preempt, great_hit_window);

        Self {
            aim,
            aim_no_sliders,
            speed,
            flashlight,
            reading,
        }
    }

    pub fn process(&mut self, curr: &OsuDifficultyObject<'_>, objects: &[OsuDifficultyObject<'_>]) {
        self.aim.process(curr, objects);
        self.aim_no_sliders.process(curr, objects);
        self.speed.process(curr, objects);
        self.flashlight.process(curr, objects);
        self.reading.process(curr, objects);
    }
}

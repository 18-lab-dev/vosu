use crate::{
    any::difficulty::skills::StrainSkill, model::mods::GameMods,
    osu::difficulty::object::OsuDifficultyObject,
};

use self::{aim::Aim, flashlight::Flashlight, reading::Reading, speed::Speed};

use super::scaling_factor::ScalingFactor;

pub mod aim;
pub mod flashlight;
pub mod reading;
pub mod speed;
pub mod strain;

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
        clock_rate: f64,
        total_objects: u32,
    ) -> Self {
        let hit_window = 2.0 * great_hit_window;
        let overall_difficulty = (79.5 - great_hit_window) / 6.0;

        let aim = Aim::new(
            true,
            scaling_factor.radius,
            overall_difficulty,
            mods.ap(),
            mods.td(),
            mods.rx(),
            mods.attraction_strength(),
        );
        let aim_no_sliders = Aim::new(
            false,
            scaling_factor.radius,
            overall_difficulty,
            mods.ap(),
            mods.td(),
            mods.rx(),
            mods.attraction_strength(),
        );
        let speed = Speed::new(mods, hit_window);
        let flashlight = Flashlight::new(
            mods,
            scaling_factor.radius,
            time_preempt,
            total_objects,
            overall_difficulty,
        );
        let reading = Reading::new(
            mods,
            overall_difficulty,
            time_preempt,
            clock_rate,
            total_objects as usize,
        );

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

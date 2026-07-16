use std::{cmp, pin::Pin};

use rosu_map::section::general::GameMode;
use skills::flashlight::Flashlight;

use crate::{
    Beatmap,
    any::{
        CalculateError,
        difficulty::{Difficulty, skills::StrainSkill},
    },
    model::{
        beatmap::{BeatmapAttributes, attributes::BeatmapAttributesExt},
        mode::ConvertError,
        mods::GameMods,
    },
    osu::{
        convert::{convert_objects, prepare_map},
        difficulty::{object::OsuDifficultyObject, scaling_factor::ScalingFactor},
        legacy_score_simulator::OsuLegacyScoreSimulator,
        object::OsuObject,
        performance::PERFORMANCE_BASE_MULTIPLIER,
        utils::legacy_score::NestedScorePerObject,
    },
};

use self::skills::OsuSkills;

use super::attributes::OsuDifficultyAttributes;

mod evaluators;
pub mod gradual;
mod object;
pub mod rating;
pub mod scaling_factor;
pub mod skills;

const HD_FADE_IN_DURATION_MULTIPLIER: f64 = 0.4;
const HD_FADE_OUT_DURATION_MULTIPLIER: f64 = 0.3;

pub fn difficulty(
    difficulty: &Difficulty,
    map: &Beatmap,
) -> Result<OsuDifficultyAttributes, ConvertError> {
    let map = prepare_map(difficulty, map)?;

    Ok(calculate_difficulty(difficulty, &map))
}

pub fn checked_difficulty(
    difficulty: &Difficulty,
    map: &Beatmap,
) -> Result<OsuDifficultyAttributes, CalculateError> {
    let map = prepare_map(difficulty, map)?;
    map.check_suspicion()?;

    Ok(calculate_difficulty(difficulty, &map))
}

fn calculate_difficulty(difficulty: &Difficulty, map: &Beatmap) -> OsuDifficultyAttributes {
    debug_assert_eq!(map.mode, GameMode::Osu);

    let DifficultyValues {
        osu_objects,
        mut skills,
        mut attrs,
    } = DifficultyValues::calculate(difficulty, map);

    let mods = difficulty.get_mods();
    let passed_objects = difficulty.get_passed_objects();

    DifficultyValues::eval(&mut attrs, mods, &mut skills);

    let mut simulator = OsuLegacyScoreSimulator::new(&osu_objects, map, passed_objects);

    let score_attrs = simulator.simulate();
    attrs.maximum_legacy_combo_score = score_attrs.combo_score as f64;

    let map_attrs = map.attributes().difficulty(difficulty).build();

    attrs.legacy_score_base_multiplier = f64::from(OsuLegacyScoreSimulator::score_multiplier(
        map,
        &map_attrs,
        passed_objects,
    ));

    let slider_nested_score_per_object =
        NestedScorePerObject::calculate(&osu_objects, passed_objects);
    attrs.nested_score_per_object = slider_nested_score_per_object;

    attrs
}

pub struct OsuDifficultySetup {
    scaling_factor: ScalingFactor,
    map_attrs: BeatmapAttributes,
    attrs: OsuDifficultyAttributes,
    time_preempt: f64,
}

impl OsuDifficultySetup {
    pub fn new(difficulty: &Difficulty, map: &Beatmap) -> Self {
        let map_attrs = map.attributes().difficulty(difficulty).build();
        let hit_windows = map_attrs.hit_windows();
        let scaling_factor = ScalingFactor::new(map_attrs.cs());

        let attrs = OsuDifficultyAttributes {
            ar: map_attrs.apply_clock_rate().ar,
            hp: f64::from(map_attrs.hp()),
            great_hit_window: hit_windows.od_great.unwrap_or(0.0),
            ok_hit_window: hit_windows.od_ok.unwrap_or(0.0),
            meh_hit_window: hit_windows.od_meh.unwrap_or(0.0),
            ..Default::default()
        };

        // osu! stores `TimePreempt` as `DifficultyRangeInt(AR, ...)` on
        // each hit object before stacking. Computing it directly avoids the
        // different rounding produced by undoing clock-rate adjustment.
        let time_preempt = difficulty_range_int_preempt(map_attrs.ar());

        Self {
            scaling_factor,
            map_attrs,
            attrs,
            time_preempt,
        }
    }
}

pub struct DifficultyValues {
    pub osu_objects: Box<[OsuObject]>,
    pub skills: OsuSkills,
    pub attrs: OsuDifficultyAttributes,
}

impl DifficultyValues {
    pub fn calculate(difficulty: &Difficulty, map: &Beatmap) -> Self {
        let mods = difficulty.get_mods();
        let take = difficulty.get_passed_objects();

        let OsuDifficultySetup {
            scaling_factor,
            map_attrs,
            mut attrs,
            time_preempt,
        } = OsuDifficultySetup::new(difficulty, map);

        let mut osu_objects = convert_objects(
            map,
            &scaling_factor,
            mods.reflection(),
            mods.freeze_frame(),
            time_preempt,
            take,
            &mut attrs,
        );

        let osu_object_iter = osu_objects.iter_mut().map(Pin::new);

        let diff_objects =
            Self::create_difficulty_objects(difficulty, &scaling_factor, osu_object_iter);

        let great_hit_window = map_attrs.hit_windows().od_great.unwrap_or(0.0);

        let total_objects = cmp::min(map.hit_objects.len(), take) as u32;
        let mut skills = OsuSkills::new(
            mods,
            &scaling_factor,
            great_hit_window,
            time_preempt,
            difficulty.get_clock_rate(),
            total_objects,
        );

        // The first hit object has no difficulty object
        let take_diff_objects = cmp::min(map.hit_objects.len(), take).saturating_sub(1);

        for hit_object in diff_objects.iter().take(take_diff_objects) {
            skills.process(hit_object, &diff_objects);
        }

        Self {
            osu_objects,
            skills,
            attrs,
        }
    }

    /// Process the difficulty values and store the results in `attrs`.
    pub fn eval(attrs: &mut OsuDifficultyAttributes, mods: &GameMods, skills: &mut OsuSkills) {
        let OsuSkills {
            aim,
            aim_no_sliders,
            speed,
            flashlight,
            reading,
        } = skills;

        let mut aim_eval = aim.clone();
        let mut aim_no_sliders_eval = aim_no_sliders.clone();
        let mut speed_eval = speed.clone();
        let mut reading_eval = reading.clone();
        let aim_difficulty_value = aim_eval.difficulty_value();
        let aim_no_sliders_difficulty_value = aim_no_sliders_eval.difficulty_value();
        let speed_difficulty_value = speed_eval.difficulty_value();
        let reading_difficulty_value = reading_eval.difficulty_value();

        let aim_difficult_strain_count = aim_eval.count_top_weighted_strains(aim_difficulty_value);
        let speed_difficult_strain_count =
            speed_eval.count_top_weighted_object_difficulties(speed_difficulty_value);
        let reading_difficult_note_count =
            reading_eval.count_top_weighted_object_difficulties(reading_difficulty_value);

        let aim_no_sliders_top_weighted_slider_count =
            aim_no_sliders_eval.count_top_weighted_sliders(aim_no_sliders_difficulty_value);
        let aim_no_sliders_difficult_strain_count =
            aim_no_sliders_eval.count_top_weighted_strains(aim_no_sliders_difficulty_value);
        let aim_top_weighted_slider_factor = aim_no_sliders_top_weighted_slider_count
            / (aim_no_sliders_difficult_strain_count - aim_no_sliders_top_weighted_slider_count)
                .max(1.0);

        let speed_top_weighted_slider_count =
            speed_eval.count_top_weighted_sliders(speed_difficulty_value);
        let speed_top_weighted_slider_factor = speed_top_weighted_slider_count
            / (speed_difficult_strain_count - speed_top_weighted_slider_count).max(1.0);

        let slider_factor = if aim_difficulty_value > 0.0 {
            calculate_aim_difficulty_rating(aim_no_sliders_difficulty_value)
                / calculate_aim_difficulty_rating(aim_difficulty_value)
        } else {
            1.0
        };

        let aim_rating = calculate_aim_difficulty_rating(aim_difficulty_value);
        let speed_rating = calculate_difficulty_rating(speed_difficulty_value);
        let reading_rating = calculate_difficulty_rating(reading_difficulty_value);
        let flashlight_rating = if mods.fl() {
            calculate_difficulty_rating(flashlight.cloned_difficulty_value())
        } else {
            0.0
        };

        let base_aim_performance = difficulty_to_performance(aim_rating);
        let base_speed_performance = difficulty_to_performance(speed_rating);
        let base_reading_performance = difficulty_to_performance(reading_rating);
        let base_flashlight_performance = Flashlight::difficulty_to_performance(flashlight_rating);
        let base_cognition_performance =
            sum_cognition_difficulty(base_reading_performance, base_flashlight_performance);
        let base_performance = crate::util::difficulty::norm(
            1.1,
            [
                base_aim_performance,
                base_speed_performance,
                base_cognition_performance,
            ],
        );

        attrs.aim = aim_rating;
        attrs.aim_difficult_slider_count = aim.get_difficult_sliders();
        attrs.speed = speed_rating;
        attrs.reading = reading_rating;
        attrs.reading_difficult_note_count = reading_difficult_note_count;
        attrs.flashlight = flashlight_rating;
        attrs.slider_factor = slider_factor;
        attrs.aim_top_weighted_slider_factor = aim_top_weighted_slider_factor;
        attrs.speed_top_weighted_slider_factor = speed_top_weighted_slider_factor;
        attrs.speed_note_count = speed.relevant_object_count();
        attrs.aim_difficult_strain_count = aim_difficult_strain_count;
        attrs.speed_difficult_strain_count = speed_difficult_strain_count;
        attrs.stars = calculate_star_rating(base_performance);
    }

    pub fn create_difficulty_objects<'a>(
        difficulty: &Difficulty,
        scaling_factor: &ScalingFactor,
        osu_objects: impl ExactSizeIterator<Item = Pin<&'a mut OsuObject>>,
    ) -> Vec<OsuDifficultyObject<'a>> {
        let take = difficulty.get_passed_objects();
        let clock_rate = difficulty.get_clock_rate();

        let mut osu_objects_iter = osu_objects.map(Pin::into_ref);

        let Some(mut last) = osu_objects_iter.next().filter(|_| take > 0) else {
            return Vec::new();
        };

        let mut diff_objects = Vec::with_capacity(osu_objects_iter.len());

        for (idx, h) in osu_objects_iter.enumerate() {
            let last_diff = if idx > 0 {
                diff_objects.get(idx - 1)
            } else {
                None
            };

            let last_last_diff = if idx > 1 {
                diff_objects.get(idx - 2)
            } else {
                None
            };

            let diff_object = OsuDifficultyObject::new(
                h.get_ref(),
                last.get_ref(),
                last_diff,
                last_last_diff,
                clock_rate,
                idx,
                scaling_factor,
            );

            last = h;

            diff_objects.push(diff_object);
        }

        diff_objects
    }
}

fn difficulty_range_int_preempt(ar: f32) -> f64 {
    BeatmapAttributesExt::difficulty_range(f64::from(ar), 1800.0, 1200.0, 450.0) as i32 as f64
}

#[cfg(test)]
mod source_semantics_tests {
    use super::difficulty_range_int_preempt;

    #[test]
    fn time_preempt_uses_difficulty_range_int_before_stacking() {
        assert_eq!(difficulty_range_int_preempt(0.0), 1800.0);
        assert_eq!(difficulty_range_int_preempt(5.0), 1200.0);
        assert_eq!(difficulty_range_int_preempt(7.5), 825.0);
        assert_eq!(difficulty_range_int_preempt(10.0), 450.0);
    }
}

fn calculate_aim_difficulty_rating(difficulty: f64) -> f64 {
    difficulty.powf(0.63) * 0.02275
}

fn calculate_difficulty_rating(difficulty: f64) -> f64 {
    difficulty.sqrt() * 0.0675
}

fn difficulty_to_performance(difficulty: f64) -> f64 {
    4.0 * difficulty.powi(3)
}

fn sum_cognition_difficulty(reading: f64, flashlight: f64) -> f64 {
    if reading <= 0.0 {
        flashlight
    } else if flashlight <= 0.0 {
        reading
    } else {
        crate::util::difficulty::norm(
            1.1,
            [
                reading,
                flashlight * (flashlight / reading).clamp(0.25, 1.0),
            ],
        )
    }
}

fn calculate_star_rating(base_performance: f64) -> f64 {
    (base_performance * PERFORMANCE_BASE_MULTIPLIER).cbrt()
}

#[cfg(test)]
mod tests {
    use crate::{Beatmap, Difficulty, osu::Osu};

    #[test]
    fn speed_slider_weight_uses_evaluated_skill_state() {
        let map = Beatmap::from_bytes(include_bytes!("../../../../../tests/fixture.osu")).unwrap();
        let attrs = Difficulty::new().calculate_for_mode::<Osu>(&map).unwrap();

        assert!(attrs.speed_top_weighted_slider_factor > 0.0);
    }
}

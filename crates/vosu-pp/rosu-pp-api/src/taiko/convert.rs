use std::{borrow::Cow, cmp};

use rosu_map::{
    section::{
        general::GameMode, hit_objects::hit_samples::HitSoundType, timing_points::ControlPoint,
    },
    util::Pos,
};

#[cfg(feature = "oracle-debug")]
use crate::GameMods;
use crate::{
    Difficulty,
    model::{
        beatmap::Beatmap,
        control_point::{DifficultyPoint, EffectPoint, TimingPoint},
        hit_object::{HitObject, HitObjectKind, HoldNote, Slider, Spinner},
        mode::ConvertError,
    },
    util::{
        float_ext::FloatExt, get_precision_adjusted_beat_len,
        random::csharp::Random as CsharpRandom, sort::TandemSorter,
    },
};

const VELOCITY_MULTIPLIER: f32 = 1.4;
const OSU_BASE_SCORING_DIST: f32 = 100.0;

pub fn prepare_map<'map>(
    difficulty: &Difficulty,
    map: &'map Beatmap,
) -> Result<Cow<'map, Beatmap>, ConvertError> {
    let mut map = map.convert_ref(GameMode::Taiko, difficulty.get_mods())?;

    if let Some(seed) = difficulty.get_mods().random_seed() {
        apply_random_to_beatmap(map.to_mut(), seed);
    }

    Ok(map)
}

#[cfg(feature = "oracle-debug")]
pub fn conversion_debug(
    map: &Beatmap,
    mods: &GameMods,
) -> Result<Vec<super::ConversionMapping>, String> {
    if map.mode != GameMode::Osu && map.mode != GameMode::Taiko {
        return Err(format!("cannot convert {:?} to taiko", map.mode));
    }

    let mut mappings = Vec::with_capacity(map.hit_objects.len());

    for (object, &base_sound) in map.hit_objects.iter().zip(&map.hit_sounds) {
        let mut objects = Vec::new();

        match &object.kind {
            HitObjectKind::Circle => {
                objects.push(debug_circle(object.start_time, base_sound));
            }
            HitObjectKind::Slider(slider) if map.mode == GameMode::Osu => {
                let slider_velocity = map
                    .difficulty_point_at(object.start_time)
                    .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
                        point.slider_velocity
                    });
                let mut params = SliderParams::new(object.start_time, slider, slider_velocity);

                if should_convert_slider_to_taiko_hits(map, &mut params) {
                    let edge_sound_count = cmp::max(slider.node_sounds.len(), 1);
                    let mut i = 0;
                    let mut time = object.start_time;

                    while time
                        <= object.start_time
                            + f64::from(params.duration)
                            + params.tick_spacing / 8.0
                    {
                        let sound = slider.node_sounds.get(i).copied().unwrap_or(base_sound);
                        objects.push(debug_circle(f64::from(time as i32), sound));

                        if params.tick_spacing.eq(0.0) {
                            break;
                        }

                        time += params.tick_spacing;
                        i = (i + 1) % edge_sound_count;
                    }
                } else {
                    objects.push(super::ConversionObject {
                        start_time: object.start_time,
                        end_time: object.start_time + f64::from(params.duration),
                        is_rim: false,
                        is_centre: false,
                        is_drum_roll: true,
                        is_swell: false,
                        is_strong: false,
                    });
                }
            }
            HitObjectKind::Slider(slider) => {
                let slider_velocity = map
                    .difficulty_point_at(object.start_time)
                    .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
                        point.slider_velocity
                    });
                let mut params = SliderParams::new(object.start_time, slider, slider_velocity);
                let _ = should_convert_slider_to_taiko_hits(map, &mut params);
                objects.push(super::ConversionObject {
                    start_time: object.start_time,
                    end_time: object.start_time + f64::from(params.duration),
                    is_rim: false,
                    is_centre: false,
                    is_drum_roll: true,
                    is_swell: false,
                    is_strong: false,
                });
            }
            HitObjectKind::Spinner(Spinner { duration })
            | HitObjectKind::Hold(HoldNote { duration }) => {
                objects.push(super::ConversionObject {
                    start_time: object.start_time,
                    end_time: object.start_time + *duration,
                    is_rim: false,
                    is_centre: false,
                    is_drum_roll: false,
                    is_swell: true,
                    is_strong: false,
                });
            }
        }

        mappings.push(super::ConversionMapping {
            start_time: object.start_time,
            objects,
        });
    }

    if let Some(seed) = mods.random_seed() {
        let mut rng = CsharpRandom::new(seed);

        for object in mappings.iter_mut().flat_map(|mapping| &mut mapping.objects) {
            if !object.is_centre && !object.is_rim {
                continue;
            }

            object.is_rim = rng.next_max(2) != 0;
            object.is_centre = !object.is_rim;
        }
    }

    Ok(mappings)
}

#[cfg(feature = "oracle-debug")]
fn debug_circle(start_time: f64, sound: HitSoundType) -> super::ConversionObject {
    let is_rim = sound.has_flag(HitSoundType::CLAP) || sound.has_flag(HitSoundType::WHISTLE);

    super::ConversionObject {
        start_time,
        end_time: start_time,
        is_rim,
        is_centre: !is_rim,
        is_drum_roll: false,
        is_swell: false,
        is_strong: sound.has_flag(HitSoundType::FINISH),
    }
}

pub fn convert(map: &mut Beatmap) {
    let mut new_objects = Vec::new();
    let mut new_sounds = Vec::new();

    let mut idx = 0;

    let mut last_scroll_speed = 1.0;

    while idx < map.hit_objects.len() {
        match map.hit_objects[idx].kind {
            HitObjectKind::Circle | HitObjectKind::Spinner(_) => {}
            HitObjectKind::Slider(ref slider) => {
                let obj = &map.hit_objects[idx];

                let slider_velocity = map
                    .difficulty_point_at(obj.start_time)
                    .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
                        point.slider_velocity
                    });

                if !FloatExt::almost_eq(last_scroll_speed, slider_velocity, f64::EPSILON) {
                    let curr_kiai = map
                        .effect_point_at(obj.start_time)
                        .map_or(EffectPoint::DEFAULT_KIAI, |point| point.kiai);

                    let effect_point = EffectPoint {
                        scroll_speed: slider_velocity,
                        ..EffectPoint::new(obj.start_time, curr_kiai)
                    };

                    last_scroll_speed = slider_velocity;

                    effect_point.add(&mut map.effect_points);
                }

                let mut params = SliderParams::new(obj.start_time, slider, slider_velocity);

                if should_convert_slider_to_taiko_hits(map, &mut params) {
                    let mut i = 0;
                    let mut j = obj.start_time;

                    let edge_sound_count = cmp::max(slider.node_sounds.len(), 1);

                    while j
                        <= obj.start_time + f64::from(params.duration) + params.tick_spacing / 8.0
                    {
                        let h = HitObject {
                            pos: Pos::default(),
                            start_time: j,
                            new_combo: false,
                            index_in_current_combo: 0,
                            kind: HitObjectKind::Circle,
                        };

                        let sound = slider
                            .node_sounds
                            .get(i)
                            .copied()
                            .unwrap_or(map.hit_sounds[idx]);

                        new_objects.push(h);
                        new_sounds.push(sound);

                        if params.tick_spacing.eq(0.0) {
                            break;
                        }

                        j += params.tick_spacing;
                        i = (i + 1) % edge_sound_count;
                    }

                    if let Some(len) = new_objects.len().checked_sub(1) {
                        map.hit_objects.splice(idx..=idx, new_objects.drain(..));
                        map.hit_sounds.splice(idx..=idx, new_sounds.drain(..));
                        idx += len;
                    } else {
                        map.hit_objects.remove(idx);
                        map.hit_sounds.remove(idx);
                        idx -= 1;
                    }
                }
            }
            HitObjectKind::Hold(HoldNote { duration }) => {
                map.hit_objects[idx].kind = HitObjectKind::Spinner(Spinner { duration });
            }
        }

        idx += 1;
    }

    // We only convert osu! to taiko so we don't need to remove objects
    // with the same timestamp that would appear only in mania

    let mut sorter = TandemSorter::new_stable(&map.hit_objects, |a, b| {
        a.start_time.total_cmp(&b.start_time)
    });
    sorter.sort(&mut map.hit_objects);
    sorter.sort(&mut map.hit_sounds);

    map.mode = GameMode::Taiko;
    map.is_convert = true;
}

fn should_convert_slider_to_taiko_hits(map: &Beatmap, params: &mut SliderParams<'_>) -> bool {
    let SliderParams {
        slider,
        duration,
        start_time,
        tick_spacing,
        slider_velocity,
    } = params;

    // * The true distance, accounting for any repeats. This ends up being the drum roll distance later
    let spans = slider.span_count() as f64;
    let mut dist = slider.expected_dist.unwrap_or(0.0);

    // * Do not combine the following two lines!
    dist *= f64::from(VELOCITY_MULTIPLIER);
    dist *= spans;

    let timing_beat_len = map
        .timing_point_at(*start_time)
        .map_or(TimingPoint::DEFAULT_BEAT_LEN, |point| point.beat_len);

    let mut beat_len = get_precision_adjusted_beat_len(*slider_velocity, timing_beat_len);

    let slider_scoring_point_dist = f64::from(OSU_BASE_SCORING_DIST)
        * (map.slider_multiplier * f64::from(VELOCITY_MULTIPLIER))
        / map.slider_tick_rate;

    // * The velocity and duration of the taiko hit object - calculated as the velocity of a drum roll.
    let taiko_vel = slider_scoring_point_dist * map.slider_tick_rate;
    *duration = (dist / taiko_vel * beat_len) as u32;

    let osu_vel = taiko_vel * (f64::from(1000.0_f32) / beat_len);

    // * osu-stable always uses the speed-adjusted beatlength to determine the osu! velocity, but only uses it for conversion if beatmap version < 8
    if map.version >= 8 {
        beat_len = timing_beat_len;
    }

    // * If the drum roll is to be split into hit circles, assume the ticks are 1/8 spaced within the duration of one beat
    *tick_spacing = (beat_len / map.slider_tick_rate).min(f64::from(*duration) / spans);

    *tick_spacing > 0.0 && dist / osu_vel * 1000.0 < 2.0 * beat_len
}

struct SliderParams<'c> {
    slider: &'c Slider,
    duration: u32,
    start_time: f64,
    tick_spacing: f64,
    slider_velocity: f64,
}

impl<'c> SliderParams<'c> {
    const fn new(start_time: f64, slider: &'c Slider, slider_velocity: f64) -> Self {
        Self {
            slider,
            start_time,
            duration: 0,
            tick_spacing: 0.0,
            slider_velocity,
        }
    }
}

fn apply_random_to_beatmap(map: &mut Beatmap, seed: i32) {
    let mut rng = CsharpRandom::new(seed);

    for (h, s) in map.hit_objects.iter().zip(map.hit_sounds.iter_mut()) {
        if !h.is_circle() {
            continue;
        }

        if rng.next_max(2) == 0 {
            // Center
            *s &= !(HitSoundType::CLAP | HitSoundType::WHISTLE);
        } else {
            // Rim
            *s = HitSoundType::from(u8::from(*s) | HitSoundType::CLAP);
        }
    }
}

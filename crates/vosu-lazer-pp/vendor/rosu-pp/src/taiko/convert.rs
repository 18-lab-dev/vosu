use std::{
    borrow::Cow,
    cmp,
    sync::atomic::{AtomicU32, Ordering as AtomicOrdering},
    time::{SystemTime, UNIX_EPOCH},
};

use rosu_map::{
    section::{
        general::GameMode, hit_objects::hit_samples::HitSoundType, timing_points::ControlPoint,
    },
    util::Pos,
};

use crate::{
    Difficulty, GameMods,
    model::{
        beatmap::Beatmap,
        control_point::{DifficultyPoint, EffectPoint, TimingPoint},
        hit_object::{HitObject, HitObjectKind, HoldNote, Slider, Spinner},
        mode::ConvertError,
        mods::rosu_mods::{GameMod, GameModIntermode},
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
    apply_difficulty_mods(map.to_mut(), difficulty.get_mods());

    Ok(map)
}

#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct TaikoConversionMapping {
    pub start_time: f64,
    pub objects: Vec<TaikoConvertedObject>,
}

#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct TaikoConvertedObject {
    pub start_time: f64,
    pub end_time: f64,
    pub is_rim: bool,
    pub is_centre: bool,
    pub is_drum_roll: bool,
    pub is_swell: bool,
    pub is_strong: bool,
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn conversion_debug(
    map: &Beatmap,
    mods: &GameMods,
) -> Result<Vec<TaikoConversionMapping>, ConvertError> {
    let difficulty = Difficulty::new().mods(mods.clone());
    let converted = prepare_map(&difficulty, map)?;
    let converted = converted.as_ref();
    let mut mappings: Vec<_> = map
        .hit_objects
        .iter()
        .map(|object| TaikoConversionMapping {
            start_time: object.start_time,
            objects: Vec::new(),
        })
        .collect();

    if mappings.is_empty() {
        return Ok(mappings);
    }

    for (object, sound) in converted.hit_objects.iter().zip(&converted.hit_sounds) {
        let mapping_idx = mappings
            .partition_point(|mapping| mapping.start_time <= object.start_time)
            .saturating_sub(1);
        let (end_time, is_drum_roll, is_swell) = match &object.kind {
            HitObjectKind::Circle => (object.start_time, false, false),
            HitObjectKind::Slider(slider) => {
                let slider_velocity = converted
                    .difficulty_point_at(object.start_time)
                    .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
                        point.slider_velocity
                    });
                let mut params = SliderParams::new(object.start_time, slider, slider_velocity);
                should_convert_slider_to_taiko_hits(converted, &mut params);

                (object.start_time + f64::from(params.duration), true, false)
            }
            HitObjectKind::Spinner(Spinner { duration })
            | HitObjectKind::Hold(HoldNote { duration }) => {
                (object.start_time + *duration, false, true)
            }
        };
        let is_circle = object.is_circle();
        let is_rim = is_circle && sound.has_flag(HitSoundType::CLAP | HitSoundType::WHISTLE);

        mappings[mapping_idx].objects.push(TaikoConvertedObject {
            start_time: object.start_time,
            end_time,
            is_rim,
            is_centre: is_circle && !is_rim,
            is_drum_roll,
            is_swell,
            is_strong: sound.has_flag(HitSoundType::FINISH),
        });
    }

    Ok(mappings)
}

fn apply_difficulty_mods(map: &mut Beatmap, mods: &GameMods) {
    match mods {
        GameMods::Lazer(mods) => {
            for game_mod in mods.iter() {
                apply_playable_mod(map, game_mod);
            }
        }
        GameMods::Intermode(mods) => {
            if mods.contains(GameModIntermode::SimplifiedRhythm) {
                apply_simplified_rhythm(map, (false, true, false));
            }

            if mods.contains(GameModIntermode::Swap) {
                apply_swap(map);
            }
        }
        GameMods::Legacy(_) => {}
    }
}

#[doc(hidden)]
pub fn apply_playable_mod(map: &mut Beatmap, game_mod: &GameMod) -> Option<f64> {
    match game_mod {
        GameMod::RandomTaiko(random) => Some(apply_random_mod(map, random.seed)),
        GameMod::SimplifiedRhythmTaiko(settings) => {
            apply_simplified_rhythm(
                map,
                (
                    settings.one_third_conversion.unwrap_or(false),
                    settings.one_sixth_conversion.unwrap_or(true),
                    settings.one_eighth_conversion.unwrap_or(false),
                ),
            );

            None
        }
        GameMod::SwapTaiko(_) => {
            apply_swap(map);

            None
        }
        _ => None,
    }
}

fn apply_swap(map: &mut Beatmap) {
    for (object, sound) in map.hit_objects.iter().zip(map.hit_sounds.iter_mut()) {
        if !object.is_circle() {
            continue;
        }

        let raw = u8::from(*sound);
        let rim_flags = u8::from(HitSoundType::CLAP | HitSoundType::WHISTLE);

        *sound = if raw & rim_flags == 0 {
            HitSoundType::from(raw | u8::from(HitSoundType::WHISTLE))
        } else {
            HitSoundType::from(raw & !rim_flags)
        };
    }
}

fn apply_simplified_rhythm(map: &mut Beatmap, settings: (bool, bool, bool)) {
    let hit_indices = map
        .hit_objects
        .iter()
        .enumerate()
        .filter_map(|(idx, object)| object.is_circle().then_some(idx))
        .collect::<Vec<_>>();

    if hit_indices.is_empty() {
        return;
    }

    let mut removed = vec![false; map.hit_objects.len()];
    let conversions = [
        settings.2.then_some((8, 4)),
        settings.1.then_some((6, 4)),
        settings.0.then_some((3, 2)),
    ];
    let mut in_pattern = false;

    for (base_rhythm, adjusted_rhythm) in conversions.into_iter().flatten() {
        let mut pattern_start_idx = 0;

        for i in 1..hit_indices.len() {
            let curr_idx = hit_indices[i - 1];
            let next_idx = hit_indices[i];
            let snap = closest_beat_divisor(
                map,
                map.hit_objects[curr_idx].start_time,
                map.hit_objects[next_idx].start_time,
            );

            if in_pattern {
                if snap == base_rhythm {
                    continue;
                }

                in_pattern = false;
                process_simplified_pattern(
                    map,
                    &hit_indices,
                    &mut removed,
                    pattern_start_idx,
                    i,
                    base_rhythm,
                    adjusted_rhythm,
                );
            } else if snap == base_rhythm {
                pattern_start_idx = i - 1;
                in_pattern = true;
            }
        }

        if in_pattern {
            process_simplified_pattern(
                map,
                &hit_indices,
                &mut removed,
                pattern_start_idx,
                hit_indices.len(),
                base_rhythm,
                adjusted_rhythm,
            );
        }
    }

    let mut idx = 0;
    map.hit_objects.retain(|_| {
        let retain = !removed[idx];
        idx += 1;

        retain
    });
    idx = 0;
    map.hit_sounds.retain(|_| {
        let retain = !removed[idx];
        idx += 1;

        retain
    });

    let mut sorter = TandemSorter::new_stable(&map.hit_objects, |a, b| {
        a.start_time.total_cmp(&b.start_time)
    });
    sorter.sort(&mut map.hit_objects);
    sorter.sort(&mut map.hit_sounds);
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the source pattern operation"
)]
fn process_simplified_pattern(
    map: &mut Beatmap,
    hit_indices: &[usize],
    removed: &mut [bool],
    pattern_start_idx: usize,
    pattern_end_idx: usize,
    base_rhythm: i32,
    adjusted_rhythm: i32,
) {
    for j in pattern_start_idx..pattern_end_idx {
        let index_in_pattern = j - pattern_start_idx;
        let object_idx = hit_indices[j];

        match base_rhythm {
            8 if index_in_pattern % 2 == 1 => removed[object_idx] = true,
            3 | 6 if index_in_pattern % 3 == 1 => removed[object_idx] = true,
            3 | 6 if index_in_pattern % 3 == 2 => {
                let previous_idx = hit_indices[j - 2];
                let beat_len = map
                    .timing_point_at(map.hit_objects[object_idx].start_time)
                    .map_or(TimingPoint::DEFAULT_BEAT_LEN, |point| point.beat_len);

                map.hit_objects[object_idx].start_time = map.hit_objects[previous_idx].start_time
                    + beat_len / f64::from(adjusted_rhythm);
            }
            _ => {}
        }
    }
}

fn closest_beat_divisor(map: &Beatmap, current_time: f64, next_time: f64) -> i32 {
    const DIVISORS: [i32; 11] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 12, 16];
    const DEFINITELY_BIGGER_EPSILON: f64 = 1e-7;

    let current_timing_point = map
        .timing_point_at(current_time)
        .copied()
        .unwrap_or_default();
    let mut time = current_timing_point.time + (next_time - current_time);
    let timing_point = map.timing_point_at(time).copied().unwrap_or_default();

    if time < 0.0 {
        let offset_beats = (-time / timing_point.beat_len).ceil();
        time += offset_beats * timing_point.beat_len;
    }

    let mut closest_divisor = 0;
    let mut closest_distance = f64::MAX;

    for divisor in DIVISORS {
        let beat_len = timing_point.beat_len / f64::from(divisor);
        let beats = ((time.max(0.0) - timing_point.time) / beat_len).round();
        let mut snapped_time = timing_point.time + beats * beat_len;

        if snapped_time < 0.0 {
            snapped_time += beat_len;
        }

        let distance = (time - snapped_time).abs();

        if closest_distance - distance > DEFINITELY_BIGGER_EPSILON {
            closest_divisor = divisor;
            closest_distance = distance;
        }
    }

    closest_divisor
}

pub fn convert(map: &mut Beatmap) {
    let source_mode = map.mode;
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

                if source_mode == GameMode::Osu
                    && !FloatExt::almost_eq(last_scroll_speed, slider_velocity, f64::EPSILON)
                {
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
                            start_time: j.trunc(),
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

    let mut sorter = TandemSorter::new_stable(&map.hit_objects, |a, b| {
        a.start_time.total_cmp(&b.start_time)
    });
    sorter.sort(&mut map.hit_objects);
    sorter.sort(&mut map.hit_sounds);

    if source_mode == GameMode::Mania {
        let mut grouped_objects = Vec::with_capacity(map.hit_objects.len());
        let mut grouped_sounds = Vec::with_capacity(map.hit_sounds.len());
        let mut objects = map
            .hit_objects
            .drain(..)
            .zip(map.hit_sounds.drain(..))
            .peekable();

        while let Some((object, mut sound)) = objects.next() {
            let mut is_strong = false;

            while objects
                .peek()
                .is_some_and(|(next, _)| next.start_time == object.start_time)
            {
                objects.next();
                is_strong = true;
            }

            if is_strong {
                sound = HitSoundType::from(u8::from(sound) | HitSoundType::FINISH);
            }

            grouped_objects.push(object);
            grouped_sounds.push(sound);
        }
        drop(objects);

        map.hit_objects = grouped_objects;
        map.hit_sounds = grouped_sounds;
    }

    map.mode = GameMode::Taiko;
    map.is_convert = source_mode == GameMode::Osu;
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

fn random_seed() -> u32 {
    static NEXT_SEED: AtomicU32 = AtomicU32::new(0);

    let time_seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos());

    (time_seed ^ NEXT_SEED.fetch_add(1, AtomicOrdering::Relaxed)) % i32::MAX as u32
}

#[doc(hidden)]
pub fn apply_random_mod(map: &mut Beatmap, seed: Option<f64>) -> f64 {
    apply_random_mod_with(map, seed, random_seed)
}

fn apply_random_mod_with(
    map: &mut Beatmap,
    seed: Option<f64>,
    seed_source: impl FnOnce() -> u32,
) -> f64 {
    let seed = seed.unwrap_or_else(|| f64::from(seed_source()));
    apply_random(map, seed as i32);

    seed
}

#[doc(hidden)]
pub fn apply_random(map: &mut Beatmap, seed: i32) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(start_time: f64) -> HitObject {
        HitObject {
            pos: Pos::default(),
            start_time,
            kind: HitObjectKind::Circle,
        }
    }

    #[test]
    fn swap_flips_center_and_rim_hits_only() {
        let mut map = Beatmap {
            mode: GameMode::Taiko,
            hit_objects: vec![circle(0.0), circle(100.0)],
            hit_sounds: vec![HitSoundType::default(), HitSoundType::WHISTLE.into()],
            ..Default::default()
        };

        apply_playable_mod(&mut map, &GameMod::SwapTaiko(Default::default()));

        assert!(map.hit_sounds[0].has_flag(HitSoundType::WHISTLE));
        assert!(!map.hit_sounds[1].has_flag(HitSoundType::CLAP | HitSoundType::WHISTLE));
    }

    #[test]
    fn simplified_rhythm_removes_and_retimes_one_sixth_patterns() {
        let sixth = 500.0 / 6.0;
        let mut map = Beatmap {
            mode: GameMode::Taiko,
            timing_points: vec![TimingPoint::new(0.0, 500.0)],
            hit_objects: vec![
                circle(0.0),
                circle(sixth),
                circle(2.0 * sixth),
                circle(3.0 * sixth),
            ],
            hit_sounds: vec![HitSoundType::default(); 4],
            ..Default::default()
        };

        apply_playable_mod(
            &mut map,
            &GameMod::SimplifiedRhythmTaiko(Default::default()),
        );

        assert_eq!(map.hit_objects.len(), 3);
        assert_eq!(
            map.hit_objects
                .iter()
                .map(|object| object.start_time)
                .collect::<Vec<_>>(),
            [0.0, 125.0, 250.0],
        );
    }

    #[test]
    fn random_uses_the_fixed_csharp_seed_for_every_hit() {
        const EXPECTED_RIM: [bool; 16] = [
            false, false, false, true, true, true, true, false, false, false, false, false, true,
            false, true, false,
        ];
        let mut map = Beatmap {
            mode: GameMode::Taiko,
            hit_objects: (0..EXPECTED_RIM.len())
                .map(|idx| circle(idx as f64 * 100.0))
                .collect(),
            hit_sounds: vec![HitSoundType::FINISH.into(); EXPECTED_RIM.len()],
            ..Default::default()
        };

        apply_random(&mut map, 1337);

        let actual_rim: Vec<_> = map
            .hit_sounds
            .iter()
            .map(|sound| sound.has_flag(HitSoundType::CLAP | HitSoundType::WHISTLE))
            .collect();

        assert_eq!(actual_rim, EXPECTED_RIM);
        assert!(
            map.hit_sounds
                .iter()
                .all(|sound| sound.has_flag(HitSoundType::FINISH))
        );

        let once = map.hit_sounds.clone();
        apply_random(&mut map, 1337);
        assert_eq!(map.hit_sounds, once);
    }

    #[test]
    fn random_without_a_seed_materializes_and_applies_the_generated_seed_once() {
        let mut generated = Beatmap {
            mode: GameMode::Taiko,
            hit_objects: (0..16).map(|idx| circle(idx as f64 * 100.0)).collect(),
            hit_sounds: vec![HitSoundType::default(); 16],
            ..Default::default()
        };
        let mut explicit = generated.clone();

        let chosen_seed = apply_random_mod_with(&mut generated, None, || 1337);
        apply_random(&mut explicit, chosen_seed as i32);

        assert_eq!(chosen_seed, 1337.0);
        assert_eq!(generated.hit_sounds, explicit.hit_sounds);
        assert!(
            generated
                .hit_sounds
                .iter()
                .any(|sound| sound.has_flag(HitSoundType::CLAP | HitSoundType::WHISTLE))
        );
        assert!(
            generated
                .hit_sounds
                .iter()
                .any(|sound| !sound.has_flag(HitSoundType::CLAP | HitSoundType::WHISTLE))
        );
    }

    #[test]
    fn mania_conversion_does_not_inherit_osu_sv_or_convert_flag() {
        const MAP: &[u8] = br#"osu file format v14

[General]
Mode:0

[Difficulty]
SliderMultiplier:1.4
SliderTickRate:1

[TimingPoints]
0,500,4,2,0,100,1,0
0,-50,4,2,0,100,0,0

[HitObjects]
256,192,1000,2,0,B|356:192,1,100
"#;

        let standard = Beatmap::from_bytes(MAP).unwrap();
        let mut mania = standard.clone();
        mania.mode = GameMode::Mania;

        convert(&mut mania);

        assert_eq!(mania.mode, GameMode::Taiko);
        assert!(!mania.is_convert);
        assert!(mania.effect_points.is_empty());

        let mut standard = standard;
        convert(&mut standard);

        assert!(standard.is_convert);
        assert_eq!(standard.effect_points.len(), 1);
        assert_eq!(standard.effect_points[0].scroll_speed, 2.0);
    }
}

// Ported from osu! at revision 97e94babde9d5ecbe574ecdd33509db27452cd0f.
// The reduced beatmap model preserves every transformation input that changes
// difficulty or score-pool projection: geometry, timing, combo, and hit sounds.

use std::f32::consts::{PI, TAU};

use rosu_map::{
    section::{general::GameMode, hit_objects::CurveBuffers},
    util::Pos,
};

use crate::{
    Beatmap,
    model::hit_object::{HitObject, HitObjectKind, Slider},
    osu::{PLAYFIELD_BASE_SIZE, difficulty::scaling_factor::ScalingFactor},
};

const TIMING_PRECISION: f64 = 1.0;
const MAX_BASE_DISTANCE: f32 = 333.0;
const DISTANCE_CAP: f32 = 380.0;
const EDGE_ROTATION_MULTIPLIER: f32 = 0.75;
const PLAYFIELD_DIAGONAL: f32 = 640.0;
const PRECEDING_OBJECTS_TO_SHIFT: usize = 10;

pub fn seed_to_i32(seed: f64) -> i32 {
    if seed.is_nan() {
        0
    } else if seed >= f64::from(i32::MAX) {
        i32::MAX
    } else if seed <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        seed as i32
    }
}

pub fn apply_target_practice_difficulty(map: &mut Beatmap) {
    map.ar *= 0.5;
}

pub fn apply_target_practice(map: &mut Beatmap, seed: i32) {
    if map.hit_objects.is_empty() {
        return;
    }

    let original_objects = map.hit_objects.clone();
    let original_sounds = map.hit_sounds.clone();
    let original_combos = map.hit_object_combo_info.clone();
    let start_time = original_objects[0].start_time;
    let end_time = playable_end_time(map, original_objects.last().expect("non-empty"));
    let mut beats = Vec::new();

    for (idx, point) in map.timing_points.iter().enumerate() {
        if definitely_bigger(point.time, end_time) {
            break;
        }

        let next_time = map
            .timing_points
            .get(idx + 1)
            .map_or(f64::INFINITY, |next| next.time);
        let mut i = 0_u64;

        loop {
            let current = point.time + i as f64 * point.beat_len;

            if definitely_bigger(current, end_time) || current >= next_time {
                break;
            }

            let beat = current.floor();

            if almost_bigger(beat, start_time)
                && !inside_break(&original_objects, &map.breaks, beat)
            {
                beats.push(beat);
            }

            i += 1;
        }
    }

    for idx in (0..beats.len().saturating_sub(1)).rev() {
        let beat_len = map
            .timing_point_at(beats[idx])
            .map_or(1000.0, |point| point.beat_len);

        if !definitely_bigger(beats[idx + 1] - beats[idx], beat_len / 2.0) {
            beats.remove(idx);
        }
    }

    let radius = ScalingFactor::new(map.cs).radius as f32;
    let mut sounds = Vec::with_capacity(beats.len());
    let mut objects = Vec::with_capacity(beats.len());

    let mut combos = Vec::with_capacity(beats.len());
    let mut previous_source_combo = None;
    let mut generated_combo = 0_u32;
    let mut index_in_combo = 0_u32;

    for beat in beats {
        let source_idx = original_objects
            .partition_point(|object| almost_bigger(beat, object.start_time))
            .saturating_sub(1);
        let source_combo = original_combos
            .get(source_idx)
            .map_or(0, |info| info.combo_index);

        if previous_source_combo.is_some_and(|previous| previous != source_combo) {
            generated_combo += 1;
            index_in_combo = 0;
        }

        combos.push(crate::model::beatmap::HitObjectComboInfo {
            combo_index: generated_combo,
            index_in_combo,
            starts_new_combo: previous_source_combo
                .is_some_and(|previous| previous != source_combo),
        });
        previous_source_combo = Some(source_combo);
        index_in_combo += 1;
        sounds.push(sound_at_time(
            map,
            &original_objects,
            &original_sounds,
            beat,
        ));
        objects.push(HitObject {
            pos: Pos::new(256.0, 192.0),
            start_time: beat,
            kind: HitObjectKind::Circle,
        });
    }

    randomize_target_positions(map, &mut objects, &combos, radius, seed);
    map.hit_object_combo_info = combos;
    map.hit_objects = objects;
    map.hit_sounds = sounds;
}

fn playable_end_time(map: &Beatmap, object: &HitObject) -> f64 {
    let mut curve_bufs = CurveBuffers::default();
    let mut ticks_buf = Vec::new();
    crate::osu::object::OsuObject::new(
        object,
        map,
        crate::model::mods::Reflection::None,
        &mut curve_bufs,
        &mut ticks_buf,
    )
    .end_time()
}

fn sound_at_time(
    map: &Beatmap,
    objects: &[HitObject],
    sounds: &[rosu_map::section::hit_objects::hit_samples::HitSoundType],
    time: f64,
) -> rosu_map::section::hit_objects::hit_samples::HitSoundType {
    for (idx, object) in objects.iter().enumerate() {
        if almost_equal(time, object.start_time) {
            return sounds.get(idx).copied().unwrap_or_default();
        }

        if let HitObjectKind::Slider(slider) = &object.kind {
            let end_time = playable_end_time(map, object);
            let span_duration = (end_time - object.start_time) / slider.span_count() as f64;

            if almost_bigger(time, object.start_time) && almost_bigger(end_time, time) {
                let node_idx = ((time - object.start_time) / span_duration).round() as usize;
                let node_time = object.start_time + node_idx as f64 * span_duration;

                if almost_equal(time, node_time)
                    && let Some(sound) = slider.node_sounds.get(node_idx)
                {
                    return *sound;
                }
            }
        }
    }

    let idx = closest_object_idx(objects, time);
    let mut sound = sounds.get(idx).copied().unwrap_or_default();
    sound &= rosu_map::section::hit_objects::hit_samples::HitSoundType::NORMAL;
    sound
}

fn inside_break(
    objects: &[HitObject],
    breaks: &[crate::model::beatmap::BreakPeriod],
    time: f64,
) -> bool {
    breaks.iter().any(|break_period| {
        let first_after = objects
            .iter()
            .find(|object| almost_bigger(object.start_time, break_period.end_time));

        almost_bigger(time, break_period.start_time)
            && first_after.is_none_or(|object| definitely_bigger(object.start_time, time))
    })
}

fn closest_object_idx(objects: &[HitObject], time: f64) -> usize {
    match objects.partition_point(|object| object.start_time < time) {
        0 => 0,
        idx if idx == objects.len() => idx - 1,
        idx => {
            if objects[idx].start_time - time < time - objects[idx - 1].start_time {
                idx
            } else {
                idx - 1
            }
        }
    }
}

fn randomize_target_positions(
    map: &Beatmap,
    objects: &mut [HitObject],
    combos: &[crate::model::beatmap::HitObjectComboInfo],
    radius: f32,
    seed: i32,
) {
    if objects.is_empty() {
        return;
    }

    let mut random = DotNetRandom::new(seed);
    let mut direction = TAU * random.next_single();
    let max_combo_idx = combos.last().map_or(0, |info| info.combo_index);

    for idx in 0..objects.len() {
        let last_pos = idx
            .checked_sub(1)
            .map_or(Pos::new(256.0, 192.0), |prev| objects[prev].pos);
        let combo = combos.get(idx).copied().unwrap_or_default();
        let combo_idx = combo.combo_index;
        let mut distance = if max_combo_idx == 0 {
            radius
        } else {
            map_range(
                combo_idx as f32,
                0.0,
                max_combo_idx as f32,
                radius,
                MAX_BASE_DISTANCE,
            )
        };
        distance = distance.min(DISTANCE_CAP);
        if combo.index_in_combo == 0 {
            distance *= 1.5;
        }
        if map
            .effect_point_at(objects[idx].start_time)
            .is_some_and(|point| point.kiai)
        {
            distance *= 1.2;
        }
        let preceding_start = idx.saturating_sub(5);
        let mut attempts = 0;

        loop {
            if attempts > 0 {
                direction = TAU * random.next_single();
            }

            let relative = Pos::new(distance * direction.cos(), distance * direction.sin());
            let relative = rotate_away_from_edge(last_pos, relative, EDGE_ROTATION_MULTIPLIER);
            direction = relative.y.atan2(relative.x);
            objects[idx].pos = clamp_circle(last_pos + relative, radius);
            attempts += 1;

            let overlaps = objects[preceding_start..idx]
                .iter()
                .any(|previous| distance_between(previous.pos, objects[idx].pos) < radius * 2.0);

            if distance < radius * 2.0 || !overlaps {
                break;
            }

            if attempts % 10 == 0 {
                distance *= 0.9;
            }
        }

        let last_in_combo = combos
            .get(idx + 1)
            .is_none_or(|next| next.combo_index != combo.combo_index);
        if last_in_combo {
            direction = TAU * random.next_single();
        } else {
            direction += distance / DISTANCE_CAP * (random.next_single() * TAU - PI);
        }
    }
}

pub fn apply_random(map: &mut Beatmap, seed: i32, angle_sharpness: f32) {
    if map.hit_objects.is_empty() {
        return;
    }

    let mut random = DotNetRandom::new(seed);
    let mut infos = generate_position_infos(map);
    let mut section_offset = 0.0;
    let mut flow_direction = false;

    for idx in 0..infos.len() {
        if starts_new_section(map, idx, &mut random) {
            section_offset = random_offset(&mut random, 0.0008, angle_sharpness);
            flow_direction = !flow_direction;
        }

        if matches!(map.hit_objects[idx].kind, HitObjectKind::Slider(_))
            && random.next_double() < 0.5
        {
            flip_slider_horizontally(&mut map.hit_objects[idx]);
            infos[idx].rotation = -infos[idx].rotation;
        }

        if idx == 0 {
            infos[idx].distance = (random.next_double() * 192.0) as f32;
            infos[idx].relative_angle =
                (random.next_double() * f64::from(TAU) - f64::from(PI)) as f32;
        } else {
            let one_time_offset = random_offset(&mut random, 0.002, angle_sharpness);
            let flow_change_offset = if should_apply_flow_change(map, idx, &mut random) {
                flow_direction = !flow_direction;
                random_offset(&mut random, 0.002, angle_sharpness)
            } else {
                0.0
            };
            let total_offset = (section_offset + one_time_offset) * infos[idx].distance
                + flow_change_offset * (PLAYFIELD_DIAGONAL - infos[idx].distance);
            infos[idx].relative_angle = relative_target_angle(
                infos[idx].distance,
                total_offset,
                flow_direction,
                angle_sharpness,
            );
        }
    }

    reposition(map, &infos);
}

#[derive(Copy, Clone)]
struct PositionInfo {
    relative_angle: f32,
    distance: f32,
    rotation: f32,
}

fn generate_position_infos(map: &Beatmap) -> Vec<PositionInfo> {
    let mut infos = Vec::with_capacity(map.hit_objects.len());
    let mut previous_pos = Pos::new(256.0, 192.0);
    let mut previous_angle = 0.0;

    for object in &map.hit_objects {
        let relative = object.pos - previous_pos;
        let absolute_angle = relative.y.atan2(relative.x);
        let mut ending_angle = absolute_angle;
        let mut rotation = 0.0;

        if let HitObjectKind::Slider(slider) = &object.kind {
            let slider_rotation = slider_rotation(slider);
            rotation = slider_rotation - absolute_angle;
            ending_angle = slider_rotation;
        }

        infos.push(PositionInfo {
            relative_angle: absolute_angle - previous_angle,
            distance: length(relative),
            rotation,
        });
        previous_pos = object_end_pos(object);
        previous_angle = ending_angle;
    }

    infos
}

fn starts_new_section(map: &Beatmap, idx: usize, random: &mut DotNetRandom) -> bool {
    if idx == 0 {
        return true;
    }

    let previous = &map.hit_objects[idx - 1];
    let previous_started_combo = previous_object_started_combo(map, idx);

    is_on_beat(map, previous, true)
        || (is_on_beat(map, previous, false) && random.next_double() < 0.4)
        || (previous_started_combo && random.next_double() < 0.6)
}

fn should_apply_flow_change(map: &Beatmap, idx: usize, random: &mut DotNetRandom) -> bool {
    previous_object_started_combo(map, idx) && random.next_double() < 0.6
}

fn previous_object_started_combo(map: &Beatmap, idx: usize) -> bool {
    let combo_info = &map.hit_object_combo_info;

    combo_info
        .get(idx.saturating_sub(2))
        .is_some_and(|info| info.index_in_combo > 1)
        && combo_info
            .get(idx.saturating_sub(1))
            .is_some_and(|info| info.index_in_combo == 0)
}

fn is_on_beat(map: &Beatmap, object: &HitObject, downbeat: bool) -> bool {
    let Some(point) = map.timing_point_at(object.start_time) else {
        return false;
    };
    let beat_len = if downbeat {
        point.beat_len * f64::from(point.time_signature)
    } else {
        point.beat_len
    };
    (object.start_time - point.time + 1.0).rem_euclid(beat_len) < 2.0
}

fn random_offset(random: &mut DotNetRandom, std_dev: f32, angle_sharpness: f32) -> f32 {
    let multiplier = (15.0 - angle_sharpness.clamp(1.0, 10.0)) / 8.0;
    let x1 = 1.0 - random.next_double();
    let x2 = 1.0 - random.next_double();
    let normal = (-2.0 * x1.ln()).sqrt() * (2.0 * std::f64::consts::PI * x2).sin();
    std_dev * multiplier * normal as f32
}

fn relative_target_angle(distance: f32, offset: f32, flow: bool, sharpness: f32) -> f32 {
    let angle_sharpness = sharpness.clamp(1.0, 10.0) / 10.0;
    let custom_x = angle_sharpness * 100.0 - 70.0;
    let custom_y = (1.0 - angle_sharpness) * 0.25 - 0.075;
    let adjusted = distance + custom_x;
    let angle = 2.16 / (1.0 + 200.0 * (0.036 * (adjusted - 310.0 + custom_x)).exp())
        + 0.5
        + offset
        + custom_y;
    let relative = PI - angle;
    if flow { -relative } else { relative }
}

fn reposition(map: &mut Beatmap, infos: &[PositionInfo]) {
    let radius = ScalingFactor::new(map.cs).radius as f32;
    let mut previous_end = Pos::new(256.0, 192.0);
    let mut previous_angle = 0.0;

    for idx in 0..map.hit_objects.len() {
        if matches!(map.hit_objects[idx].kind, HitObjectKind::Spinner(_)) {
            previous_end = map.hit_objects[idx].pos;
            continue;
        }

        let absolute_angle = previous_angle + infos[idx].relative_angle;
        let relative = Pos::new(
            infos[idx].distance * absolute_angle.cos(),
            infos[idx].distance * absolute_angle.sin(),
        );
        let relative = rotate_away_from_edge(previous_end, relative, 0.5);
        let unclamped = previous_end + relative;

        if matches!(map.hit_objects[idx].kind, HitObjectKind::Circle) {
            let clamped = clamp_circle(unclamped, radius);
            let shift = clamped - unclamped;
            map.hit_objects[idx].pos = clamped;

            if shift != Pos::default() {
                shift_preceding_circles(&mut map.hit_objects, idx, shift, radius);
            }
        } else {
            map.hit_objects[idx].pos = unclamped;

            if let HitObjectKind::Slider(slider) = &map.hit_objects[idx].kind {
                let centre_original = slider_centre_of_mass(slider);
                let current_rotation = slider_rotation(slider);
                let centre_modified = rotate(
                    centre_original,
                    infos[idx].rotation + absolute_angle - current_rotation,
                );
                let centre_modified = rotate_away_from_edge(unclamped, centre_modified, 0.5);
                let rotation = centre_modified.y.atan2(centre_modified.x)
                    - centre_original.y.atan2(centre_original.x);
                rotate_slider(&mut map.hit_objects[idx], rotation);
            }

            clamp_slider_head(&mut map.hit_objects[idx], radius);
        }

        previous_end = object_end_pos(&map.hit_objects[idx]);
        previous_angle = if let HitObjectKind::Slider(slider) = &map.hit_objects[idx].kind {
            slider_rotation(slider)
        } else {
            relative.y.atan2(relative.x)
        };
    }
}

fn shift_preceding_circles(objects: &mut [HitObject], idx: usize, shift: Pos, radius: f32) {
    let mut count = 0;
    for object in objects[..idx].iter().rev().take(PRECEDING_OBJECTS_TO_SHIFT) {
        if !object.is_circle() {
            break;
        }
        count += 1;
    }

    for offset in 0..count {
        let object = &mut objects[idx - 1 - offset];
        let ratio = (count - offset) as f32 / (count + 1) as f32;
        object.pos = clamp_circle(object.pos + shift * ratio, radius);
    }
}

fn flip_slider_horizontally(object: &mut HitObject) {
    if let HitObjectKind::Slider(slider) = &mut object.kind {
        for point in &mut slider.control_points {
            point.pos.x = -point.pos.x;
        }
    }
}

fn rotate_slider(object: &mut HitObject, angle: f32) {
    if let HitObjectKind::Slider(slider) = &mut object.kind {
        for point in &mut slider.control_points {
            point.pos = rotate(point.pos, angle);
        }
    }
}

fn clamp_slider_head(object: &mut HitObject, radius: f32) {
    object.pos = clamp_circle(object.pos, radius);
}

fn slider_end_offset(slider: &Slider) -> Pos {
    if slider.repeats % 2 == 1 {
        return Pos::default();
    }

    let mut bufs = CurveBuffers::default();
    slider
        .borrowed_curve(GameMode::Osu, &mut bufs)
        .position_at(1.0)
}

fn slider_rotation(slider: &Slider) -> f32 {
    let mut bufs = CurveBuffers::default();
    let end = slider
        .borrowed_curve(GameMode::Osu, &mut bufs)
        .position_at(1.0);
    end.y.atan2(end.x)
}

fn slider_centre_of_mass(slider: &Slider) -> Pos {
    const SAMPLE_STEP: f64 = 50.0;

    let mut bufs = CurveBuffers::default();
    let curve = slider.borrowed_curve(GameMode::Osu, &mut bufs);
    let distance = curve.dist();

    if distance <= SAMPLE_STEP {
        return curve.position_at(1.0) / 2.0;
    }

    let mut sum = Pos::default();
    let mut count = 0;
    let mut sample = 0.0;

    while sample < distance {
        sum += curve.position_at(sample / distance);
        count += 1;
        sample += SAMPLE_STEP;
    }

    sum / count as f32
}

fn object_end_pos(object: &HitObject) -> Pos {
    match &object.kind {
        HitObjectKind::Slider(slider) => object.pos + slider_end_offset(slider),
        _ => object.pos,
    }
}

fn rotate_away_from_edge(previous: Pos, initial: Pos, ratio: f32) -> Pos {
    let middle = Pos::new(256.0, 192.0);
    let border_x = 192.0;
    let border_y = 144.0;
    let x_distance = if previous.x < middle.x {
        (border_x - previous.x) / border_x
    } else {
        (previous.x - (512.0 - border_x)) / border_x
    };
    let y_distance = if previous.y < middle.y {
        (border_y - previous.y) / border_y
    } else {
        (previous.y - (384.0 - border_y)) / border_y
    };
    let rotate_ratio = (x_distance.max(y_distance).max(0.0) * ratio).min(1.0);
    let destination = middle - previous;
    let initial_angle = initial.y.atan2(initial.x);
    let destination_angle = destination.y.atan2(destination.x);
    let mut diff = destination_angle - initial_angle;
    while diff < -PI {
        diff += TAU;
    }
    while diff > PI {
        diff -= TAU;
    }
    let angle = initial_angle + rotate_ratio * diff;
    Pos::new(length(initial) * angle.cos(), length(initial) * angle.sin())
}

fn rotate(pos: Pos, angle: f32) -> Pos {
    let len = length(pos);
    let angle = pos.y.atan2(pos.x) + angle;
    Pos::new(len * angle.cos(), len * angle.sin())
}

fn clamp_circle(pos: Pos, radius: f32) -> Pos {
    Pos::new(
        pos.x.clamp(radius, PLAYFIELD_BASE_SIZE.x - radius),
        pos.y.clamp(radius, PLAYFIELD_BASE_SIZE.y - radius),
    )
}

fn length(pos: Pos) -> f32 {
    (pos.x * pos.x + pos.y * pos.y).sqrt()
}
fn distance_between(a: Pos, b: Pos) -> f32 {
    length(a - b)
}
fn map_range(value: f32, min: f32, max: f32, out_min: f32, out_max: f32) -> f32 {
    out_min + (value - min) / (max - min) * (out_max - out_min)
}
fn almost_equal(a: f64, b: f64) -> bool {
    (a - b).abs() <= TIMING_PRECISION
}
fn almost_bigger(a: f64, b: f64) -> bool {
    a >= b - TIMING_PRECISION
}
fn definitely_bigger(a: f64, b: f64) -> bool {
    a > b + TIMING_PRECISION
}

struct DotNetRandom {
    seed_array: [i32; 56],
    inext: usize,
    inextp: usize,
}

impl DotNetRandom {
    fn new(seed: i32) -> Self {
        let subtraction = if seed == i32::MIN {
            i32::MAX
        } else {
            seed.abs()
        };
        let mut seed_array = [0; 56];
        let mut mj = 161_803_398_i32.wrapping_sub(subtraction);
        seed_array[55] = mj;
        let mut mk = 1;
        let mut ii = 0;
        for _ in 1..55 {
            ii = (ii + 21) % 55;
            seed_array[ii] = mk;
            mk = mj.wrapping_sub(mk);
            if mk < 0 {
                mk = mk.wrapping_add(i32::MAX);
            }
            mj = seed_array[ii];
        }
        for _ in 0..4 {
            for i in 1..56 {
                seed_array[i] = seed_array[i].wrapping_sub(seed_array[1 + (i + 30) % 55]);
                if seed_array[i] < 0 {
                    seed_array[i] = seed_array[i].wrapping_add(i32::MAX);
                }
            }
        }
        Self {
            seed_array,
            inext: 0,
            inextp: 21,
        }
    }

    fn next(&mut self) -> i32 {
        self.inext += 1;
        if self.inext >= 56 {
            self.inext = 1;
        }
        self.inextp += 1;
        if self.inextp >= 56 {
            self.inextp = 1;
        }
        let mut value = self.seed_array[self.inext].wrapping_sub(self.seed_array[self.inextp]);
        if value == i32::MAX {
            value -= 1;
        }
        if value < 0 {
            value = value.wrapping_add(i32::MAX);
        }
        self.seed_array[self.inext] = value;
        value
    }

    fn next_double(&mut self) -> f64 {
        f64::from(self.next()) / f64::from(i32::MAX)
    }
    fn next_single(&mut self) -> f32 {
        self.next_double() as f32
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::*;
    use crate::model::{
        beatmap::HitObjectComboInfo, control_point::TimingPoint, hit_object::HitSoundType,
    };

    fn slider_fixture() -> Beatmap {
        Beatmap::from_bytes(include_bytes!("../../../../tests/fixture.osu")).unwrap()
    }

    fn metadata_fixture(time_signature: u32, new_combo: bool) -> Beatmap {
        let object_type = if new_combo { 5 } else { 1 };
        let map = format!(
            "osu file format v14\n\
             \n\
             [General]\n\
             Mode:0\n\
             \n\
             [Difficulty]\n\
             HPDrainRate:5\n\
             CircleSize:4\n\
             OverallDifficulty:5\n\
             ApproachRate:8\n\
             SliderMultiplier:1.4\n\
             SliderTickRate:1\n\
             \n\
             [TimingPoints]\n\
             0,500,{time_signature},2,1,60,1,0\n\
             \n\
             [HitObjects]\n\
             100,100,500,1,0,0:0:0:0:\n\
             200,100,1000,1,0,0:0:0:0:\n\
             250,100,1250,1,0,0:0:0:0:\n\
             300,100,1500,{object_type},10,0:2:3:70:\n\
             400,100,2000,1,0,0:0:0:0:\n\
             100,200,2500,1,0,0:0:0:0:\n"
        );

        Beatmap::from_bytes(map.as_bytes()).unwrap()
    }

    #[test]
    fn decoder_preserves_combo_signature_and_sample_metadata() {
        let map = metadata_fixture(3, true);

        assert_eq!(map.timing_points[0].time_signature, 3);
        assert_eq!(map.hit_object_combo_info[3].combo_index, 1);
        assert_eq!(map.hit_object_combo_info[3].index_in_combo, 0);
        assert!(map.hit_object_combo_info[3].starts_new_combo);
        assert_ne!(map.hit_sounds[3], HitSoundType::default());

        // Exact metadata cost: 12 bytes per object plus the Beatmap Vec header,
        // and one extra 8-byte slot per timing point due to alignment.
        assert_eq!(size_of::<HitObjectComboInfo>(), 12);
        assert_eq!(size_of::<TimingPoint>(), 24);
    }

    #[test]
    fn target_practice_reduces_ar_and_generates_seeded_circles() {
        let mut first = slider_fixture();
        let mut second = first.clone();
        let original_ar = first.ar;
        let original_len = first.hit_objects.len();

        apply_target_practice_difficulty(&mut first);
        apply_target_practice(&mut first, 1337);
        apply_target_practice_difficulty(&mut second);
        apply_target_practice(&mut second, 1337);

        assert_eq!(first.ar, original_ar * 0.5);
        assert_ne!(first.hit_objects.len(), original_len);
        assert!(first.hit_objects.iter().all(HitObject::is_circle));
        assert_eq!(first.hit_objects, second.hit_objects);
        assert_eq!(first.hit_sounds, second.hit_sounds);
        assert!(
            first
                .hit_objects
                .windows(2)
                .all(|pair| pair[0].start_time <= pair[1].start_time)
        );
    }

    #[test]
    fn target_practice_preserves_exact_object_samples_and_combo_mapping() {
        let source = metadata_fixture(4, true);
        let expected_sound = source.hit_sounds[3];
        let mut transformed = source.clone();

        apply_target_practice(&mut transformed, 17);

        let index = transformed
            .hit_objects
            .iter()
            .position(|object| almost_equal(object.start_time, 1500.0))
            .unwrap();
        assert_eq!(transformed.hit_sounds[index], expected_sound);
        assert_eq!(transformed.hit_object_combo_info[index].combo_index, 1);
    }

    #[test]
    fn duplicate_target_practice_applies_in_order() {
        let mut once = slider_fixture();
        let original_ar = once.ar;
        apply_target_practice_difficulty(&mut once);
        apply_target_practice(&mut once, 11);

        let mut twice = once.clone();
        apply_target_practice_difficulty(&mut twice);
        apply_target_practice(&mut twice, 29);

        assert_eq!(twice.ar, original_ar * 0.25);
        assert_ne!(twice.hit_objects, once.hit_objects);
    }

    #[test]
    fn random_is_seeded_and_angle_sharpness_changes_slider_geometry() {
        let source = slider_fixture();
        let mut first = source.clone();
        let mut repeated = source.clone();
        let mut other_seed = source.clone();
        let mut other_sharpness = source;

        apply_random(&mut first, 42, 7.0);
        apply_random(&mut repeated, 42, 7.0);
        apply_random(&mut other_seed, 43, 7.0);
        apply_random(&mut other_sharpness, 42, 2.0);

        assert_eq!(first.hit_objects, repeated.hit_objects);
        assert_ne!(first.hit_objects, other_seed.hit_objects);
        assert_ne!(first.hit_objects, other_sharpness.hit_objects);
        assert!(first.hit_objects.iter().any(HitObject::is_slider));
    }

    #[test]
    fn random_uses_new_combo_and_time_signature_metadata() {
        let mut common = metadata_fixture(4, false);
        let mut combo = metadata_fixture(4, true);
        let mut triple = metadata_fixture(3, true);

        apply_random(&mut common, 9, 7.0);
        apply_random(&mut combo, 9, 7.0);
        apply_random(&mut triple, 9, 7.0);

        assert_ne!(common.hit_objects, combo.hit_objects);
        assert_ne!(combo.hit_objects, triple.hit_objects);
    }

    #[test]
    fn duplicate_random_applies_in_order_and_remains_deterministic() {
        let mut first = slider_fixture();
        apply_random(&mut first, 7, 3.0);
        apply_random(&mut first, 19, 8.0);

        let mut repeated = slider_fixture();
        apply_random(&mut repeated, 7, 3.0);
        apply_random(&mut repeated, 19, 8.0);

        let mut reversed = slider_fixture();
        apply_random(&mut reversed, 19, 8.0);
        apply_random(&mut reversed, 7, 3.0);

        assert_eq!(first.hit_objects, repeated.hit_objects);
        assert_ne!(first.hit_objects, reversed.hit_objects);
    }

    #[test]
    fn dotnet_random_wraps_for_extreme_materialized_seeds() {
        for seed in [i32::MIN, i32::MAX, 2_000_000_000] {
            let mut first = DotNetRandom::new(seed);
            let mut repeated = DotNetRandom::new(seed);

            for _ in 0..128 {
                let value = first.next();
                assert_eq!(value, repeated.next());
                assert!((0..i32::MAX).contains(&value));
            }
        }

        let mut first = slider_fixture();
        let mut repeated = first.clone();
        apply_target_practice(&mut first, 2_000_000_000);
        apply_target_practice(&mut repeated, 2_000_000_000);
        assert_eq!(first.hit_objects, repeated.hit_objects);
    }
}

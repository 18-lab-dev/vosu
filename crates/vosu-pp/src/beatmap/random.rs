use super::*;

pub(super) fn apply_random(map: &mut Beatmap, mode: Ruleset, seed: i32, angle_sharpness: f32) {
    let mut rng = CompatRandom::new(seed);
    match mode {
        Ruleset::Taiko => {
            for (object, sound) in map.hit_objects.iter().zip(map.hit_sounds.iter_mut()) {
                if !object.is_circle() {
                    continue;
                }

                if rng.next_max(2) == 0 {
                    *sound &= !(HitSoundType::CLAP | HitSoundType::WHISTLE);
                } else {
                    *sound = HitSoundType::from(u8::from(*sound) | HitSoundType::CLAP);
                }
            }
        }
        Ruleset::Mania => {
            let columns = map.cs.max(1.0).round() as usize;
            let mut shuffled: Vec<_> = (0..columns).collect();
            shuffled.sort_by_key(|_| rng.next());
            let divisor = 512.0 / columns as f32;

            for object in &mut map.hit_objects {
                let old = mania_column(object.pos.x, columns);
                object.pos.x = (shuffled[old] as f32 * divisor).ceil();
            }
        }
        Ruleset::Osu => apply_random_osu(map, &mut rng, angle_sharpness),
        Ruleset::Catch => {}
    }
}

fn apply_random_osu(map: &mut Beatmap, rng: &mut CompatRandom, angle_sharpness_value: f32) {
    const PLAYFIELD_DIAGONAL: f32 = 640.0;
    const PRECEDING_HITOBJECTS_TO_SHIFT: usize = 10;
    let sharpness = angle_sharpness_value.clamp(1.0, 10.0) / 10.0;
    let gaussian_scale = (15.0 - angle_sharpness_value.clamp(1.0, 10.0)) / 8.0;
    let timing_points = map.timing_points.clone();
    let starts: Vec<_> = map.hit_objects.iter().map(|h| h.start_time).collect();
    let combos: Vec<_> = map
        .hit_objects
        .iter()
        .map(|h| (h.index_in_current_combo, h.new_combo))
        .collect();
    let count = map.hit_objects.len();

    // Generate all relative position information before any random slider flips.
    let mut infos = Vec::with_capacity(count);
    let mut previous_position = (256.0_f32, 192.0_f32);
    let mut previous_angle = 0.0_f32;
    for i in 0..count {
        let object = &map.hit_objects[i];
        let relative = (
            object.pos.x - previous_position.0,
            object.pos.y - previous_position.1,
        );
        let absolute_angle = relative.1.atan2(relative.0);
        let (rotation, end) = if object.is_slider() {
            let geometry = map.osu_slider_geometry(i);
            let end = geometry
                .as_ref()
                .map_or((0.0, 0.0), |(_, p, _, _)| (p.x, p.y));
            let rotation = map.osu_slider_rotation(i).unwrap_or(0.0);
            (rotation - absolute_angle, end)
        } else {
            (0.0, (0.0, 0.0))
        };
        infos.push((
            absolute_angle - previous_angle,
            relative.0.hypot(relative.1),
            rotation,
        ));
        previous_position = (object.pos.x + end.0, object.pos.y + end.1);
        previous_angle = if object.is_slider() {
            rotation + absolute_angle
        } else {
            absolute_angle
        };
    }

    let mut section_offset = 0.0_f32;
    let mut flow_direction = false;
    let mut heads = vec![(0.0_f32, 0.0_f32); count];
    let mut ends = vec![(0.0_f32, 0.0_f32); count];
    let mut rotations = vec![0.0_f32; count];

    for i in 0..count {
        let previous_started_combo = i >= 1 && combos[i.saturating_sub(2)].0 > 1 && combos[i - 1].1;
        let should_start_section = if i == 0 {
            true
        } else {
            let previous_time = starts[i - 1];
            let timing = timing_points
                .iter()
                .rev()
                .find(|point| point.time <= previous_time)
                .copied()
                .unwrap_or_default();
            let since = previous_time - timing.time;
            let beat_len = timing.beat_len;
            previous_started_combo && rng.next_double() < 0.6
                || (since + 1.0)
                    .rem_euclid(beat_len * timing.time_signature_numerator as f64)
                    .abs()
                    < 2.0
                || ((since + 1.0).rem_euclid(beat_len).abs() < 2.0 && rng.next_double() < 0.4)
        };
        if should_start_section {
            section_offset = random_gaussian(rng, 0.0008 * gaussian_scale);
            flow_direction = !flow_direction;
        }

        let is_slider = map.hit_objects[i].is_slider();
        if is_slider && rng.next_double() < 0.5 {
            let _ = map.osu_slider_flip_horizontal(i);
        }
        let original_slider_rotation = if is_slider {
            map.osu_slider_rotation(i).unwrap_or(0.0)
        } else {
            0.0
        };

        let relative_angle = if i == 0 {
            infos[i].1 = rng.next_double() as f32 * 192.0;
            rng.next_double() as f32 * std::f32::consts::TAU - std::f32::consts::PI
        } else {
            let one_time_offset = random_gaussian(rng, 0.002 * gaussian_scale);
            let flow_change_offset = if previous_started_combo && rng.next_double() < 0.6 {
                flow_direction = !flow_direction;
                random_gaussian(rng, 0.002 * gaussian_scale)
            } else {
                0.0
            };
            let total_offset = (section_offset + one_time_offset) * infos[i].1
                + flow_change_offset * (PLAYFIELD_DIAGONAL - infos[i].1);
            let angle_sharpness = sharpness;
            let custom_offset_x = angle_sharpness * 100.0 - 70.0;
            let custom_offset_y = (1.0 - angle_sharpness) * 0.25 - 0.075;
            let target_distance = infos[i].1 + custom_offset_x;
            let angle = 2.16
                / (1.0 + 200.0 * (0.036 * (target_distance - 310.0 + custom_offset_x)).exp())
                + 0.5
                + total_offset
                + custom_offset_y;
            let relative_target_angle = std::f32::consts::PI - angle;
            if flow_direction {
                -relative_target_angle
            } else {
                relative_target_angle
            }
        };

        if map.hit_objects[i].is_spinner() {
            heads[i] = (map.hit_objects[i].pos.x, map.hit_objects[i].pos.y);
            ends[i] = heads[i];
            continue;
        }

        let previous_absolute_angle = if i == 0 {
            0.0
        } else if map.hit_objects[i - 1].is_slider() {
            rotations[i - 1]
        } else {
            let earliest = if i > 1 { ends[i - 2] } else { (256.0, 192.0) };
            (heads[i - 1].1 - earliest.1).atan2(heads[i - 1].0 - earliest.0)
        };
        let absolute_angle = previous_absolute_angle + relative_angle;
        let mut relative = (
            infos[i].1 * absolute_angle.cos(),
            infos[i].1 * absolute_angle.sin(),
        );
        let last_end = if i == 0 { (256.0, 192.0) } else { ends[i - 1] };
        relative = rotate_away_from_edge(last_end, relative, 0.5);
        let original_head = (last_end.0 + relative.0, last_end.1 + relative.1);
        let mut head = original_head;

        if is_slider {
            let before_rotation = map.osu_slider_geometry(i);
            let centroid = before_rotation
                .as_ref()
                .map_or((0.0, 0.0), |(_, _, _, c)| (c.x, c.y));
            let current_rotation = map.osu_slider_rotation(i).unwrap_or(0.0);
            rotations[i] = current_rotation;
            let rotated = rotate_vector(
                centroid,
                infos[i].2 + relative.1.atan2(relative.0) - current_rotation,
            );
            let rotated = rotate_away_from_edge(head, rotated, 0.5);
            let relative_rotation = rotated.1.atan2(rotated.0) - centroid.1.atan2(centroid.0);
            let _ = map.osu_slider_rotate(i, relative_rotation);
            rotations[i] = map
                .osu_slider_rotation(i)
                .unwrap_or(current_rotation + relative_rotation);
            let radius = (54.4 - 4.48 * map.cs).max(1.0);
            let (_, _, bounds, _) = map.osu_slider_geometry(i).unwrap();
            let mut left = radius - bounds[0];
            let mut right = 512.0 - radius - bounds[1];
            let mut top = radius - bounds[2];
            let mut bottom = 384.0 - radius - bounds[3];
            if right < left || bottom < top {
                let target_rotation = if angle_difference(original_slider_rotation, rotations[i])
                    < angle_difference(
                        original_slider_rotation + std::f32::consts::PI,
                        rotations[i],
                    ) {
                    original_slider_rotation
                } else {
                    original_slider_rotation + std::f32::consts::PI
                };
                let _ = map.osu_slider_rotate(i, target_rotation - rotations[i]);
                rotations[i] = map.osu_slider_rotation(i).unwrap_or(target_rotation);
                let (_, _, corrected, _) = map.osu_slider_geometry(i).unwrap();
                left = radius - corrected[0];
                right = 512.0 - radius - corrected[1];
                top = radius - corrected[2];
                bottom = 384.0 - radius - corrected[3];
            }
            head.0 = if right < left {
                left.clamp(0.0, 512.0)
            } else {
                head.0.clamp(left, right)
            };
            head.1 = if bottom < top {
                top.clamp(0.0, 384.0)
            } else {
                head.1.clamp(top, bottom)
            };
        } else {
            let radius = (54.4 - 4.48 * map.cs).max(1.0);
            head.0 = head.0.clamp(radius, 512.0 - radius);
            head.1 = head.1.clamp(radius, 384.0 - radius);
        }

        let shift = (head.0 - original_head.0, head.1 - original_head.1);
        if shift.0 != 0.0 || shift.1 != 0.0 {
            let mut previous_circles = Vec::new();
            for j in (i.saturating_sub(PRECEDING_HITOBJECTS_TO_SHIFT)..i).rev() {
                if !map.hit_objects[j].is_circle() {
                    break;
                }
                previous_circles.push(j);
            }
            let count = previous_circles.len();
            let radius = (54.4 - 4.48 * map.cs).max(1.0);
            for (offset, j) in previous_circles.into_iter().enumerate() {
                let factor = (count - offset) as f32 / (count + 1) as f32;
                let mut shifted = (heads[j].0 + shift.0 * factor, heads[j].1 + shift.1 * factor);
                shifted.0 = shifted.0.clamp(radius, 512.0 - radius);
                shifted.1 = shifted.1.clamp(radius, 384.0 - radius);
                map.hit_objects[j].pos.x = shifted.0;
                map.hit_objects[j].pos.y = shifted.1;
                heads[j] = shifted;
                ends[j] = shifted;
            }
        }
        map.hit_objects[i].pos.x = head.0;
        map.hit_objects[i].pos.y = head.1;
        heads[i] = head;
        if is_slider {
            let (_, end, _, _) = map.osu_slider_geometry(i).unwrap();
            ends[i] = (head.0 + end.x, head.1 + end.y);
        } else {
            ends[i] = head;
        }
    }
}

fn random_gaussian(rng: &mut CompatRandom, std_dev: f32) -> f32 {
    let x1 = 1.0 - rng.next_double();
    let x2 = 1.0 - rng.next_double();
    (-2.0 * x1.ln()).sqrt() as f32 * (std::f64::consts::TAU * x2).sin() as f32 * std_dev
}

fn rotate_away_from_edge(
    previous: (f32, f32),
    relative: (f32, f32),
    rotation_ratio: f32,
) -> (f32, f32) {
    let border_x = 512.0 * 0.375;
    let border_y = 384.0 * 0.375;
    let mut edge = 0.0_f32;
    if previous.0 < 256.0 {
        edge = edge.max((border_x - previous.0) / border_x);
    } else {
        edge = edge.max((previous.0 - (512.0 - border_x)) / border_x);
    }
    if previous.1 < 192.0 {
        edge = edge.max((border_y - previous.1) / border_y);
    } else {
        edge = edge.max((previous.1 - (384.0 - border_y)) / border_y);
    }
    let ratio = (edge * rotation_ratio).min(1.0);
    let initial = relative.1.atan2(relative.0);
    let target = (192.0 - previous.1).atan2(256.0 - previous.0);
    let mut diff = target - initial;
    while diff < -std::f32::consts::PI {
        diff += std::f32::consts::TAU;
    }
    while diff > std::f32::consts::PI {
        diff -= std::f32::consts::TAU;
    }
    let angle = initial + ratio * diff;
    let length = (relative.0 * relative.0 + relative.1 * relative.1).sqrt();
    (length * angle.cos(), length * angle.sin())
}
fn rotate_vector(vector: (f32, f32), rotation: f32) -> (f32, f32) {
    let angle = vector.1.atan2(vector.0) + rotation;
    let length = vector.0.hypot(vector.1);
    (length * angle.cos(), length * angle.sin())
}

fn angle_difference(first: f32, second: f32) -> f32 {
    let diff = (first - second).abs() % std::f32::consts::TAU;
    diff.min(std::f32::consts::TAU - diff)
}

struct CompatRandom {
    seed_array: [i32; 56],
    inext: i32,
    inextp: i32,
}

impl CompatRandom {
    fn new(seed: i32) -> Self {
        let mut seed_array = [0; 56];
        let subtraction = if seed == i32::MIN {
            i32::MAX
        } else {
            seed.abs()
        };
        let mut mj = 161_803_398 - subtraction;
        seed_array[55] = mj;
        let mut mk = 1;
        let mut ii = 0;
        for _ in 1..55 {
            ii += 21;
            if ii >= 55 {
                ii -= 55;
            }
            seed_array[ii as usize] = mk;
            mk = mj - mk;
            if mk < 0 {
                mk += i32::MAX;
            }
            mj = seed_array[ii as usize];
        }
        for _ in 1..5 {
            for i in 1..56 {
                let mut n = i + 30;
                if n >= 55 {
                    n -= 55;
                }
                seed_array[i as usize] =
                    seed_array[i as usize].wrapping_sub(seed_array[(1 + n) as usize]);
                if seed_array[i as usize] < 0 {
                    seed_array[i as usize] += i32::MAX;
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
        let mut inext = self.inext + 1;
        if inext >= 56 {
            inext = 1;
        }
        let mut inextp = self.inextp + 1;
        if inextp >= 56 {
            inextp = 1;
        }
        let mut value = self.seed_array[inext as usize] - self.seed_array[inextp as usize];
        if value == i32::MAX {
            value -= 1;
        }
        if value < 0 {
            value += i32::MAX;
        }
        self.seed_array[inext as usize] = value;
        self.inext = inext;
        self.inextp = inextp;
        value
    }

    fn next_max(&mut self, max: i32) -> i32 {
        (f64::from(self.next()) * f64::from(max) / f64::from(i32::MAX)) as i32
    }
    fn next_double(&mut self) -> f64 {
        f64::from(self.next()) / f64::from(i32::MAX)
    }
}

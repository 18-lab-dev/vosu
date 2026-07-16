use std::borrow::Cow;

use rosu_map::section::{general::GameMode, hit_objects::CurveBuffers};

use crate::{
    Difficulty,
    model::{beatmap::Beatmap, mode::ConvertError, mods::Reflection},
};

use super::{
    attributes::OsuDifficultyAttributes,
    difficulty::scaling_factor::ScalingFactor,
    object::{OsuObject, OsuObjectKind},
};

pub fn prepare_map<'map>(
    difficulty: &Difficulty,
    map: &'map Beatmap,
) -> Result<Cow<'map, Beatmap>, ConvertError> {
    map.convert_ref(GameMode::Osu, difficulty.get_mods())
}

pub fn convert_objects(
    map: &Beatmap,
    scaling_factor: &ScalingFactor,
    reflection: Reflection,
    freeze_frame: bool,
    time_preempt: f64,
    mut take: usize,
    attrs: &mut OsuDifficultyAttributes,
) -> Box<[OsuObject]> {
    let mut curve_bufs = CurveBuffers::default();
    // mean=5.16 | median=4
    let mut ticks_buf = Vec::new();

    let mut osu_objects: Box<[_]> = map
        .hit_objects
        .iter()
        .map(|h| OsuObject::new(h, map, reflection, &mut curve_bufs, &mut ticks_buf))
        .inspect(|h| {
            if take == 0 {
                return;
            }

            take -= 1;
            attrs.max_combo += 1;

            match h.kind {
                OsuObjectKind::Circle => attrs.n_circles += 1,
                OsuObjectKind::Slider(ref slider) => {
                    attrs.n_sliders += 1;
                    attrs.n_large_ticks += slider.large_tick_count() as u32;
                    attrs.max_combo += slider.nested_objects.len() as u32;
                }
                OsuObjectKind::Spinner(_) => attrs.n_spinners += 1,
            }
        })
        .collect();

    for object in &mut osu_objects {
        object.time_preempt = time_preempt;
    }

    match reflection {
        Reflection::None => osu_objects.iter_mut().for_each(OsuObject::finalize_nested),
        Reflection::Vertical => osu_objects
            .iter_mut()
            .for_each(OsuObject::reflect_vertically),
        Reflection::Horizontal => osu_objects
            .iter_mut()
            .for_each(OsuObject::reflect_horizontally),
        Reflection::Both => osu_objects
            .iter_mut()
            .for_each(OsuObject::reflect_both_axes),
    }

    let stack_threshold = stack_threshold(time_preempt, map.stack_leniency);

    if map.version >= 6 {
        stacking(&mut osu_objects, stack_threshold);
    } else {
        old_stacking(&mut osu_objects, stack_threshold);
    }

    // `IApplicableToBeatmap` mods run after the osu beatmap processor's
    // stacking post-process. FR therefore changes later visibility only.
    if freeze_frame {
        let mut last_new_combo_time = 0.0;

        for (idx, object) in osu_objects.iter_mut().enumerate() {
            if map
                .hit_object_combo_info
                .get(idx)
                .is_some_and(|combo| combo.starts_new_combo)
            {
                last_new_combo_time = object.start_time;
            }

            if !object.is_spinner() {
                object.time_preempt += object.start_time - last_new_combo_time;
            }
        }
    }

    for h in osu_objects.iter_mut() {
        h.stack_offset = scaling_factor.stack_offset(h.stack_height);
    }

    osu_objects
}

const STACK_DISTANCE: f32 = 3.0;

fn stack_threshold(time_preempt: f64, stack_leniency: f32) -> f64 {
    f64::from((time_preempt as i32 as f32) * stack_leniency)
}

fn stacking(hit_objects: &mut [OsuObject], stack_threshold: f64) {
    let mut extended_start_idx = 0;

    let Some(extended_end_idx) = hit_objects.len().checked_sub(1) else {
        return;
    };

    // First big `if` in osu!lazer's function can be skipped

    for i in (1..=extended_end_idx).rev() {
        let mut n = i;
        let mut obj_i_idx = i;
        // * We should check every note which has not yet got a stack.
        // * Consider the case we have two interwound stacks and this will make sense.
        // *   o <-1      o <-2
        // *    o <-3      o <-4
        // * We first process starting from 4 and handle 2,
        // * then we come backwards on the i loop iteration until we reach 3 and handle 1.
        // * 2 and 1 will be ignored in the i loop because they already have a stack value.

        if hit_objects[obj_i_idx].stack_height != 0 || hit_objects[obj_i_idx].is_spinner() {
            continue;
        }

        // * If this object is a hitcircle, then we enter this "special" case.
        // * It either ends with a stack of hitcircles only,
        // * or a stack of hitcircles that are underneath a slider.
        // * Any other case is handled by the "is_slider" code below this.
        if hit_objects[obj_i_idx].is_circle() {
            loop {
                n = match n.checked_sub(1) {
                    Some(n) => n,
                    None => break,
                };

                if hit_objects[n].is_spinner() {
                    continue;
                }

                if (hit_objects[obj_i_idx].start_time as i32 - hit_objects[n].end_time() as i32)
                    as f64
                    > stack_threshold
                {
                    break; // * We are no longer within stacking range of the previous object.
                }

                // * HitObjects before the specified update range haven't been reset yet
                if n < extended_start_idx {
                    hit_objects[n].stack_height = 0;
                    extended_start_idx = n;
                }

                // * This is a special case where hticircles are moved DOWN and RIGHT (negative stacking)
                // * if they are under the *last* slider in a stacked pattern.
                // *    o==o <- slider is at original location
                // *        o <- hitCircle has stack of -1
                // *         o <- hitCircle has stack of -2
                if hit_objects[n].is_slider()
                    && hit_objects[n]
                        .end_pos()
                        .distance(hit_objects[obj_i_idx].pos)
                        < STACK_DISTANCE
                {
                    let offset =
                        hit_objects[obj_i_idx].stack_height - hit_objects[n].stack_height + 1;

                    for j in n + 1..=i {
                        // * For each object which was declared under this slider, we will offset
                        // * it to appear *below* the slider end (rather than above).
                        if hit_objects[n].end_pos().distance(hit_objects[j].pos) < STACK_DISTANCE {
                            hit_objects[j].stack_height -= offset;
                        }
                    }

                    // * We have hit a slider. We should restart calculation using this as the new base.
                    // * Breaking here will mean that the slider still has StackCount of 0,
                    // * so will be handled in the i-outer-loop.
                    break;
                }

                if hit_objects[n].pos.distance(hit_objects[obj_i_idx].pos) < STACK_DISTANCE {
                    // * Keep processing as if there are no sliders.
                    // * If we come across a slider, this gets cancelled out.
                    // * NOTE: Sliders with start positions stacking
                    // * are a special case that is also handled here.

                    hit_objects[n].stack_height = hit_objects[obj_i_idx].stack_height + 1;
                    obj_i_idx = n;
                }
            }
        } else if hit_objects[obj_i_idx].is_slider() {
            // * We have hit the first slider in a possible stack.
            // * From this point on, we ALWAYS stack positive regardless.
            loop {
                n = match n.checked_sub(1) {
                    Some(n) => n,
                    None => break,
                };

                if hit_objects[n].is_spinner() {
                    continue;
                }

                if hit_objects[obj_i_idx].start_time - hit_objects[n].start_time > stack_threshold {
                    break; // * We are no longer within stacking range of the previous object.
                }

                if hit_objects[n]
                    .end_pos()
                    .distance(hit_objects[obj_i_idx].pos)
                    < STACK_DISTANCE
                {
                    hit_objects[n].stack_height = hit_objects[obj_i_idx].stack_height + 1;
                    obj_i_idx = n;
                }
            }
        }
    }
}

fn old_stacking(hit_objects: &mut [OsuObject], stack_threshold: f64) {
    for i in 0..hit_objects.len() {
        if hit_objects[i].stack_height != 0 && !hit_objects[i].is_slider() {
            continue;
        }

        let mut start_time = hit_objects[i].end_time();

        let pos2 = {
            let h = &hit_objects[i];

            match h.kind {
                OsuObjectKind::Circle | OsuObjectKind::Spinner(_) => h.pos,
                OsuObjectKind::Slider(ref slider) => h.pos + slider.path.position_at(1.0),
            }
        };

        let mut slider_stack = 0;

        for j in i + 1..hit_objects.len() {
            if hit_objects[j].start_time - stack_threshold > start_time {
                break;
            }

            // * Note the use of `StartTime` in the code below doesn't match stable's use of `EndTime`.
            // * This is because in the stable implementation, `UpdateCalculations` is not called on the inner-loop hitobject (j)
            // * and therefore it does not have a correct `EndTime`, but instead the default of `EndTime = StartTime`.
            // *
            // * Effects of this can be seen on https://osu.ppy.sh/beatmapsets/243#osu/1146 at sliders around 86647 ms, where
            // * if we use `EndTime` here it would result in unexpected stacking.

            if hit_objects[j].pos.distance(hit_objects[i].pos) < STACK_DISTANCE {
                hit_objects[i].stack_height += 1;
                start_time = hit_objects[j].start_time;
            } else if hit_objects[j].pos.distance(pos2) < STACK_DISTANCE {
                slider_stack += 1;
                hit_objects[j].stack_height -= slider_stack;
                start_time = hit_objects[j].start_time;
            }
        }
    }
}

#[cfg(test)]
mod source_semantics_tests {
    use super::{convert_objects, stack_threshold};
    use crate::{
        Beatmap,
        model::mods::Reflection,
        osu::{attributes::OsuDifficultyAttributes, difficulty::scaling_factor::ScalingFactor},
    };

    #[test]
    fn stack_threshold_casts_each_objects_preempt_before_leniency() {
        assert_eq!(stack_threshold(825.999, 0.7), f64::from(825.0_f32 * 0.7));
        assert_ne!(stack_threshold(825.999, 0.7), f64::from(825.999_f32 * 0.7));
    }

    #[test]
    fn freeze_frame_extends_preempt_until_explicit_combo_boundary() {
        let map = Beatmap::from_bytes(
            b"osu file format v14\n\
              [General]\nMode:0\n\
              [Difficulty]\nCircleSize:4\nApproachRate:5\nSliderMultiplier:1.4\n\
              [TimingPoints]\n0,500,4,2,1,60,1,0\n\
              [HitObjects]\n\
              100,100,1000,1,0,0:0:0:0:\n\
              100,100,2300,1,0,0:0:0:0:\n\
              300,100,2600,5,0,0:0:0:0:\n\
              400,100,2900,1,0,0:0:0:0:\n\
              256,192,3200,8,0,3500,0:0:0:0:\n",
        )
        .unwrap();
        let scaling = ScalingFactor::new(map.cs);
        let mut attrs = OsuDifficultyAttributes::default();
        let objects = convert_objects(
            &map,
            &scaling,
            Reflection::None,
            true,
            1200.0,
            map.hit_objects.len(),
            &mut attrs,
        );
        let mut plain_attrs = OsuDifficultyAttributes::default();
        let plain = convert_objects(
            &map,
            &scaling,
            Reflection::None,
            false,
            1200.0,
            map.hit_objects.len(),
            &mut plain_attrs,
        );

        let preempts: Vec<_> = objects.iter().map(|object| object.time_preempt).collect();
        assert_eq!(preempts, [2200.0, 3500.0, 1200.0, 1500.0, 1200.0]);
        assert_eq!(
            objects
                .iter()
                .map(|object| object.stack_height)
                .collect::<Vec<_>>(),
            plain
                .iter()
                .map(|object| object.stack_height)
                .collect::<Vec<_>>()
        );
        assert_eq!(objects[0].stack_height, 0);
        assert_eq!(objects[1].stack_height, 0);
    }
}

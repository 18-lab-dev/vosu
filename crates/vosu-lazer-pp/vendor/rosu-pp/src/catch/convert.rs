use std::borrow::Cow;

use rosu_map::section::{general::GameMode, hit_objects::CurveBuffers};

#[cfg(debug_assertions)]
use crate::GameMods;
use crate::{
    Difficulty,
    model::{
        beatmap::Beatmap,
        hit_object::{HitObject, HitObjectKind, HoldNote, Spinner},
        mode::ConvertError,
        mods::Reflection,
    },
    util::{float_ext::FloatExt, random::osu::Random},
};

use super::{
    PLAYFIELD_WIDTH,
    attributes::ObjectCountBuilder,
    catcher::Catcher,
    object::{
        banana_shower::BananaShower,
        fruit::Fruit,
        juice_stream::{JuiceStream, JuiceStreamBufs, NestedJuiceStreamObjectKind},
        palpable::PalpableObject,
    },
};

const RNG_SEED: i32 = 1337;

pub fn prepare_map<'map>(
    difficulty: &Difficulty,
    map: &'map Beatmap,
) -> Result<Cow<'map, Beatmap>, ConvertError> {
    map.convert_ref(GameMode::Catch, difficulty.get_mods())
}

#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct CatchConversionMapping {
    pub start_time: f64,
    pub objects: Vec<CatchConvertedObject>,
}

#[cfg(debug_assertions)]
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq)]
pub struct CatchConvertedObject {
    pub start_time: f64,
    pub position: f64,
    pub hyper_dash: bool,
}

#[cfg(debug_assertions)]
fn legacy_serialized_float(value: f32) -> f64 {
    value
        .to_string()
        .parse()
        .expect("a finite f32 must serialize as an f64")
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn conversion_debug(
    map: &Beatmap,
    mods: &GameMods,
) -> Result<Vec<CatchConversionMapping>, ConvertError> {
    let difficulty = Difficulty::new().mods(mods.clone());
    let converted = prepare_map(&difficulty, map)?;
    let converted = converted.as_ref();
    let map_attrs = converted.attributes().difficulty(&difficulty).build();
    let mut count = ObjectCountBuilder::new_regular(usize::MAX);
    let mut mappings: Vec<_> = converted
        .hit_objects
        .iter()
        .map(|object| CatchConversionMapping {
            start_time: object.start_time,
            objects: Vec::new(),
        })
        .collect();

    convert_objects_debug(
        converted,
        &mut count,
        difficulty.get_mods().reflection(),
        difficulty.get_hardrock_offsets(),
        map_attrs.cs(),
        &mut mappings,
    );

    Ok(mappings)
}

pub const fn convert(map: &mut Beatmap) {
    map.mode = GameMode::Catch;
    map.is_convert = true;
}

pub fn convert_objects(
    map: &Beatmap,
    count: &mut ObjectCountBuilder,
    reflection: Reflection,
    hr_offsets: bool,
    cs: f32,
) -> Vec<PalpableObject> {
    // mean=686.54 | median=501
    let mut palpable_objects = Vec::with_capacity(512);

    let mut bufs = JuiceStreamBufs {
        curve: CurveBuffers::default(),
        // mean=31.65 | median=16
        nested_objects: Vec::with_capacity(16),
        // mean=5.21 | median=4
        ticks: Vec::new(),
    };

    let mut rng = Random::new(RNG_SEED);
    let mut last_pos = None;
    let mut last_start_time = 0.0;

    for h in map.hit_objects.iter() {
        let mut new_objects = convert_object(h, map, count, &mut bufs);

        apply_pos_offset(
            &mut new_objects,
            hr_offsets,
            &mut last_pos,
            &mut last_start_time,
            &mut rng,
            #[cfg(debug_assertions)]
            None,
        );

        palpable_objects.extend(new_objects);
        count.finish_top_level();
    }

    if let Reflection::Horizontal = reflection {
        for h in palpable_objects.iter_mut() {
            h.x = PLAYFIELD_WIDTH - h.x;
            h.x_offset = -h.x_offset;
        }
    }

    palpable_objects.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    initialize_hyper_dash(cs, &mut palpable_objects);

    palpable_objects
}

#[cfg(debug_assertions)]
fn convert_objects_debug(
    map: &Beatmap,
    count: &mut ObjectCountBuilder,
    reflection: Reflection,
    hr_offsets: bool,
    cs: f32,
    mappings: &mut [CatchConversionMapping],
) {
    let mut palpable_objects = Vec::with_capacity(512);
    let mut bufs = JuiceStreamBufs {
        curve: CurveBuffers::default(),
        nested_objects: Vec::with_capacity(16),
        ticks: Vec::new(),
    };
    let mut rng = Random::new(RNG_SEED);
    let mut last_pos = None;
    let mut last_start_time = 0.0;

    for (h, mapping) in map.hit_objects.iter().zip(mappings.iter_mut()) {
        let mut new_objects = convert_object(h, map, count, &mut bufs);
        apply_pos_offset(
            &mut new_objects,
            hr_offsets,
            &mut last_pos,
            &mut last_start_time,
            &mut rng,
            Some(mapping),
        );
        palpable_objects.extend(new_objects);
        count.finish_top_level();
    }

    if let Reflection::Horizontal = reflection {
        for object in palpable_objects.iter_mut() {
            object.x = PLAYFIELD_WIDTH - object.x;
            object.x_offset = -object.x_offset;
        }

        for mapping in mappings.iter_mut() {
            for object in mapping.objects.iter_mut() {
                object.position = f64::from(PLAYFIELD_WIDTH) - object.position;
            }
        }
    }

    palpable_objects.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    initialize_hyper_dash(cs, &mut palpable_objects);

    for mapping in mappings {
        for projected in mapping.objects.iter_mut() {
            projected.hyper_dash = palpable_objects.iter().any(|object| {
                object.hyper_dash
                    && object.start_time == projected.start_time
                    && legacy_serialized_float(object.effective_x()) == projected.position
            });
        }
    }
}

fn convert_object<'a>(
    h: &'a HitObject,
    map: &Beatmap,
    count: &mut ObjectCountBuilder,
    bufs: &'a mut JuiceStreamBufs,
) -> ObjectIter<'a> {
    let state = match h.kind {
        HitObjectKind::Circle => ObjectIterState::Fruit(Some(Fruit::new(count))),
        HitObjectKind::Slider(ref slider) => {
            let stream = JuiceStream::new(h.pos, h.start_time, slider, map, count, bufs);

            ObjectIterState::JuiceStream(stream)
        }
        HitObjectKind::Spinner(Spinner { duration })
        | HitObjectKind::Hold(HoldNote { duration }) => {
            ObjectIterState::BananaShower(BananaShower::new(h.start_time, h.start_time + duration))
        }
    };

    ObjectIter {
        x: h.pos.x,
        start_time: h.start_time,
        state,
    }
}

struct ObjectIter<'a> {
    x: f32,
    start_time: f64,
    state: ObjectIterState<'a>,
}

enum ObjectIterState<'a> {
    Fruit(Option<Fruit>),
    JuiceStream(JuiceStream<'a>),
    BananaShower(BananaShower),
}

impl Iterator for ObjectIter<'_> {
    type Item = PalpableObject;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.state {
            ObjectIterState::Fruit(fruit) => fruit
                .take()
                .map(|fruit| PalpableObject::new(self.x, fruit.x_offset, self.start_time)),
            ObjectIterState::JuiceStream(stream) => stream
                .nested_objects
                .find(|nested| !matches!(nested.kind, NestedJuiceStreamObjectKind::TinyDroplet))
                .map(|nested| PalpableObject::new(nested.pos, nested.x_offset, nested.start_time)),
            ObjectIterState::BananaShower(_) => None,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();

        (len, Some(len))
    }
}

impl ExactSizeIterator for ObjectIter<'_> {
    fn len(&self) -> usize {
        match &self.state {
            ObjectIterState::Fruit(fruit) => usize::from(fruit.is_some()),
            ObjectIterState::JuiceStream(stream) => stream.nested_objects.len(),
            ObjectIterState::BananaShower(_) => 0,
        }
    }
}

fn apply_pos_offset(
    hit_object: &mut ObjectIter<'_>,
    hr_offsets: bool,
    last_pos: &mut Option<f32>,
    last_start_time: &mut f64,
    rng: &mut Random,
    #[cfg(debug_assertions)] mut debug: Option<&mut CatchConversionMapping>,
) {
    match &mut hit_object.state {
        ObjectIterState::Fruit(Some(fruit)) => {
            if hr_offsets {
                apply_hr_offset(
                    hit_object.x,
                    &mut fruit.x_offset,
                    hit_object.start_time,
                    last_pos,
                    last_start_time,
                    rng,
                );
            }

            #[cfg(debug_assertions)]
            if let Some(mapping) = debug.as_mut() {
                mapping.objects.push(CatchConvertedObject {
                    start_time: hit_object.start_time,
                    position: legacy_serialized_float(
                        (hit_object.x + fruit.x_offset).clamp(0.0, PLAYFIELD_WIDTH),
                    ),
                    hyper_dash: false,
                });
            }
        }
        ObjectIterState::JuiceStream(stream) => {
            let pos = hit_object.x
                + stream
                    .control_points
                    .last()
                    .map_or(0.0, |control_point| control_point.pos.x);

            *last_pos = Some(pos);
            *last_start_time = hit_object.start_time;

            for nested in stream.nested_objects.as_slice() {
                let _position = match nested.kind {
                    NestedJuiceStreamObjectKind::TinyDroplet => {
                        let offset = -20 + (rng.next_double() * 40.0) as i32;
                        nested.pos
                            + (offset as f32).clamp(-nested.pos, PLAYFIELD_WIDTH - nested.pos)
                    }
                    NestedJuiceStreamObjectKind::Droplet => {
                        let _ = rng.next_int();

                        nested.pos
                    }
                    NestedJuiceStreamObjectKind::Fruit => nested.pos,
                };

                #[cfg(debug_assertions)]
                if let Some(mapping) = debug.as_mut() {
                    mapping.objects.push(CatchConvertedObject {
                        start_time: nested.start_time.trunc(),
                        position: legacy_serialized_float(_position.clamp(0.0, PLAYFIELD_WIDTH)),
                        hyper_dash: false,
                    });
                }
            }
        }
        ObjectIterState::BananaShower(shower) => {
            for _idx in 0..shower.n_bananas {
                let _position = (rng.next_double() * f64::from(PLAYFIELD_WIDTH)).trunc() as f32;
                let _ = rng.next_int();
                let _ = rng.next_int();
                let _ = rng.next_int();

                #[cfg(debug_assertions)]
                if let Some(mapping) = debug.as_mut() {
                    mapping.objects.push(CatchConvertedObject {
                        start_time: (hit_object.start_time
                            + f64::from(shower.spacing) * _idx as f64)
                            .trunc(),
                        position: legacy_serialized_float(_position),
                        hyper_dash: false,
                    });
                }
            }
        }
        ObjectIterState::Fruit(None) => unreachable!(),
    }
}

fn apply_hr_offset(
    x: f32,
    x_offset: &mut f32,
    start_time: f64,
    last_pos: &mut Option<f32>,
    last_start_time: &mut f64,
    rng: &mut Random,
) {
    let mut offset_pos = x;

    let last_pos = match last_pos {
        Some(pos) if pos.abs() >= f32::EPSILON => pos,
        Some(_) | None => {
            *last_pos = Some(offset_pos);
            *last_start_time = start_time;

            return;
        }
    };

    let pos_diff = offset_pos - *last_pos;
    let time_diff = (start_time - *last_start_time) as i32;

    if time_diff > 1000 {
        *last_pos = offset_pos;
        *last_start_time = start_time;

        return;
    }

    if pos_diff.eq(0.0) {
        apply_random_offset(&mut offset_pos, f64::from(time_diff) / 4.0, rng);
        *x_offset = offset_pos - x;

        return;
    }

    if pos_diff.abs() < (time_diff / 3) as f32 {
        apply_offset(&mut offset_pos, pos_diff);
    }

    *x_offset = offset_pos - x;

    *last_pos = offset_pos;
    *last_start_time = start_time;
}

fn apply_random_offset(pos: &mut f32, max_offset: f64, rng: &mut Random) {
    let right = rng.next_bool();
    let rand = (rng.next_double_range(0.0, max_offset.max(0.0)) as f32).min(20.0);

    if right {
        if *pos + rand <= PLAYFIELD_WIDTH {
            *pos += rand;
        } else {
            *pos -= rand;
        }
    } else if *pos - rand >= 0.0 {
        *pos -= rand;
    } else {
        *pos += rand;
    }
}

fn apply_offset(pos: &mut f32, amount: f32) {
    if amount > 0.0 {
        if *pos + amount < PLAYFIELD_WIDTH {
            *pos += amount;
        }
    } else if *pos + amount > 0.0 {
        *pos += amount;
    }
}

fn initialize_hyper_dash(cs: f32, palpable_objects: &mut [PalpableObject]) {
    let mut half_catcher_width = f64::from(Catcher::calculate_catch_width(cs) / 2.0);
    half_catcher_width /= f64::from(Catcher::ALLOWED_CATCH_RANGE);

    let mut last_dir = 0;
    let mut last_excess = half_catcher_width;

    for i in 0..palpable_objects.len().saturating_sub(1) {
        let next = &palpable_objects[i + 1];
        let curr = &palpable_objects[i];

        let this_dir = if next.effective_x() > curr.effective_x() {
            1
        } else {
            -1
        };

        // * Int truncation added to match osu!stable.
        let time_to_next = f64::from(
            (next.start_time as i32 - curr.start_time as i32) as f32 - 1000.0 / 60.0 / 4.0,
        );

        let dist_to_next = f64::from((next.effective_x() - curr.effective_x()).abs())
            - if last_dir == this_dir {
                last_excess
            } else {
                half_catcher_width
            };

        let dist_to_hyper = (time_to_next * Catcher::BASE_SPEED - dist_to_next) as f32;

        let curr = &mut palpable_objects[i];

        if dist_to_hyper < 0.0 {
            curr.hyper_dash = true;
            last_excess = half_catcher_width;
        } else {
            curr.dist_to_hyper_dash = dist_to_hyper;
            last_excess = f64::from(dist_to_hyper).clamp(0.0, half_catcher_width);
        }

        last_dir = this_dir;
    }
}

use std::{
    borrow::Cow,
    cmp::{self, Ordering},
    sync::atomic::{AtomicI32, Ordering as AtomicOrdering},
    time::{SystemTime, UNIX_EPOCH},
};

use rosu_map::{section::general::GameMode, util::Pos};

use crate::{
    Difficulty, GameMods,
    mania::object::ManiaObject,
    model::{
        beatmap::Beatmap,
        control_point::TimingPoint,
        hit_object::{HitObject, HitObjectKind, HoldNote, Spinner},
        mode::ConvertError,
        mods::rosu_mods::GameMod,
    },
    util::{
        limited_queue::LimitedQueue,
        random::{csharp::Random as CsharpRandom, osu::Random as OsuRandom},
    },
};

use self::{
    pattern::Pattern,
    pattern_generator::{
        end_time_object::EndTimeObjectPatternGenerator, hit_object::HitObjectPatternGenerator,
        path_object::PathObjectPatternGenerator,
    },
    pattern_type::PatternType,
};

mod pattern;
mod pattern_generator;
mod pattern_type;

const MAX_NOTES_FOR_DENSITY: usize = 7;

pub fn prepare_map<'map>(
    difficulty: &Difficulty,
    map: &'map Beatmap,
) -> Result<Cow<'map, Beatmap>, ConvertError> {
    let mods = difficulty.get_mods();
    let mut map = map.convert_ref(GameMode::Mania, mods)?;
    let _ = apply_map_mods(map.to_mut(), mods);

    Ok(map)
}

pub fn convert(map: &mut Beatmap, mods: &GameMods) {
    convert_impl(map, mods, None);
}

fn convert_impl(
    map: &mut Beatmap,
    mods: &GameMods,
    #[cfg(test)] mut debug: Option<&mut ConversionDebug>,
    #[cfg(not(test))] _: Option<&mut ()>,
) {
    let seed = (map.hp + map.cs).round_ties_even() as i32 * 20
        + (map.od * 41.2) as i32
        + map.ar.round_ties_even() as i32;

    let mut random = OsuRandom::new(seed);

    map.cs = target_columns(map, mods) * if mania_dual_stages(mods) { 2.0 } else { 1.0 };

    let mut prev_note_times = LimitedQueue::<f64, MAX_NOTES_FOR_DENSITY>::new();
    let mut density = f64::from(i32::MAX);

    let mut compute_density = |new_note_time: f64, d: &mut f64| {
        prev_note_times.push(new_note_time);

        if let ([first, ..], [.., last]) | ([], [first, .., last]) | ([first, .., last], []) =
            prev_note_times.as_slices()
        {
            *d = (last - first) / prev_note_times.len() as f64;
        }
    };

    let total_columns = map.cs as i32;
    let mut last_values = PrevValues::default();

    // mean=668.7 | median=512
    let mut new_hit_objects = Vec::with_capacity(512);

    for (obj, sound) in map.hit_objects.iter().zip(map.hit_sounds.iter().copied()) {
        #[cfg(test)]
        let generated_start = new_hit_objects.len();

        match obj.kind {
            HitObjectKind::Circle => {
                compute_density(obj.start_time, &mut density);

                let mut generator = HitObjectPatternGenerator::new(
                    &mut random,
                    obj,
                    sound,
                    total_columns,
                    &last_values,
                    density,
                    map,
                );

                let new_pattern = generator.generate();

                last_values.stair = generator.stair_type;
                last_values.time = obj.start_time;
                last_values.pos = obj.pos;

                let new_hit_objects_iter = new_pattern.hit_objects.iter().cloned();
                new_hit_objects.extend(new_hit_objects_iter);

                last_values.pattern = new_pattern;
            }
            HitObjectKind::Slider(ref slider) => {
                let mut generator = PathObjectPatternGenerator::new(
                    &mut random,
                    obj,
                    sound,
                    total_columns,
                    &last_values.pattern,
                    map,
                    slider.repeats,
                    slider.expected_dist,
                    &slider.node_sounds,
                );

                let segment_duration = f64::from(generator.segment_duration);

                for i in 0..=slider.repeats as i32 + 1 {
                    let time = obj.start_time + segment_duration * f64::from(i);

                    last_values.time = time;
                    last_values.pos = obj.pos;

                    compute_density(time, &mut density);
                }

                for new_pattern in generator.generate() {
                    new_hit_objects.extend_from_slice(&new_pattern.hit_objects);
                    last_values.pattern = new_pattern;
                }
            }
            HitObjectKind::Spinner(Spinner { duration })
            | HitObjectKind::Hold(HoldNote { duration }) => {
                let end_time = obj.start_time + duration;

                let mut generator = EndTimeObjectPatternGenerator::new(
                    &mut random,
                    obj,
                    end_time,
                    sound,
                    total_columns,
                    &last_values.pattern,
                    map,
                );

                last_values.time = end_time;
                last_values.pos = Pos::new(256.0, 192.0);

                compute_density(end_time, &mut density);

                let new_pattern = generator.generate();
                new_hit_objects.extend(new_pattern.hit_objects);
            }
        }
        #[cfg(test)]
        let (random_w, random_x, random_y, random_z) = random.state();
        #[cfg(test)]
        if let Some(debug) = debug.as_deref_mut() {
            debug.mappings.push(ConversionDebugMapping::new(
                random_w,
                random_x,
                random_y,
                random_z,
                obj.start_time,
                &new_hit_objects[generated_start..],
                map.cs,
            ));
        }
    }

    map.hit_sounds.clear();
    map.hit_objects = new_hit_objects;
    map.hit_objects.sort_by(cmp_by_start_time);

    map.mode = GameMode::Mania;
    map.is_convert = true;
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ManiaMapModEffects {
    pub random_seed: Option<i32>,
    pub score_v2_active: bool,
    pub classic_active: bool,
}

#[doc(hidden)]
pub fn apply_map_mods(map: &mut Beatmap, mods: &GameMods) -> ManiaMapModEffects {
    prepare_native_map(map);

    if mods.ho() {
        apply_hold_off_to_beatmap(map);
    }

    if mods.invert() {
        apply_invert_to_beatmap(map);
    }

    if mania_mirror(mods) {
        apply_mirror_to_beatmap(map);
    }

    let random_seed = mania_random_seed(mods).map(|seed| seed.unwrap_or_else(random_seed));

    if let Some(seed) = random_seed {
        apply_random_to_beatmap(map, seed);
    }

    map.mania_hitobjects_legacy_sort();

    ManiaMapModEffects {
        random_seed,
        score_v2_active: mania_score_v2(mods),
        classic_active: mania_classic(mods),
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq)]
pub(crate) struct ConversionDebug {
    pub columns: usize,
    pub mappings: Vec<ConversionDebugMapping>,
}

#[cfg(test)]
#[derive(Debug, PartialEq)]
pub(crate) struct ConversionDebugMapping {
    pub random_w: u32,
    pub random_x: u32,
    pub random_y: u32,
    pub random_z: u32,
    pub start_time: f64,
    pub objects: Vec<ConversionDebugObject>,
}

#[cfg(test)]
impl ConversionDebugMapping {
    fn new(
        random_w: u32,
        random_x: u32,
        random_y: u32,
        random_z: u32,
        start_time: f64,
        objects: &[HitObject],
        columns: f32,
    ) -> Self {
        let objects = objects
            .iter()
            .map(|object| ConversionDebugObject {
                start_time: object.start_time,
                end_time: match object.kind {
                    HitObjectKind::Hold(HoldNote { duration }) => object.start_time + duration,
                    _ => object.start_time,
                },
                column: ManiaObject::column(object.pos.x, columns),
            })
            .collect();

        Self {
            random_w,
            random_x,
            random_y,
            random_z,
            start_time,
            objects,
        }
    }
}

#[cfg(test)]
#[derive(Debug, PartialEq)]
pub(crate) struct ConversionDebugObject {
    pub start_time: f64,
    pub end_time: f64,
    pub column: usize,
}

#[cfg(test)]
pub(crate) fn conversion_debug(map: &Beatmap, mods: &GameMods) -> ConversionDebug {
    let mut converted = map.clone();
    let mut debug = ConversionDebug {
        columns: 0,
        mappings: Vec::with_capacity(map.hit_objects.len()),
    };

    if map.mode == GameMode::Mania {
        prepare_native_map_debug(&mut converted, &mut debug);
    } else {
        convert_impl(&mut converted, mods, Some(&mut debug));
    }

    debug.columns = converted.cs as usize;

    debug
}

pub struct PrevValues {
    time: f64,
    pos: Pos,
    pattern: Pattern,
    stair: PatternType,
}

impl Default for PrevValues {
    fn default() -> Self {
        Self {
            time: 0.0,
            pos: Pos::default(),
            pattern: Pattern::default(),
            stair: PatternType::STAIR,
        }
    }
}

fn target_columns(map: &Beatmap, mods: &GameMods) -> f32 {
    if let Some(keys) = mods.mania_keys() {
        return keys;
    }

    let rounded_cs = map.cs.round_ties_even();
    let rounded_od = map.od.round_ties_even();

    if !map.hit_objects.is_empty() {
        let count_slider_or_spinner = map
            .hit_objects
            .iter()
            .filter(|h| matches!(h.kind, HitObjectKind::Slider(_) | HitObjectKind::Spinner(_)))
            .count();

        let len = map.hit_objects.len();

        // * In osu!stable, this division appears as if it happens on floats, but due to release-mode
        // * optimisations, it actually ends up happening on doubles.
        let percent_slider_or_spinner = count_slider_or_spinner as f64 / len as f64;

        if percent_slider_or_spinner < 0.2 {
            return 7.0;
        } else if percent_slider_or_spinner < 0.3 || rounded_cs >= 5.0 {
            return f32::from(6 + u8::from(rounded_od > 5.0));
        } else if percent_slider_or_spinner > 0.6 {
            return f32::from(4 + u8::from(rounded_od > 4.0));
        }
    }

    #[expect(clippy::manual_clamp, reason = "staying in-sync with lazer")]
    {
        cmp::max(cmp::min((rounded_od as i32) + 1, 7), 4) as f32
    }
}

fn apply_hold_off_to_beatmap(map: &mut Beatmap) {
    let new_hit_objects_iter = map.hit_objects.iter().filter_map(|h| {
        if h.is_hold_note() {
            Some(HitObject {
                pos: h.pos,
                start_time: h.start_time,
                kind: HitObjectKind::Circle,
            })
        } else {
            None
        }
    });

    let old_hit_objects_iter = map.hit_objects.iter().filter_map(|h| {
        if h.is_circle() {
            Some(HitObject {
                pos: h.pos,
                start_time: h.start_time,
                kind: HitObjectKind::Circle,
            })
        } else {
            None
        }
    });

    let mut new_hit_objects = Vec::with_capacity(map.hit_objects.len());
    new_hit_objects.extend(old_hit_objects_iter.chain(new_hit_objects_iter));
    map.hit_objects = new_hit_objects;
    map.hit_sounds.clear();
    map.hit_objects.sort_by(cmp_by_start_time);
}

fn apply_invert_to_beatmap(map: &mut Beatmap) {
    let mut new_objects = Vec::with_capacity(map.hit_objects.len());
    let mut column_buf = Vec::new();
    let mut locations = Vec::new();

    let total_columns = map.cs;

    for column in 0..total_columns as usize {
        let iter = map
            .hit_objects
            .iter()
            .filter(|h| ManiaObject::column(h.pos.x, total_columns) == column);

        column_buf.clear();
        column_buf.extend(iter);

        let notes = column_buf
            .iter()
            .filter_map(|h| h.is_circle().then_some(h.start_time));

        let hold_notes = column_buf.iter().filter_map(|h| match h.kind {
            HitObjectKind::Hold(_) => Some(h.start_time),
            _ => None,
        });

        locations.clear();
        locations.extend(notes.chain(hold_notes));
        locations.sort_by(f64::total_cmp);

        let iter = locations.windows(2).map(|window| {
            let [start_time, end_time] = *window else {
                unreachable!()
            };

            // * Full duration of the hold note.
            let mut duration = end_time - start_time;

            // * Beat length at the end of the hold note.
            let beat_length = map
                .timing_point_at(end_time)
                .map_or(TimingPoint::DEFAULT_BEAT_LEN, |tp| tp.beat_len);

            // * Decrease the duration by at most a 1/4 beat to ensure there's no instantaneous notes.
            duration = f64::max(duration / 2.0, duration - beat_length / 4.0);

            HitObject {
                pos: column_buf[0].pos,
                start_time,
                kind: HitObjectKind::Hold(HoldNote { duration }),
            }
        });

        new_objects.extend(iter);
    }

    map.hit_objects = new_objects;
    map.hit_sounds.clear();
    map.hit_objects.sort_by(cmp_by_start_time);

    // * No breaks
    map.breaks.clear();
}

fn apply_random_to_beatmap(map: &mut Beatmap, seed: i32) {
    let mut rng = CsharpRandom::new(seed);

    let total_columns = map.cs;
    let available_columns = total_columns as u8;
    let mut shuffled_columns: Vec<_> = (0..available_columns).collect();

    shuffled_columns.sort_by_cached_key(|_| rng.next());
    let divisor = 512.0 / total_columns;

    for h in map.hit_objects.iter_mut() {
        let old_column = ManiaObject::column(h.pos.x, total_columns);
        let new_column = shuffled_columns[old_column];
        h.pos.x = f32::ceil(f32::from(new_column) * divisor);
    }
}

fn apply_mirror_to_beatmap(map: &mut Beatmap) {
    let total_columns = map.cs;
    let divisor = 512.0 / total_columns;

    for object in &mut map.hit_objects {
        let old_column = ManiaObject::column(object.pos.x, total_columns);
        let new_column = total_columns as usize - 1 - old_column;
        object.pos.x = f32::ceil(new_column as f32 * divisor);
    }
}

fn mania_mirror(mods: &GameMods) -> bool {
    matches!(mods, GameMods::Lazer(mods) if mods.iter().any(|m| matches!(m, GameMod::MirrorMania(_))))
}

fn mania_score_v2(mods: &GameMods) -> bool {
    matches!(mods, GameMods::Lazer(mods) if mods.iter().any(|m| matches!(m, GameMod::ScoreV2Mania(_))))
}

fn mania_classic(mods: &GameMods) -> bool {
    matches!(mods, GameMods::Lazer(mods) if mods.iter().any(|m| matches!(m, GameMod::ClassicMania(_))))
}

fn mania_dual_stages(mods: &GameMods) -> bool {
    matches!(mods, GameMods::Lazer(mods) if mods.iter().any(|m| matches!(m, GameMod::DualStagesMania(_))))
}

fn mania_random_seed(mods: &GameMods) -> Option<Option<i32>> {
    let GameMods::Lazer(mods) = mods else {
        return None;
    };

    mods.iter().find_map(|m| match m {
        GameMod::RandomMania(random) => Some(random.seed.map(|seed| seed as i32)),
        _ => None,
    })
}

fn random_seed() -> i32 {
    static NEXT_SEED: AtomicI32 = AtomicI32::new(0);

    let time_seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.subsec_nanos() as i32);

    time_seed ^ NEXT_SEED.fetch_add(1, AtomicOrdering::Relaxed)
}

fn prepare_native_map(map: &mut Beatmap) {
    prepare_native_map_impl(map, None);
}

fn prepare_native_map_impl(
    map: &mut Beatmap,
    #[cfg(test)] debug: Option<&mut ConversionDebug>,
    #[cfg(not(test))] _: Option<&mut ()>,
) {
    let seed = (map.hp + map.cs).round_ties_even() as i32 * 20
        + (map.od * 41.2) as i32
        + map.ar.round_ties_even() as i32;
    let columns = map.cs.round_ties_even().max(1.0) as i32;

    // Native mania maps above the per-stage limit are represented as two
    // equal stages. Odd key counts above ten are intentionally truncated,
    // matching lazer's converter.
    if columns > 10 {
        map.cs = (columns / 2 * 2) as f32;
    }

    #[cfg(test)]
    convert_native_spinners_impl(map, seed, debug);
    #[cfg(not(test))]
    convert_native_spinners_impl(map, seed, None);
}

#[cfg(test)]
fn prepare_native_map_debug(map: &mut Beatmap, debug: &mut ConversionDebug) {
    prepare_native_map_impl(map, Some(debug));
}

fn convert_native_spinners_impl(
    map: &mut Beatmap,
    seed: i32,
    #[cfg(test)] mut debug: Option<&mut ConversionDebug>,
    #[cfg(not(test))] _: Option<&mut ()>,
) {
    let had_spinner = map
        .hit_objects
        .iter()
        .any(|object| matches!(object.kind, HitObjectKind::Spinner(_)));

    #[cfg(not(test))]
    if !had_spinner {
        return;
    }

    let mut random = OsuRandom::new(seed);
    let total_columns = map.cs as i32;
    let previous_pattern = Pattern::default();
    let mut converted = Vec::with_capacity(map.hit_objects.len());

    for (idx, object) in map.hit_objects.iter().enumerate() {
        #[cfg(test)]
        let generated_start = converted.len();

        if let HitObjectKind::Spinner(Spinner { duration }) = object.kind {
            let sample = map.hit_sounds.get(idx).copied().unwrap_or_default();
            let mut generator = EndTimeObjectPatternGenerator::new(
                &mut random,
                object,
                object.start_time + duration,
                sample,
                total_columns,
                &previous_pattern,
                map,
            );

            converted.extend(generator.generate().hit_objects);
        } else {
            converted.push(object.clone());
        }
        #[cfg(test)]
        let (random_w, random_x, random_y, random_z) = random.state();

        #[cfg(test)]
        if let Some(debug) = debug.as_deref_mut() {
            debug.mappings.push(ConversionDebugMapping::new(
                random_w,
                random_x,
                random_y,
                random_z,
                object.start_time,
                &converted[generated_start..],
                map.cs,
            ));
        }
    }

    map.hit_objects = converted;

    if had_spinner {
        map.hit_sounds.clear();
    }
}

fn cmp_by_start_time(a: &HitObject, b: &HitObject) -> Ordering {
    a.start_time.total_cmp(&b.start_time)
}

#[cfg(test)]
mod tests {
    use crate::{
        mania::difficulty::gradual::ManiaGradualDifficulty,
        model::mods::rosu_mods::{
            GameMods as GameModsLazer,
            generated_mods::{
                ClassicMania, DualStagesMania, FourKeysMania, InvertMania, MirrorMania,
                RandomMania, ScoreV2Mania,
            },
        },
        util::float_ext::FloatExt,
    };

    use super::*;

    fn object(column: usize, columns: f32, start_time: f64, kind: HitObjectKind) -> HitObject {
        HitObject {
            pos: Pos::new((column as f32 + 0.5) * 512.0 / columns, 192.0),
            start_time,
            kind,
        }
    }

    fn native_map(columns: f32, objects: Vec<HitObject>) -> Beatmap {
        Beatmap {
            mode: GameMode::Mania,
            cs: columns,
            hit_sounds: vec![Default::default(); objects.len()],
            hit_objects: objects,
            ..Default::default()
        }
    }

    fn lazer_mods(mods: impl IntoIterator<Item = GameMod>) -> GameMods {
        GameMods::from(mods.into_iter().collect::<GameModsLazer>())
    }

    fn assert_canonical_conversion(name: &str) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../tools/oracle/inputs/osu-97e94bab/mania");
        let map = Beatmap::from_path(root.join(format!("{name}.osu"))).unwrap();
        let expected: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join(format!("{name}-expected-conversion.json"))).unwrap(),
        )
        .unwrap();
        let actual = conversion_debug(&map, &GameMods::default());
        let expected_mappings = expected["Mappings"].as_array().unwrap();

        assert_eq!(
            actual.mappings.len(),
            expected_mappings.len(),
            "{name}: mapping count"
        );

        for (idx, (actual, expected)) in actual.mappings.iter().zip(expected_mappings).enumerate() {
            assert_eq!(
                actual.random_w,
                expected["RandomW"].as_u64().unwrap() as u32,
                "{name}: mapping {idx} RandomW"
            );
            assert_eq!(
                actual.random_x,
                expected["RandomX"].as_u64().unwrap() as u32,
                "{name}: mapping {idx} RandomX"
            );
            assert_eq!(
                actual.random_y,
                expected["RandomY"].as_u64().unwrap() as u32,
                "{name}: mapping {idx} RandomY"
            );
            assert_eq!(
                actual.random_z,
                expected["RandomZ"].as_u64().unwrap() as u32,
                "{name}: mapping {idx} RandomZ"
            );
            assert_eq!(
                actual.start_time,
                expected["StartTime"].as_f64().unwrap(),
                "{name}: mapping {idx} StartTime"
            );

            let expected_objects = expected["Objects"].as_array().unwrap();
            assert_eq!(
                actual.objects.len(),
                expected_objects.len(),
                "{name}: mapping {idx} object count"
            );

            for (object_idx, (actual, expected)) in
                actual.objects.iter().zip(expected_objects).enumerate()
            {
                assert_eq!(
                    actual.start_time,
                    expected["StartTime"].as_f64().unwrap(),
                    "{name}: mapping {idx} object {object_idx} StartTime"
                );
                assert_eq!(
                    actual.end_time,
                    expected["EndTime"].as_f64().unwrap(),
                    "{name}: mapping {idx} object {object_idx} EndTime"
                );
                assert_eq!(
                    actual.column,
                    expected["Column"].as_u64().unwrap() as usize,
                    "{name}: mapping {idx} object {object_idx} Column"
                );
            }
        }
    }

    macro_rules! canonical_conversion_test {
        ($test:ident, $name:literal) => {
            #[test]
            fn $test() {
                assert_canonical_conversion($name);
            }
        };
    }

    canonical_conversion_test!(canonical_conversion_1k, "1K");
    canonical_conversion_test!(canonical_conversion_7k, "7K");
    canonical_conversion_test!(canonical_conversion_10k, "10K");
    canonical_conversion_test!(canonical_conversion_18k, "18K");
    canonical_conversion_test!(canonical_conversion_basic, "basic");
    canonical_conversion_test!(
        canonical_conversion_mania_specific_spinner,
        "mania-specific-spinner"
    );
    canonical_conversion_test!(
        canonical_conversion_zero_length_slider,
        "zero-length-slider"
    );

    #[test]
    fn dual_stages_doubles_converted_target_columns_after_key_mods() {
        let mut map = Beatmap {
            mode: GameMode::Osu,
            cs: 4.0,
            hit_objects: vec![object(0, 4.0, 0.0, HitObjectKind::Circle)],
            hit_sounds: vec![Default::default()],
            ..Default::default()
        };
        let mods = lazer_mods([
            GameMod::FourKeysMania(FourKeysMania::default()),
            GameMod::DualStagesMania(DualStagesMania::default()),
        ]);

        convert(&mut map, &mods);

        assert_eq!(map.cs, 8.0);
        assert!(
            map.hit_objects
                .iter()
                .all(|h| ManiaObject::column(h.pos.x, map.cs) < 8)
        );
    }

    #[test]
    fn native_columns_above_stage_limit_use_even_dual_layout() {
        let difficulty = Difficulty::new();
        let twelve = native_map(12.0, vec![object(11, 12.0, 0.0, HitObjectKind::Circle)]);
        let eleven = native_map(11.0, vec![object(10, 11.0, 0.0, HitObjectKind::Circle)]);

        assert_eq!(prepare_map(&difficulty, &twelve).unwrap().cs, 12.0);
        assert_eq!(prepare_map(&difficulty, &eleven).unwrap().cs, 10.0);
    }

    #[test]
    fn native_spinners_generate_short_notes_and_long_holds() {
        let map = native_map(
            4.0,
            vec![
                object(
                    0,
                    4.0,
                    100.0,
                    HitObjectKind::Spinner(Spinner { duration: 99.0 }),
                ),
                object(
                    0,
                    4.0,
                    300.0,
                    HitObjectKind::Spinner(Spinner { duration: 100.0 }),
                ),
            ],
        );

        let prepared = prepare_map(&Difficulty::new(), &map).unwrap();

        assert!(prepared.hit_objects[0].is_circle());
        assert!(matches!(
            prepared.hit_objects[1].kind,
            HitObjectKind::Hold(HoldNote { duration }) if duration == 100.0
        ));
    }

    #[test]
    fn invert_uses_hold_starts_but_not_hold_ends() {
        let map = native_map(
            4.0,
            vec![
                object(
                    0,
                    4.0,
                    0.0,
                    HitObjectKind::Hold(HoldNote { duration: 100.0 }),
                ),
                object(0, 4.0, 300.0, HitObjectKind::Circle),
            ],
        );
        let mods = lazer_mods([GameMod::InvertMania(InvertMania::default())]);
        let difficulty = Difficulty::new().mods(mods);

        let prepared = prepare_map(&difficulty, &map).unwrap();

        assert_eq!(prepared.hit_objects.len(), 1);
        assert_eq!(prepared.hit_objects[0].start_time, 0.0);
        assert!(matches!(
            prepared.hit_objects[0].kind,
            HitObjectKind::Hold(HoldNote { duration }) if duration == 150.0
        ));
    }

    #[test]
    fn random_without_seed_returns_persistable_seed_and_shuffles() {
        let objects = (0..8)
            .map(|column| object(column, 8.0, column as f64, HitObjectKind::Circle))
            .collect();
        let original = native_map(8.0, objects);
        let unseeded = lazer_mods([GameMod::RandomMania(RandomMania { seed: None })]);
        let mut generated = original.clone();

        let assigned_seed = apply_map_mods(&mut generated, &unseeded)
            .random_seed
            .expect("RD without a seed must materialize one");
        let generated_columns: Vec<_> = generated
            .hit_objects
            .iter()
            .map(|h| ManiaObject::column(h.pos.x, generated.cs))
            .collect();

        assert_ne!(generated_columns, (0..8).collect::<Vec<_>>());

        let seeded = lazer_mods([GameMod::RandomMania(RandomMania {
            seed: Some(f64::from(assigned_seed)),
        })]);
        let mut replay = original;
        assert_eq!(
            apply_map_mods(&mut replay, &seeded).random_seed,
            Some(assigned_seed)
        );
        assert_eq!(replay.hit_objects, generated.hit_objects);
    }

    #[test]
    fn fixed_random_is_one_observable_non_idempotent_transform() {
        let objects = (0..8)
            .map(|column| object(column, 8.0, column as f64, HitObjectKind::Circle))
            .collect();
        let original = native_map(8.0, objects);
        let mods = lazer_mods([GameMod::RandomMania(RandomMania {
            seed: Some(12345.0),
        })]);
        let mut once = original.clone();
        let mut twice = original;

        assert_eq!(apply_map_mods(&mut once, &mods).random_seed, Some(12345));
        let _ = apply_map_mods(&mut twice, &mods);
        let _ = apply_map_mods(&mut twice, &mods);

        assert_ne!(once.hit_objects, twice.hit_objects);
    }

    #[test]
    fn mirror_and_scoring_flags_are_projected_from_actual_mods() {
        let objects = (0..4)
            .map(|column| object(column, 4.0, column as f64, HitObjectKind::Circle))
            .collect();
        let mut map = native_map(4.0, objects);
        let mods = lazer_mods([
            GameMod::MirrorMania(MirrorMania::default()),
            GameMod::ScoreV2Mania(ScoreV2Mania::default()),
            GameMod::ClassicMania(ClassicMania::default()),
        ]);

        let effects = apply_map_mods(&mut map, &mods);
        let columns: Vec<_> = map
            .hit_objects
            .iter()
            .map(|h| ManiaObject::column(h.pos.x, map.cs))
            .collect();

        assert_eq!(columns, [3, 2, 1, 0]);
        assert!(effects.score_v2_active);
        assert!(effects.classic_active);
        assert_eq!(effects.random_seed, None);
    }

    #[test]
    fn gradual_end_times_are_from_prepared_spinner_patterns() {
        let map = native_map(
            4.0,
            vec![
                object(
                    0,
                    4.0,
                    100.0,
                    HitObjectKind::Spinner(Spinner { duration: 50.0 }),
                ),
                object(
                    0,
                    4.0,
                    300.0,
                    HitObjectKind::Spinner(Spinner { duration: 250.0 }),
                ),
            ],
        );

        let mut gradual = ManiaGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert_eq!(
            gradual.object_end_times().collect::<Vec<_>>(),
            [100.0, 550.0]
        );
        assert_eq!(gradual.current_strains().strains, [0.0]);
        gradual.next();
        assert_eq!(gradual.current_strains().strains, [0.0]);
    }

    fn discriminator_map(columns: usize) -> Beatmap {
        let columns_f32 = columns as f32;
        let mut objects = vec![
            object(3, columns_f32, -100.0, HitObjectKind::Circle),
            object(
                0,
                columns_f32,
                0.0,
                HitObjectKind::Hold(HoldNote { duration: 1000.0 }),
            ),
            object(
                1,
                columns_f32,
                200.0,
                HitObjectKind::Hold(HoldNote { duration: 500.0 }),
            ),
            object(
                2,
                columns_f32,
                400.0,
                HitObjectKind::Hold(HoldNote { duration: 800.0 }),
            ),
        ];
        let starts = [
            535.0, 548.0, 600.0, 650.0, 700.0, 750.0, 800.0, 850.0, 900.0, 950.0, 1000.0, 1050.0,
            1083.0, 1097.0, 1150.0, 1200.0, 1250.0, 1300.0, 1350.0, 1400.0, 1450.0,
        ];

        objects.extend(starts.into_iter().enumerate().map(|(idx, start_time)| {
            object(
                idx % columns,
                columns_f32,
                start_time,
                HitObjectKind::Circle,
            )
        }));

        native_map(columns_f32, objects)
    }

    fn difficulty_projection(map: &Beatmap, clock_rate: f64) -> (f64, Vec<f64>) {
        let difficulty = Difficulty::new().clock_rate(clock_rate);
        let attrs = difficulty
            .calculate_for_mode::<crate::mania::Mania>(map)
            .unwrap();
        let strains = difficulty
            .strains_for_mode::<crate::mania::Mania>(map)
            .unwrap();

        (attrs.stars, strains.strains)
    }

    #[test]
    fn discriminator_difficulty_strain_rate_relocation() {
        let map = discriminator_map(4);
        let normal = difficulty_projection(&map, 1.0);
        let custom = difficulty_projection(&map, 1.37);

        assert!(normal.0.is_normal());
        assert!(custom.0.is_normal());
        assert_ne!(normal, custom);
        eprintln!("mania.difficulty-strain normal={normal:?} custom={custom:?}");
    }

    #[test]
    fn discriminator_individual_strain_column_locality() {
        let mut same_column = discriminator_map(4);
        let mut alternating = same_column.clone();

        for (idx, object) in same_column.hit_objects[4..10].iter_mut().enumerate() {
            object.pos.x = 64.0;
            alternating.hit_objects[idx + 4].pos.x = if idx % 2 == 0 { 64.0 } else { 192.0 };
        }

        let same = difficulty_projection(&same_column, 1.37);
        let alternating = difficulty_projection(&alternating, 1.37);

        assert_ne!(same, alternating);
        eprintln!(
            "mania.difficulty-strain.individual-strain same={same:?} alternating={alternating:?}"
        );
    }

    #[test]
    fn discriminator_overall_strain_chord_timing() {
        let mut chord = discriminator_map(4);
        let mut separated = chord.clone();

        for (idx, object) in chord.hit_objects[4..10].iter_mut().enumerate() {
            object.start_time = 600.0;
            object.pos.x = (idx % 4) as f32 * 128.0 + 64.0;
            separated.hit_objects[idx + 4].start_time = 600.0 + idx as f64 * 40.0;
        }

        let chord = difficulty_projection(&chord, 1.37);
        let separated = difficulty_projection(&separated, 1.37);

        assert_ne!(chord, separated);
        eprintln!("mania.difficulty-strain.overall-strain chord={chord:?} separated={separated:?}");
    }

    #[test]
    fn discriminator_hold_factor_enclosing_hold_boundary() {
        let nested = discriminator_map(4);
        let mut released = nested.clone();

        released.hit_objects[1].kind = HitObjectKind::Hold(HoldNote { duration: 650.0 });

        let nested = difficulty_projection(&nested, 1.37);
        let released = difficulty_projection(&released, 1.37);

        assert_ne!(nested, released);
        eprintln!("mania.difficulty-strain.hold-factor nested={nested:?} released={released:?}");
    }

    #[test]
    fn discriminator_section_peaks_include_final_gradual_peak() {
        let map = discriminator_map(4);
        let full = difficulty_projection(&map, 1.37).1;
        let mut gradual =
            ManiaGradualDifficulty::new(Difficulty::new().clock_rate(1.37), &map).unwrap();

        assert_eq!(gradual.current_strains().strains, [0.0]);

        while gradual.next().is_some() {}

        let gradual = gradual.current_strains().strains;
        assert!(full.len() >= 3);
        assert_eq!(gradual, full);
        eprintln!("mania.difficulty-strain.section-peaks peaks={gradual:?}");
    }

    #[test]
    fn discriminator_column_count_changes_strain_projection() {
        let four = difficulty_projection(&discriminator_map(4), 1.37);
        let seven = difficulty_projection(&discriminator_map(7), 1.37);

        assert_ne!(four, seven);
        eprintln!("mania.difficulty-strain.column-count four={four:?} seven={seven:?}");
    }

    #[test]
    fn convert_mania() {
        let map = Beatmap::from_path(crate::test_resource("2785319.osu")).unwrap();
        let map = map.convert(GameMode::Mania, &GameMods::default()).unwrap();
        assert!(map.is_convert);

        assert_eq!(map.mode, GameMode::Mania);
        assert_eq!(map.version, 14);
        assert!(map.ar.eq(9.3), "{} != 9.3", map.ar);
        assert!(map.od.eq(8.8), "{} != 8.8", map.od);
        assert!(map.cs.eq(7.0), "{} != 7.0", map.cs);
        assert!(map.hp.eq(5.0), "{} != 5.0", map.hp);
        assert!(
            map.slider_multiplier.eq(1.7),
            "{} != 1.7",
            map.slider_multiplier
        );
        assert!(
            map.slider_tick_rate.eq(1.0),
            "{} != 1.0",
            map.slider_tick_rate
        );
        assert_eq!(map.hit_objects.len(), 1046);
        assert_eq!(map.hit_sounds.len(), 0);
        assert_eq!(map.timing_points.len(), 1);
        assert_eq!(map.difficulty_points.len(), 50);
        assert_eq!(map.effect_points.len(), 0);
        assert!(map.stack_leniency.eq(0.5), "{} != 0.5", map.stack_leniency);
        assert_eq!(map.breaks.len(), 1);
    }
}

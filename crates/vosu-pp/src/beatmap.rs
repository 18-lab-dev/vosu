use crate::{
    BeatmapDifficultyData, DifficultyAttrsData, DifficultyConfig, Error, ModValue, ParsedMapView,
    Ruleset, ScoreInfoData, StructuredMod, game_mods, ruleset,
};
use rosu_map::section::{general::GameMode, hit_objects::hit_samples::HitSoundType};
use rosu_pp::{
    Beatmap, GameMods,
    model::hit_object::{HitObject, HitObjectKind, HoldNote},
};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub(crate) struct BeatmapState {
    pub original: Beatmap,
    pub base: Beatmap,
    pub playable: Beatmap,
    pub mode: Ruleset,
    pub mods: Vec<StructuredMod>,
    pub game_mods: GameMods,
}

#[derive(Debug)]
pub struct ParsedBeatmap {
    pub(crate) state: BeatmapState,
    pub(crate) shared: Arc<RwLock<BeatmapState>>,
}

impl Clone for ParsedBeatmap {
    fn clone(&self) -> Self {
        Self::from_state(self.state.clone())
    }
}

impl ParsedBeatmap {
    pub(crate) fn from_state(state: BeatmapState) -> Self {
        Self {
            shared: Arc::new(RwLock::new(state.clone())),
            state,
        }
    }

    pub fn mode(&self) -> Ruleset {
        self.state.mode
    }

    pub fn mode_id(&self) -> u8 {
        self.mode().mode_id()
    }

    pub fn object_count(&self) -> u32 {
        self.state.playable.hit_objects.len() as u32
    }

    pub fn structured_mods(&self) -> &[StructuredMod] {
        &self.state.mods
    }

    pub fn classic_scoring(&self) -> bool {
        self.state
            .mods
            .iter()
            .any(|m| m.acronym.eq_ignore_ascii_case("CL"))
    }

    pub fn score_v2(&self) -> bool {
        self.state
            .mods
            .iter()
            .any(|m| m.acronym.eq_ignore_ascii_case("SV2"))
    }

    pub fn convert(&self, mode: Ruleset) -> Result<Self, Error> {
        LiveLazerEngine.convert(self, mode)
    }

    #[cfg(any(test, feature = "oracle-debug"))]
    pub fn debug_playable_map(&self) -> &Beatmap {
        &self.state.playable
    }

    #[cfg(any(test, feature = "oracle-debug"))]
    pub fn debug_game_mods(&self) -> &GameMods {
        &self.state.game_mods
    }

    #[cfg(any(test, feature = "oracle-debug"))]
    pub fn debug_original_map(&self) -> &Beatmap {
        &self.state.original
    }

    #[cfg(any(test, feature = "oracle-debug"))]
    pub fn debug_resolved_config_mods(&self, mods: &[StructuredMod]) -> Vec<StructuredMod> {
        resolve_random_seeds(mods)
    }
}
/// Immutable live calculation preparation shared by snapshot and progressive paths.
#[derive(Debug, Clone)]
pub struct PreparedBeatmap {
    map: Arc<ParsedBeatmap>,
    config: DifficultyConfig,
    difficulty: DifficultyAttrsData,
    max_score: ScoreInfoData,
}

impl PreparedBeatmap {
    pub fn new(
        map: ParsedBeatmap,
        config: DifficultyConfig,
        difficulty: DifficultyAttrsData,
        max_score: ScoreInfoData,
    ) -> Self {
        Self {
            map: Arc::new(map),
            config,
            difficulty,
            max_score,
        }
    }

    pub fn map(&self) -> &Arc<ParsedBeatmap> {
        &self.map
    }

    pub fn config(&self) -> &DifficultyConfig {
        &self.config
    }

    pub fn difficulty(&self) -> &DifficultyAttrsData {
        &self.difficulty
    }

    pub fn max_score(&self) -> &ScoreInfoData {
        &self.max_score
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LiveLazerEngine;

impl LiveLazerEngine {
    pub fn parse(&self, bytes: &[u8]) -> Result<ParsedBeatmap, Error> {
        let map = Beatmap::from_bytes(bytes).map_err(|e| Error::Parse(e.to_string()))?;
        Ok(parsed(map))
    }

    pub fn parse_str(&self, content: &str) -> Result<ParsedBeatmap, Error> {
        self.parse(content.as_bytes())
    }

    pub fn convert(&self, map: &ParsedBeatmap, mode: Ruleset) -> Result<ParsedBeatmap, Error> {
        if map.state.mode == mode {
            return Ok(map.clone());
        }

        let source_mode = map.state.mode;
        let mut base = if source_mode == Ruleset::Mania && mode == Ruleset::Taiko {
            let mut base = map.state.original.clone();
            base.mode = GameMode::Taiko;
            base.is_convert = true;
            base
        } else {
            convert_map(
                map.state.original.clone(),
                mode,
                &GameMods::default().into_core(),
            )
            .map_err(|_| Error::Convert {
                from: source_mode,
                to: mode,
            })?
        };
        if source_mode == Ruleset::Mania && mode == Ruleset::Taiko {
            group_taiko_simultaneous(&mut base);
        }
        Ok(ParsedBeatmap::from_state(BeatmapState {
            original: map.state.original.clone(),
            playable: base.clone(),
            base,
            mode,
            mods: Vec::new(),
            game_mods: GameMods::default(),
        }))
    }

    pub fn apply_mods(&self, map: &mut ParsedBeatmap, mods: &[StructuredMod]) -> Result<(), Error> {
        let resolved = resolve_random_seeds(mods);
        let mut playable = map.state.base.clone();
        apply_mods_to_map(&mut playable, map.state.mode, &resolved);

        let typed_input: Vec<_> = resolved
            .iter()
            .filter(|game_mod| !is_consumed_mod(map.state.mode, &game_mod.acronym))
            .cloned()
            .collect();
        let typed = game_mods(map.state.mode, &typed_input)?;

        map.state.playable = playable;
        map.state.mods = resolved;
        map.state.game_mods = typed;
        *map.shared.write().unwrap_or_else(|p| p.into_inner()) = map.state.clone();
        Ok(())
    }

    pub fn original_beatmap_difficulty(&self, map: &ParsedBeatmap) -> BeatmapDifficultyData {
        raw_difficulty(&map.state.original)
    }

    pub fn beatmap_difficulty(&self, map: &ParsedBeatmap) -> BeatmapDifficultyData {
        let mut builder = map.state.playable.attributes();

        if map.state.mode == Ruleset::Mania {
            builder.mods(map.state.game_mods.clone().into_core());
            builder
                .ar(map.state.playable.ar, true)
                .cs(map.state.base.cs, true)
                .hp(map.state.playable.hp, true);
        } else {
            builder.clock_rate(clock_rate_for_mods(&map.state.mods));
        }

        let attrs = builder.build().apply_clock_rate();

        BeatmapDifficultyData {
            approach_rate: attrs.ar as f32,
            circle_size: attrs.cs,
            overall_difficulty: attrs.od as f32,
            drain_rate: attrs.hp,
        }
    }
}

/// Resolve omitted seeds without consulting wall-clock time. This is
/// intentionally deterministic: a config is often evaluated several times
/// (difficulty, strains, gradual, performance) and all evaluations must see
/// one random map.
pub(crate) fn resolve_random_seeds(mods: &[StructuredMod]) -> Vec<StructuredMod> {
    mods.iter()
        .cloned()
        .enumerate()
        .map(|(index, mut m)| {
            if matches!(m.acronym.to_ascii_uppercase().as_str(), "RD" | "TP")
                && m.setting("seed").is_none()
            {
                let mut seed = 0x6d2b_79f5_u32 ^ index as u32;
                for byte in m.acronym.bytes() {
                    seed = seed.rotate_left(5) ^ u32::from(byte);
                }
                m.settings.insert(
                    "seed".into(),
                    ModValue::Number(f64::from(seed & 0x7fff_ffff)),
                );
            }
            m
        })
        .collect()
}

fn convert_map(
    map: Beatmap,
    mode: Ruleset,
    mods: &GameMods,
) -> Result<Beatmap, rosu_pp::model::mode::ConvertError> {
    if map.mode == mode.game_mode() {
        return Ok(map);
    }

    map.convert(mode.game_mode(), mods)
}

fn parsed(map: Beatmap) -> ParsedBeatmap {
    let mode = ruleset(map.mode);
    ParsedBeatmap::from_state(BeatmapState {
        original: map.clone(),
        base: map.clone(),
        playable: map,
        mode,
        mods: Vec::new(),
        game_mods: GameMods::default(),
    })
}

fn raw_difficulty(map: &Beatmap) -> BeatmapDifficultyData {
    BeatmapDifficultyData {
        approach_rate: map.ar,
        circle_size: map.cs,
        overall_difficulty: map.od,
        drain_rate: map.hp,
    }
}

fn group_taiko_simultaneous(map: &mut Beatmap) {
    if map.hit_objects.is_empty() {
        return;
    }

    let old_sounds = std::mem::take(&mut map.hit_sounds);
    let mut objects = Vec::with_capacity(map.hit_objects.len());
    let mut sounds = Vec::with_capacity(old_sounds.len());
    let mut last_time = None;
    let mut first_can_be_strong = false;
    for (index, object) in map.hit_objects.drain(..).enumerate() {
        if last_time == Some(object.start_time) {
            if first_can_be_strong && let Some(sound) = sounds.last_mut() {
                *sound = HitSoundType::from(u8::from(*sound) | HitSoundType::FINISH);
            }
            continue;
        }
        last_time = Some(object.start_time);
        first_can_be_strong = object.is_circle();
        objects.push(object);
        if let Some(sound) = old_sounds.get(index).copied() {
            sounds.push(sound);
        }
    }
    map.hit_objects = objects;
    map.hit_sounds = sounds;
}

pub(crate) fn clock_rate_for_mods(mods: &[StructuredMod]) -> f64 {
    mods.iter().fold(1.0, |clock_rate, game_mod| {
        let acronym = game_mod.acronym.to_ascii_uppercase();
        let rate = match acronym.as_str() {
            "DT" | "NC" => game_mod.number("speed_change").unwrap_or(1.5),
            "HT" | "DC" => game_mod.number("speed_change").unwrap_or(0.75),
            "WU" | "WD" | "AS" => game_mod.number("initial_rate").unwrap_or(1.0),
            _ => 1.0,
        };

        clock_rate * rate
    })
}

pub(crate) fn is_consumed_mod(mode: Ruleset, acronym: &str) -> bool {
    match (mode, acronym.to_ascii_uppercase().as_str()) {
        (Ruleset::Osu, "RD" | "TP") => true,
        // The committed taiko wrapper vectors require SW as an explicit
        // target no-op during PlayBeatmap.applyMods.
        (Ruleset::Taiko, "RD" | "SW" | "SR") => true,
        (Ruleset::Mania, "RD" | "MR" | "HO" | "IN") => true,
        _ => false,
    }
}

pub(crate) fn apply_mods_to_map(map: &mut Beatmap, mode: Ruleset, mods: &[StructuredMod]) {
    for game_mod in mods {
        let acronym = game_mod.acronym.to_ascii_uppercase();

        match acronym.as_str() {
            "EZ" => {
                map.ar *= 0.5;
                if mode != Ruleset::Mania {
                    map.cs *= 0.5;
                }
                map.hp *= 0.5;

                if mode != Ruleset::Mania {
                    map.od *= 0.5;
                }
            }
            "HR" => {
                map.hp = (map.hp * 1.4).min(10.0);
                match mode {
                    Ruleset::Osu | Ruleset::Catch => {
                        map.od = (map.od * 1.4).min(10.0);
                        map.cs = (map.cs * 1.3).min(10.0);
                        map.ar = (map.ar * 1.4).min(10.0);
                    }
                    Ruleset::Taiko => {
                        map.od = (map.od * 1.4).min(10.0);
                        map.slider_multiplier *= 1.4 * 4.0 / 3.0;
                    }
                    Ruleset::Mania => {}
                }
            }
            "DA" => apply_difficulty_adjust(map, mode, game_mod),
            "TP" if mode == Ruleset::Osu => apply_random(
                map,
                mode,
                game_mod.number("seed").unwrap_or(0.0) as i32,
                game_mod.number("angle_sharpness").unwrap_or(7.0) as f32,
            ),
            "RD" => apply_random(
                map,
                mode,
                game_mod.number("seed").unwrap_or(0.0) as i32,
                game_mod.number("angle_sharpness").unwrap_or(7.0) as f32,
            ),
            "SW" if mode == Ruleset::Taiko => {
                // Explicit target no-op: SW does not mutate taiko hit sounds/types.
            }
            "SR" if mode == Ruleset::Taiko => apply_simplified_rhythm(map, game_mod),
            "MR" if mode == Ruleset::Mania => apply_mirror_mania(map),
            "HO" if mode == Ruleset::Mania => apply_hold_off_mania(map),
            "IN" if mode == Ruleset::Mania => apply_invert_mania(map),
            _ => {}
        }
    }
}

fn apply_difficulty_adjust(map: &mut Beatmap, mode: Ruleset, game_mod: &StructuredMod) {
    let extended = game_mod.boolean("extended_limits").unwrap_or(false);
    let max = if extended { 11.0 } else { 10.0 };
    let mania_od_max = if extended { 15.0 } else { 10.0 };
    let clamp = |value: f64, min: f64, max: f64| value.clamp(min, max) as f32;

    if matches!(mode, Ruleset::Osu | Ruleset::Catch) {
        if let Some(value) = game_mod.number("approach_rate") {
            let min = if extended && mode == Ruleset::Osu {
                -10.0
            } else {
                0.0
            };
            map.ar = clamp(value, min, max);
        }
        if let Some(value) = game_mod.number("circle_size") {
            map.cs = clamp(value, 0.0, max);
        }
    }
    if let Some(value) = game_mod.number("overall_difficulty") {
        let (min, max) = if extended && mode == Ruleset::Mania {
            (-15.0, mania_od_max)
        } else {
            (0.0, max)
        };
        map.od = clamp(value, min, max);
    }
    if let Some(value) = game_mod.number("drain_rate") {
        map.hp = clamp(value, 0.0, max);
    }
}

fn apply_random(map: &mut Beatmap, mode: Ruleset, seed: i32, angle_sharpness: f32) {
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

fn apply_mirror_mania(map: &mut Beatmap) {
    let columns = map.cs.max(1.0).round() as usize;
    for object in &mut map.hit_objects {
        object.pos.x = ((columns - 1 - mania_column(object.pos.x, columns)) as f32
            * (512.0 / columns as f32))
            .ceil();
    }
}

fn apply_hold_off_mania(map: &mut Beatmap) {
    let mut objects = Vec::with_capacity(map.hit_objects.len());
    for object in &map.hit_objects {
        if object.is_circle() {
            objects.push(HitObject {
                pos: object.pos,
                start_time: object.start_time,
                new_combo: object.new_combo,
                index_in_current_combo: object.index_in_current_combo,
                kind: HitObjectKind::Circle,
            });
        }
    }
    for object in &map.hit_objects {
        if object.is_hold_note() {
            objects.push(HitObject {
                pos: object.pos,
                start_time: object.start_time,
                new_combo: object.new_combo,
                index_in_current_combo: object.index_in_current_combo,
                kind: HitObjectKind::Circle,
            });
        }
    }
    objects.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    map.hit_objects = objects;
    map.hit_sounds.clear();
}

fn apply_invert_mania(map: &mut Beatmap) {
    let columns = map.cs.max(1.0).round() as usize;
    let mut grouped: Vec<Vec<(f64, rosu_pp::model::hit_object::Pos)>> = vec![Vec::new(); columns];

    for object in &map.hit_objects {
        let column = mania_column(object.pos.x, columns);
        match &object.kind {
            HitObjectKind::Circle => grouped[column].push((object.start_time, object.pos)),
            HitObjectKind::Hold(_) => grouped[column].push((object.start_time, object.pos)),
            _ => {}
        }
    }

    let mut objects = Vec::with_capacity(map.hit_objects.len());
    for locations in &mut grouped {
        if locations.is_empty() {
            continue;
        }
        locations.sort_by(|a, b| a.0.total_cmp(&b.0));
        let pos = locations[0].1;
        for pair in locations.windows(2) {
            let start_time = pair[0].0;
            let end_time = pair[1].0;
            let beat_length = beat_len_at(map, end_time);
            let delta = end_time - start_time;
            let duration = (delta / 2.0).max(delta - beat_length / 4.0);
            objects.push(HitObject {
                pos,
                start_time,
                new_combo: false,
                index_in_current_combo: 0,
                kind: HitObjectKind::Hold(HoldNote { duration }),
            });
        }
    }

    objects.sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    map.hit_objects = objects;
    map.hit_sounds.clear();
    map.breaks.clear();
}
fn apply_simplified_rhythm(map: &mut Beatmap, game_mod: &StructuredMod) {
    let conversions = [
        (
            8_u32,
            4_u32,
            game_mod.boolean("one_eighth_conversion").unwrap_or(false),
        ),
        (
            6,
            4,
            game_mod.boolean("one_sixth_conversion").unwrap_or(true),
        ),
        (
            3,
            2,
            game_mod.boolean("one_third_conversion").unwrap_or(false),
        ),
    ];

    for (base, adjusted, enabled) in conversions {
        if !enabled {
            continue;
        }

        let mut remove = vec![false; map.hit_objects.len()];
        let mut pattern_start = None;

        for index in 1..=map.hit_objects.len() {
            let continues =
                index < map.hit_objects.len() && snap_between(map, index - 1, index) == base;

            if continues {
                pattern_start.get_or_insert(index - 1);
            } else if let Some(start) = pattern_start.take() {
                for (offset, should_remove) in remove[start..index].iter_mut().enumerate() {
                    let j = start + offset;
                    if base == 8 && offset % 2 == 1 {
                        *should_remove = true;
                    } else if matches!(base, 6 | 3) {
                        if offset % 3 == 1 {
                            *should_remove = true;
                        } else if offset % 3 == 2 {
                            let beat = beat_len_at(map, map.hit_objects[j].start_time);
                            let previous = map.hit_objects[j - 2].start_time;
                            map.hit_objects[j].start_time = previous + beat / adjusted as f64;
                        }
                    }
                }
            }
        }

        let old_sounds = std::mem::take(&mut map.hit_sounds);
        let mut objects = Vec::with_capacity(map.hit_objects.len());
        let mut new_sounds = Vec::with_capacity(old_sounds.len());
        for (index, object) in map.hit_objects.drain(..).enumerate() {
            if !remove[index] {
                objects.push(object);
                if let Some(sound) = old_sounds.get(index).copied() {
                    new_sounds.push(sound);
                }
            }
        }

        map.hit_objects = objects;
        map.hit_sounds = new_sounds;
        map.hit_objects
            .sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    }
}

fn snap_between(map: &Beatmap, current: usize, next: usize) -> u32 {
    let beat = beat_len_at(map, map.hit_objects[next].start_time);
    if beat <= 0.0 {
        return 0;
    }
    let ratio = (map.hit_objects[next].start_time - map.hit_objects[current].start_time) / beat;
    [1_u32, 2, 3, 4, 6, 8, 12, 16]
        .into_iter()
        .min_by(|a, b| {
            (ratio - 1.0 / f64::from(*a))
                .abs()
                .total_cmp(&(ratio - 1.0 / f64::from(*b)).abs())
        })
        .unwrap_or(0)
}

fn beat_len_at(map: &Beatmap, time: f64) -> f64 {
    map.timing_points
        .iter()
        .rev()
        .find(|point| point.time <= time)
        .map_or(500.0, |point| point.beat_len)
}

fn mania_column(x: f32, columns: usize) -> usize {
    ((x * columns as f32 / 512.0).floor() as usize).min(columns.saturating_sub(1))
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

pub(crate) fn configured(
    map: &ParsedBeatmap,
    config: &DifficultyConfig,
) -> Result<(BeatmapState, rosu_pp::Difficulty, Beatmap), Error> {
    let mut state = map.state.clone();
    let view = ParsedMapView {
        mode: state.mode,
        playable: &state.playable,
        mods: &state.mods,
    };
    let (difficulty, playable) = crate::difficulty_builder(&view, config)?;

    let mut combined = state.mods.clone();
    combined.extend(resolve_random_seeds(&config.mods));
    let typed_input: Vec<_> = combined
        .iter()
        .filter(|game_mod| !is_consumed_mod(state.mode, &game_mod.acronym))
        .cloned()
        .collect();

    state.mods = combined;
    state.game_mods = crate::game_mods(state.mode, &typed_input)?;
    state.playable = playable.clone();

    Ok((state, difficulty, playable))
}

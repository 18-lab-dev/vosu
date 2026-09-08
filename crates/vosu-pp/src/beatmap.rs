mod random;
use crate::{
    BeatmapDifficultyData, DifficultyAttrsData, DifficultyConfig, Error, ModValue, ParsedMapView,
    Ruleset, ScoreInfoData, StructuredMod, game_mods, ruleset,
};
use random::apply_random;
use rosu_map::section::{general::GameMode, hit_objects::hit_samples::HitSoundType};
use rosu_pp::{
    Beatmap, GameMods,
    model::hit_object::{HitObject, HitObjectKind, HoldNote},
};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub(crate) struct BeatmapState {
    // Retained source maps are immutable across cloned handles and transformations.
    pub original: Arc<Beatmap>,
    pub base: Arc<Beatmap>,
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
            let mut base = map.state.original.as_ref().clone();
            base.mode = GameMode::Taiko;
            base.is_convert = true;
            base
        } else {
            convert_map(
                map.state.original.as_ref().clone(),
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
        let base = Arc::new(base);
        Ok(ParsedBeatmap::from_state(BeatmapState {
            original: map.state.original.clone(),
            playable: base.as_ref().clone(),
            base,
            mode,
            mods: Vec::new(),
            game_mods: GameMods::default(),
        }))
    }

    pub fn apply_mods(&self, map: &mut ParsedBeatmap, mods: &[StructuredMod]) -> Result<(), Error> {
        let resolved = resolve_random_seeds(mods);
        let mut playable = map.state.base.as_ref().clone();
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
    let original = Arc::new(map.clone());
    ParsedBeatmap::from_state(BeatmapState {
        original: Arc::clone(&original),
        base: original,
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

// Difficulty-only callers do not need the owned state used by performance.
pub(crate) fn configured_difficulty(
    map: &ParsedBeatmap,
    config: &DifficultyConfig,
) -> Result<(rosu_pp::Difficulty, Beatmap), Error> {
    let view = ParsedMapView {
        mode: map.state.mode,
        playable: &map.state.playable,
        mods: &map.state.mods,
    };
    crate::difficulty_builder(&view, config)
}

pub(crate) fn configured(
    map: &ParsedBeatmap,
    config: &DifficultyConfig,
) -> Result<(BeatmapState, rosu_pp::Difficulty, Beatmap), Error> {
    let (difficulty, playable) = configured_difficulty(map, config)?;

    let mode = map.state.mode;
    let mut combined = map.state.mods.clone();
    combined.extend(resolve_random_seeds(&config.mods));
    let typed_input: Vec<_> = combined
        .iter()
        .filter(|game_mod| !is_consumed_mod(mode, &game_mod.acronym))
        .cloned()
        .collect();

    let state = BeatmapState {
        original: Arc::clone(&map.state.original),
        base: Arc::clone(&map.state.base),
        playable: playable.clone(),
        mode,
        mods: combined,
        game_mods: crate::game_mods(mode, &typed_input)?,
    };

    Ok((state, difficulty, playable))
}

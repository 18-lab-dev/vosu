use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rosu_map::Beatmap;
use rosu_map::section::general::GameMode;
use rosu_map::section::hit_objects::HitObjectKind;

use super::objects::*;
use super::syntax::*;
use super::timing::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ruleset {
    Osu,
    Taiko,
    Catch,
    Mania,
}

impl Ruleset {
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Osu),
            1 => Some(Self::Taiko),
            2 => Some(Self::Catch),
            3 => Some(Self::Mania),
            _ => None,
        }
    }

    pub const fn id(self) -> u8 {
        match self {
            Self::Osu => 0,
            Self::Taiko => 1,
            Self::Catch => 2,
            Self::Mania => 3,
        }
    }
}

impl From<GameMode> for Ruleset {
    fn from(value: GameMode) -> Self {
        match value {
            GameMode::Osu => Self::Osu,
            GameMode::Taiko => Self::Taiko,
            GameMode::Catch => Self::Catch,
            GameMode::Mania => Self::Mania,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScoringMode {
    Stable,
    Lazer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMod {
    pub acronym: String,
    pub settings_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModSet {
    /// Checksum of the already-sanitized structured mods. Sanitization belongs
    /// to the shared mod utility so the parser never grows a second policy.
    pub checksum: String,
    pub mods: Arc<[AppliedMod]>,
    clock_rate: f64,
}

impl ModSet {
    pub fn new(
        checksum: impl Into<String>,
        mods: Vec<AppliedMod>,
        clock_rate: f64,
    ) -> Result<Self, BeatmapError> {
        if !clock_rate.is_finite() || clock_rate <= 0.0 {
            return Err(BeatmapError::InvalidClockRate);
        }
        Ok(Self {
            checksum: checksum.into(),
            mods: mods.into(),
            clock_rate,
        })
    }

    pub const fn clock_rate(&self) -> f64 {
        self.clock_rate
    }
}

#[derive(Debug, Clone)]
pub enum BeatmapInput<'a> {
    Path(&'a Path),
    /// Path input with a caller-owned memory checksum or generation. When the
    /// identity and calculation dimensions match the current entry, the cache
    /// returns before touching the filesystem.
    PathWithIdentity {
        path: &'a Path,
        memory_identity: &'a str,
    },
    Content {
        content: &'a str,
        identity_path: Option<&'a Path>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheKey {
    pub canonical_path: Option<PathBuf>,
    pub input_path: Option<PathBuf>,
    pub memory_identity: Option<String>,
    pub source_checksum: String,
    pub requested_mode: Ruleset,
    pub mod_checksum: String,
    pub scoring_mode: ScoringMode,
    pub clock_rate_bits: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DifficultyValues {
    pub approach_rate: f64,
    pub circle_size: f64,
    pub drain_rate: f64,
    pub overall_difficulty: f64,
}
impl Default for DifficultyValues {
    fn default() -> Self {
        Self {
            approach_rate: 0.0,
            circle_size: 0.0,
            drain_rate: 0.0,
            overall_difficulty: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DifficultyAttributes {
    pub stars: f64,
    pub max_combo: u32,
    pub aim: Option<f64>,
    pub speed: Option<f64>,
    pub flashlight: Option<f64>,
    pub slider_factor: Option<f64>,
    pub stamina: Option<f64>,
    pub rhythm: Option<f64>,
    pub color: Option<f64>,
    pub reading: Option<f64>,
    pub hit_window: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PerformanceAttributes {
    pub pp: f64,
    pub accuracy: Option<f64>,
    pub aim: Option<f64>,
    pub difficulty: Option<f64>,
    pub flashlight: Option<f64>,
    pub speed: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScoreData {
    pub max_combo: u32,
    pub maximum_score: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PeakStrains {
    pub section_length: f64,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct EngineOutput {
    pub original_difficulty: DifficultyValues,
    pub converted_difficulty: DifficultyValues,
    pub difficulty: DifficultyAttributes,
    pub hit_windows: vosu_pp::HitWindows,
    pub catch_spatial_window: Option<vosu_pp::CatchSpatialWindow>,
    pub performance: PerformanceAttributes,
    pub maximum_score: ScoreData,
    pub pp_by_accuracy: BTreeMap<u8, f64>,
    pub strains: BTreeMap<String, PeakStrains>,
    pub live_prepared: Option<Arc<vosu_pp::PreparedBeatmap>>,
}
impl PartialEq for EngineOutput {
    fn eq(&self, other: &Self) -> bool {
        self.original_difficulty == other.original_difficulty
            && self.converted_difficulty == other.converted_difficulty
            && self.difficulty == other.difficulty
            && self.hit_windows == other.hit_windows
            && self.catch_spatial_window == other.catch_spatial_window
            && self.performance == other.performance
            && self.maximum_score == other.maximum_score
            && self.pp_by_accuracy == other.pp_by_accuracy
            && self.strains == other.strains
    }
}

pub struct EngineRequest<'a> {
    pub content: &'a str,
    pub original_mode: Ruleset,
    pub effective_mode: Ruleset,
    pub mods: &'a [AppliedMod],
    pub clock_rate: f64,
    pub scoring_mode: ScoringMode,
}

pub trait BeatmapEngine: Send + Sync {
    fn calculate(&self, request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BeatmapError {
    #[error("beatmap path is not a regular file: {0}")]
    NotAFile(PathBuf),
    #[error("failed to read beatmap: {0}")]
    Read(String),
    #[error("failed to parse beatmap: {0}")]
    Parse(String),
    #[error("clock rate must be finite and positive")]
    InvalidClockRate,
    #[error("calculator failed: {0}")]
    Engine(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeatmapMetadata {
    pub title: String,
    pub title_unicode: String,
    pub artist: String,
    pub artist_unicode: String,
    pub creator: String,
    pub difficulty_name: String,
    pub source: String,
    pub tags: String,
    pub beatmap_id: i32,
    pub beatmap_set_id: i32,
    pub audio_file: String,
    pub background_file: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObjectCounts {
    pub circles: u32,
    pub sliders: u32,
    pub spinners: u32,
    pub holds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimingPoint {
    pub start: f64,
    pub beat_length: f64,
    pub bpm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BreakRange {
    pub has_effect: bool,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BpmSummary {
    pub common: f64,
    pub minimum: f64,
    pub maximum: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BeatmapTimes {
    pub preview: i32,
    pub first_object: i32,
    pub first_visible_object: f64,
    pub last_visible_object: f64,
    pub full: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphSeries {
    pub name: String,
    pub data: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StrainGraph {
    pub primary: Vec<f64>,
    pub series: Vec<GraphSeries>,
    pub x_axis: Vec<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EventStatus {
    pub bpm: f64,
    pub kiai: bool,
    pub break_time: bool,
}

#[derive(Debug, Clone)]
pub(super) struct RangeIndex {
    ranges: Arc<[TimeRange]>,
    prefix_max_end: Arc<[f64]>,
}

impl RangeIndex {
    pub(super) fn new(mut ranges: Vec<TimeRange>) -> Self {
        ranges.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.end.total_cmp(&b.end)));
        let mut max = f64::NEG_INFINITY;
        let prefix_max_end = ranges
            .iter()
            .map(|range| {
                max = max.max(range.end);
                max
            })
            .collect::<Vec<_>>();
        Self {
            ranges: ranges.into(),
            prefix_max_end: prefix_max_end.into(),
        }
    }

    pub(super) fn contains(&self, time: f64) -> bool {
        let upper = self.ranges.partition_point(|range| range.start <= time);
        upper != 0 && self.prefix_max_end[upper - 1] >= time
    }
}

#[derive(Debug, Clone)]
pub struct ParsedBeatmap {
    pub(super) key: CacheKey,
    source: Arc<str>,
    clock_rate: f64,
    pub original_mode: Ruleset,
    /// Raw JavaScript-decoded General Mode exposed to API builders, separate
    /// from the strictly parsed engine ruleset.
    pub reporting_mode: i64,
    pub effective_mode: Ruleset,
    pub metadata: BeatmapMetadata,
    pub object_counts: ObjectCounts,
    pub times: BeatmapTimes,
    pub bpm: BpmSummary,
    pub timing_points: Arc<[TimingPoint]>,
    pub breaks: Arc<[BreakRange]>,
    pub kiais: Arc<[TimeRange]>,
    pub engine: EngineOutput,
    nonzero_timing_points: Arc<[TimingPoint]>,
    break_index: RangeIndex,
    kiai_index: RangeIndex,
}

impl ParsedBeatmap {
    pub fn cache_key(&self) -> &CacheKey {
        &self.key
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub const fn clock_rate(&self) -> f64 {
        self.clock_rate
    }
    // Standard-to-mania lane conversion is stable-version-specific. Keep
    // converted maps press-only instead of guessing release assignments.
    pub(crate) fn mania_judgement_events(&self) -> Option<Vec<ManiaJudgementEvent>> {
        (self.original_mode == Ruleset::Mania)
            .then(|| source_mania_judgement_events(&self.source))
            .flatten()
    }

    pub fn event_status(&self, time: f64, clock_rate: f64) -> EventStatus {
        let upper = self
            .nonzero_timing_points
            .partition_point(|point| point.start <= time);
        let raw_bpm = if upper == 0 {
            self.timing_points.first().map_or(0.0, |point| point.bpm)
        } else {
            self.nonzero_timing_points[upper - 1].bpm
        };
        EventStatus {
            bpm: raw_bpm * clock_rate,
            kiai: self.kiai_index.contains(time),
            break_time: self.break_index.contains(time),
        }
    }

    pub fn build_graph(&self, mp3_length: Option<f64>, clock_rate: f64) -> StrainGraph {
        let empty = || StrainGraph {
            primary: Vec::new(),
            series: strain_names(self.effective_mode)
                .iter()
                .map(|name| GraphSeries {
                    name: (*name).to_owned(),
                    data: Vec::new(),
                })
                .collect(),
            x_axis: Vec::new(),
        };
        let Some((primary_name, primary)) =
            primary_strain(self.effective_mode, &self.engine.strains)
        else {
            return empty();
        };
        if !primary.section_length.is_finite()
            || primary.section_length <= 0.0
            || !clock_rate.is_finite()
            || clock_rate <= 0.0
        {
            return empty();
        }
        let start = f64::from(self.times.first_object) / clock_rate;
        let end = self.times.full / clock_rate;
        let first = (self.times.first_visible_object + 0.5).floor() / clock_rate;
        let last = (self.times.last_visible_object + 0.5).floor() / clock_rate;
        let section = primary.section_length;
        let empty_left = (start / section).floor();
        let object_left = ((first - start) / section).floor();
        let audio = mp3_length.unwrap_or(0.0) / clock_rate;
        let (object_right, empty_right) = if audio >= end {
            (
                ((end - last) / section).ceil(),
                ((audio - end) / section).ceil(),
            )
        } else {
            (0.0, 0.0)
        };
        // Negative array lengths are rejected by the source graph calculation.
        if [empty_left, object_left, object_right, empty_right]
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return empty();
        }
        let (empty_left, object_left, object_right, empty_right) = (
            empty_left as usize,
            object_left as usize,
            object_right as usize,
            empty_right as usize,
        );
        let left = empty_left + object_left;
        let right = object_right + empty_right;
        let mut series = Vec::with_capacity(strain_names(self.effective_mode).len());
        for name in strain_names(self.effective_mode) {
            let mut data = Vec::new();
            if let Some(strain) = self.engine.strains.get(*name) {
                data.reserve(left + strain.values.len() + right);
                data.resize(empty_left, -100.0);
                data.resize(left, -50.0);
                data.extend_from_slice(&strain.values);
                data.resize(data.len() + object_right, -50.0);
                data.resize(data.len() + empty_right, -100.0);
            }
            series.push(GraphSeries {
                name: (*name).to_owned(),
                data,
            });
        }
        let mut primary_data = Vec::with_capacity(left + primary.values.len() + right);
        primary_data.resize(left, 0.0);
        primary_data.extend_from_slice(&primary.values);
        primary_data.resize(primary_data.len() + right, 0.0);
        let total = primary.values.len();
        let mut x_axis = Vec::with_capacity(left + total + right);
        x_axis.extend((0..left).map(|i| i as f64 * primary.section_length));
        x_axis.extend((0..total).map(|i| first + i as f64 * primary.section_length));
        x_axis.extend((0..right).map(|i| last + i as f64 * primary.section_length));
        debug_assert_eq!(primary_name, strain_names(self.effective_mode)[0]);
        StrainGraph {
            primary: primary_data,
            series,
            x_axis,
        }
    }
}

pub(super) fn strain_names(mode: Ruleset) -> &'static [&'static str] {
    match mode {
        Ruleset::Osu => &["aim", "aimNoSliders", "reading", "flashlight", "speed"],
        Ruleset::Taiko => &["color", "rhythm", "stamina", "reading"],
        Ruleset::Catch => &["movement"],
        Ruleset::Mania => &["strains"],
    }
}

fn primary_strain(
    mode: Ruleset,
    strains: &BTreeMap<String, PeakStrains>,
) -> Option<(&'static str, &PeakStrains)> {
    let name = strain_names(mode)[0];
    strains.get(name).map(|strain| (name, strain))
}
pub(super) fn parse_map<E: BeatmapEngine>(
    key: CacheKey,
    source: Arc<str>,
    requested_mode: Ruleset,
    mods: &ModSet,
    scoring_mode: ScoringMode,
    engine: &E,
) -> Result<ParsedBeatmap, BeatmapError> {
    let mut beatmap = Beatmap::from_bytes(source.as_bytes())
        .map_err(|error| BeatmapError::Parse(error.to_string()))?;
    let original_mode = Ruleset::from(beatmap.mode);
    // states/beatmap.ts only converts standard maps. Native taiko/catch/mania
    // maps retain their original ruleset even if memory reports another mode.
    let effective_mode = if original_mode == Ruleset::Osu {
        requested_mode
    } else {
        original_mode
    };
    let reporting_mode = section_integer_value(&source, "General", "Mode").unwrap_or(0);
    let engine_output = engine.calculate(EngineRequest {
        content: &source,
        original_mode,
        effective_mode,
        mods: &mods.mods,
        clock_rate: mods.clock_rate(),
        scoring_mode,
    })?;
    let title = metadata_value(&source, "Title")
        .unwrap_or("Unknown Title")
        .to_owned();
    let artist = metadata_value(&source, "Artist")
        .unwrap_or("Unknown Artist")
        .to_owned();
    let title_unicode = match metadata_value(&source, "TitleUnicode") {
        None | Some("Unknown Title") => title.clone(),
        Some(value) => value.to_owned(),
    };
    let artist_unicode = match metadata_value(&source, "ArtistUnicode") {
        None | Some("Unknown Artist") => artist.clone(),
        Some(value) => value.to_owned(),
    };
    let metadata = BeatmapMetadata {
        title,
        title_unicode,
        artist,
        artist_unicode,
        creator: metadata_value(&source, "Creator")
            .unwrap_or("Unknown Creator")
            .to_owned(),
        difficulty_name: metadata_value(&source, "Version")
            .unwrap_or("Normal")
            .to_owned(),
        source: metadata_value(&source, "Source")
            .unwrap_or_default()
            .to_owned(),
        tags: metadata_value(&source, "Tags")
            .unwrap_or_default()
            .to_owned(),
        beatmap_id: section_integer_value(&source, "Metadata", "BeatmapID")
            .map_or(0, |value| value as i32),
        beatmap_set_id: section_integer_value(&source, "Metadata", "BeatmapSetID")
            .map_or(0, |value| value as i32),
        audio_file: general_value(&source, "AudioFilename")
            .unwrap_or_default()
            .to_owned(),
        background_file: background_from_events(&source),
    };
    let preview =
        section_integer_value(&source, "General", "PreviewTime").map_or(-1, |value| value as i32);
    beatmap
        .hit_objects
        .sort_by(|a, b| a.start_time.total_cmp(&b.start_time));
    let counts = source_object_counts(&source);
    // osu-classes defines totalLength from the last start-sorted object, not
    // the maximum end time across every object. A zero/missing expected slider
    // distance uses the actual SliderPath geometry.
    let engine_slider_distance =
        beatmap
            .hit_objects
            .last_mut()
            .and_then(|object| match &mut object.kind {
                HitObjectKind::Slider(slider) => {
                    if slider.path.expected_dist() == Some(0.0) {
                        *slider.path.expected_dist_mut() = None;
                    }
                    Some(slider.path.curve().dist())
                }
                _ => None,
            });
    let actual_slider_distance = source_last_slider_geometry(&source).or(engine_slider_distance);
    let parsed_full_raw = beatmap
        .hit_objects
        .last_mut()
        .map_or(0.0, |object| object.end_time());
    let source_times = source_visible_lengths(&source, actual_slider_distance);
    let parsed_first_raw = beatmap
        .hit_objects
        .first()
        .map_or(0.0, |object| object.start_time);
    let first = js_round_i32(source_times.map_or(parsed_first_raw, |times| times.first_start));
    let full_raw = source_times.map_or(parsed_full_raw, |times| times.total);
    let full = (full_raw + 0.5).floor();
    let source_timing = source_timing_data(&source);
    let beat_lengths = source_timing.beat_lengths;
    let timing_points = beat_lengths
        .iter()
        .map(|point| TimingPoint {
            start: point.start,
            beat_length: point.beat_len.clamp(6.0, 60_000.0),
            bpm: 60_000.0 / point.beat_len.clamp(6.0, 60_000.0),
        })
        .collect::<Vec<_>>();
    let nonzero_timing_points = timing_points
        .iter()
        .copied()
        .filter(|point| point.bpm != 0.0)
        .collect::<Vec<_>>();
    let bpm_end = if source_times.is_none() {
        beat_lengths.last().map_or(0.0, |point| point.start)
    } else {
        source_times.map_or(parsed_full_raw, |times| times.duration_end)
    };
    let bpm = bpm_summary(&beat_lengths, bpm_end, mods.clock_rate());
    let breaks = source_breaks(&source);
    let kiais = source_kiai_ranges(&source_timing.effects);
    let break_index = RangeIndex::new(
        breaks
            .iter()
            .map(|range| TimeRange {
                start: range.start,
                end: range.end,
            })
            .collect(),
    );
    let kiai_index = RangeIndex::new(kiais.clone());
    Ok(ParsedBeatmap {
        key,
        source,
        clock_rate: mods.clock_rate(),
        original_mode,
        reporting_mode,
        effective_mode,
        metadata,
        object_counts: counts,
        times: BeatmapTimes {
            preview,
            first_object: first,
            first_visible_object: source_times
                .map_or(f64::from(first), |times| times.first_visible),
            last_visible_object: source_times.map_or(full_raw, |times| times.last_visible),
            full,
        },
        bpm,
        timing_points: timing_points.into(),
        breaks: breaks.into(),
        kiais: kiais.into(),
        engine: engine_output,
        nonzero_timing_points: nonzero_timing_points.into(),
        break_index,
        kiai_index,
    })
}

use rosu_map::Beatmap;
use rosu_map::section::hit_objects::HitObjectKind;

use super::model::ObjectCounts;
use super::syntax::*;
use super::timing::source_slider_control_points;

#[derive(Clone, Copy)]
pub(super) enum RawLastObject {
    Instant {
        start: f64,
    },
    Slider {
        start: f64,
        spans: f64,
        distance: Option<f64>,
    },
    Spinner {
        start: f64,
        end: f64,
    },
    Hold {
        start: f64,
        end: f64,
    },
}

impl RawLastObject {
    const fn start(self) -> f64 {
        match self {
            Self::Instant { start }
            | Self::Slider { start, .. }
            | Self::Spinner { start, .. }
            | Self::Hold { start, .. } => start,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct SourceObjectTimes {
    pub(super) total: f64,
    pub(super) duration_end: f64,
    pub(super) first_start: f64,
    pub(super) first_visible: f64,
    pub(super) last_visible: f64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ManiaJudgementKind {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManiaJudgementEvent {
    pub(crate) target: i64,
    pub(crate) lane: u8,
    pub(crate) object_index: usize,
    pub(crate) kind: ManiaJudgementKind,
}

fn sample_info_valid(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let mut fields = value.split(':');
    let (Some(normal_set), Some(addition_set)) = (fields.next(), fields.next()) else {
        return false;
    };
    if parse_integer_prefix(normal_set, MAX_PARSE_VALUE).is_none()
        || parse_integer_prefix(addition_set, MAX_PARSE_VALUE).is_none()
    {
        return false;
    }
    fields
        .next()
        .is_none_or(|index| parse_integer_prefix(index, MAX_PARSE_VALUE).is_some())
        && fields
            .next()
            .is_none_or(|volume| parse_integer_prefix(volume, MAX_PARSE_VALUE).is_some())
}

fn js_string_len(value: &str) -> usize {
    value.encode_utf16().count()
}

fn slider_path_valid(value: &str) -> bool {
    let parts: Vec<_> = value.split('|').map(js_trim).collect();
    let point_valid = |point: &str| {
        !point.is_empty()
            && point
                .split(':')
                .all(|coordinate| parse_float_prefix(coordinate, MAX_COORDINATE_VALUE).is_some())
    };
    let segment_valid = |points: &[&str], end_point: Option<&str>, is_first: bool| {
        (is_first || points.len() > 1)
            && points.iter().skip(1).all(|point| point_valid(point))
            && end_point.is_none_or(point_valid)
    };
    let mut start_index = 0;
    let mut end_index = 0;
    let mut is_first = true;
    while {
        end_index += 1;
        end_index < parts.len()
    } {
        if js_string_len(parts[end_index]) > 1 {
            continue;
        }
        let end_point = (end_index < parts.len() - 1).then(|| parts[end_index + 1]);
        if !segment_valid(&parts[start_index..end_index], end_point, is_first) {
            return false;
        }
        start_index = end_index;
        is_first = false;
    }
    end_index <= start_index || segment_valid(&parts[start_index..end_index], None, is_first)
}

fn slider_edge_sets_valid(value: &str, nodes: usize) -> bool {
    value.is_empty() || value.split('|').take(nodes).all(sample_info_valid)
}

pub(super) fn parse_raw_hit_object(line: &str) -> Option<(RawLastObject, i64)> {
    let mut fields = line.split(',').map(js_trim);
    let (x, y, start, kind, hit_sound) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if parse_integer_prefix(x, MAX_COORDINATE_VALUE).is_none()
        || parse_integer_prefix(y, MAX_COORDINATE_VALUE).is_none()
        || parse_integer_prefix(hit_sound, MAX_PARSE_VALUE).is_none()
    {
        return None;
    }
    let start = parse_float_prefix(start, MAX_PARSE_VALUE)?;
    let kind = parse_integer_prefix(kind, MAX_PARSE_VALUE)?;
    if kind & 3 == 3 {
        return None;
    }
    let object = if kind & 1 != 0 {
        let first_extra = fields.next();
        if first_extra.is_some_and(|value| !sample_info_valid(value)) {
            return None;
        }
        if kind & 8 != 0 {
            let end = first_extra.and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))?;
            if fields.next().is_some_and(|value| !sample_info_valid(value)) {
                return None;
            }
            RawLastObject::Spinner {
                start,
                end: end as f64,
            }
        } else if kind & 128 != 0 {
            let extra = first_extra.unwrap_or_default();
            let (end, sample) = extra
                .split_once(':')
                .map_or((extra, ""), |(end, sample)| (end, sample));
            if !sample_info_valid(sample) {
                return None;
            }
            let end = if extra.is_empty() {
                start
            } else {
                parse_float_prefix(end, MAX_PARSE_VALUE)?
            };
            RawLastObject::Hold {
                start,
                end: end.max(start),
            }
        } else {
            RawLastObject::Instant { start }
        }
    } else if kind & 2 != 0 {
        let (path, repeats) = (fields.next()?, fields.next()?);
        if !slider_path_valid(path) {
            return None;
        }
        let repeats = parse_integer_prefix(repeats, MAX_PARSE_VALUE)?;
        let distance = match fields.next() {
            None => None,
            Some(value) => Some(parse_float_prefix(value, MAX_COORDINATE_VALUE)?.max(0.0)),
        };
        let _edge_sounds = fields.next();
        let nodes = repeats.saturating_sub(1).max(0) as usize + 2;
        if fields
            .next()
            .is_some_and(|value| !slider_edge_sets_valid(value, nodes))
            || fields.next().is_some_and(|value| !sample_info_valid(value))
        {
            return None;
        }
        RawLastObject::Slider {
            start,
            spans: repeats.max(1) as f64,
            distance,
        }
    } else if kind & 8 != 0 {
        let end = fields
            .next()
            .and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))?;
        if fields.next().is_some_and(|value| !sample_info_valid(value)) {
            return None;
        }
        RawLastObject::Spinner {
            start,
            end: end as f64,
        }
    } else if kind & 128 != 0 {
        let extra = fields.next().unwrap_or_default();
        let (end, sample) = extra
            .split_once(':')
            .map_or((extra, ""), |(end, sample)| (end, sample));
        if !sample_info_valid(sample) {
            return None;
        }
        let end = if extra.is_empty() {
            start
        } else {
            parse_float_prefix(end, MAX_PARSE_VALUE)?
        };
        RawLastObject::Hold {
            start,
            end: end.max(start),
        }
    } else {
        return None;
    };
    Some((object, kind))
}
/// `mirror` applies osu!stable's Mirror mod, which reverses columns; its replay
/// frames record the reversed columns that the player actually pressed.
pub(super) fn source_mania_judgement_events(
    source: &str,
    mirror: bool,
) -> Option<Vec<ManiaJudgementEvent>> {
    let keys = section_integer_value(source, "Difficulty", "CircleSize")?;
    if !(1..=18).contains(&keys) {
        return None;
    }
    let keys = keys as usize;
    let mut events = Vec::new();
    let mut object_index = 0;
    for (section, line) in SourceLineScanner::new(source) {
        if section != "HitObjects" || line.is_empty() {
            continue;
        }
        let x = line
            .split(',')
            .next()
            .and_then(|value| parse_integer_prefix(js_trim(value), MAX_COORDINATE_VALUE))?;
        let (object, _) = parse_raw_hit_object(line)?;
        let lane = ((x.clamp(0, 511) as usize * keys) / 512).min(keys - 1);
        let lane = if mirror { keys - 1 - lane } else { lane } as u8;
        let current_object_index = object_index;
        object_index += 1;
        match object {
            RawLastObject::Instant { start } => events.push(ManiaJudgementEvent {
                target: start as i64,
                lane,
                object_index: current_object_index,
                kind: ManiaJudgementKind::Press,
            }),
            RawLastObject::Hold { start, end } => {
                events.push(ManiaJudgementEvent {
                    target: start as i64,
                    lane,
                    object_index: current_object_index,
                    kind: ManiaJudgementKind::Press,
                });
                events.push(ManiaJudgementEvent {
                    target: end as i64,
                    lane,
                    object_index: current_object_index,
                    kind: ManiaJudgementKind::Release,
                });
            }
            RawLastObject::Slider { .. } | RawLastObject::Spinner { .. } => return None,
        }
    }
    (!events.is_empty()).then_some(events)
}

pub(super) fn source_visible_lengths(
    source: &str,
    actual_slider_distance: Option<f64>,
) -> Option<SourceObjectTimes> {
    let mut first_start: Option<f64> = None;
    let mut first_visible: Option<f64> = None;
    let mut last_visible: Option<f64> = None;
    let mut last = None;
    for (section, line) in SourceLineScanner::new(source) {
        if line.is_empty() {
            continue;
        }
        if section != "HitObjects" {
            continue;
        }
        let Some((object, kind)) = parse_raw_hit_object(line) else {
            continue;
        };
        let start = object.start();
        if first_start.is_none_or(|first| start < first) {
            first_start = Some(start);
        }
        if kind & 8 == 0 {
            if first_visible.is_none_or(|first| start < first) {
                first_visible = Some(start);
            }
            if last_visible.is_none_or(|last| start >= last) {
                last_visible = Some(start);
            }
        }
        if last.is_none_or(|current: RawLastObject| object.start() >= current.start()) {
            last = Some(object);
        }
    }

    let object = last?;
    let start = object.start();
    let duration_end = match object {
        RawLastObject::Instant { start } => start,
        RawLastObject::Spinner { end, .. } | RawLastObject::Hold { end, .. } => end,
        RawLastObject::Slider {
            start,
            spans,
            distance,
        } => {
            let distance = distance
                .filter(|distance| *distance != 0.0)
                .or(actual_slider_distance)?;
            let (beat_length, slider_velocity) = source_slider_control_points(source, start);
            let velocity = 100.0 * slider_velocity / beat_length;
            start + spans * distance / velocity
        }
    };
    let total = if duration_end == 0.0 || duration_end.is_nan() {
        start
    } else {
        duration_end
    };
    Some(SourceObjectTimes {
        total,
        duration_end,
        first_start: first_start?,
        first_visible: first_visible.unwrap_or(0.0),
        last_visible: last_visible.unwrap_or(0.0),
    })
}

pub(super) fn source_format_version(source: &str) -> i64 {
    const MARKER: &str = "osu file format v";

    for raw_line in source.lines() {
        if raw_line.is_empty() || raw_line.starts_with("//") {
            continue;
        }
        let line = js_trim(raw_line);
        if line.contains(MARKER) {
            if let Some(value) = line.strip_prefix(MARKER) {
                return parse_integer_prefix(value, f64::from(i32::MAX)).unwrap_or(14);
            }
            continue;
        }
        break;
    }

    14
}

pub(super) fn source_last_slider_geometry(source: &str) -> Option<f64> {
    let mut last: Option<(f64, &str, RawLastObject)> = None;
    for (section, line) in SourceLineScanner::new(source) {
        if section != "HitObjects" || line.is_empty() {
            continue;
        }
        let Some((object, _)) = parse_raw_hit_object(line) else {
            continue;
        };
        if last.is_none_or(|(start, _, _)| object.start() >= start) {
            last = Some((object.start(), line, object));
        }
    }
    let (_, line, RawLastObject::Slider { distance, .. }) = last? else {
        return None;
    };
    if distance.is_some_and(|distance| distance > 0.0) {
        return None;
    }

    let fields: Vec<_> = line.split(',').map(js_trim).collect();
    let (x, y, start, hit_sound, path) = (
        fields.first()?,
        fields.get(1)?,
        fields.get(2)?,
        fields.get(4)?,
        fields.get(5)?,
    );
    let x = parse_integer_prefix(x, MAX_COORDINATE_VALUE)?;
    let y = parse_integer_prefix(y, MAX_COORDINATE_VALUE)?;
    let start = parse_float_prefix(start, MAX_PARSE_VALUE)?;
    let hit_sound = parse_integer_prefix(hit_sound, MAX_PARSE_VALUE)?;
    let mut sanitized_path = Vec::new();
    for part in path.split('|').map(js_trim) {
        if js_string_len(part) <= 1 {
            sanitized_path.push(part.to_owned());
            continue;
        }
        let mut coordinates = part.split(':');
        let x = parse_float_prefix(coordinates.next()?, MAX_COORDINATE_VALUE)?.trunc();
        let y = coordinates
            .next()
            .and_then(|value| parse_float_prefix(value, MAX_COORDINATE_VALUE))
            .unwrap_or(x)
            .trunc();
        if coordinates.any(|value| parse_float_prefix(value, MAX_COORDINATE_VALUE).is_none()) {
            return None;
        }
        sanitized_path.push(format!("{x}:{y}"));
    }
    if sanitized_path.len() == 1 && js_string_len(&sanitized_path[0]) <= 1 {
        return Some(0.0);
    }
    let format_version = source_format_version(source);
    let synthetic = format!(
        "osu file format v{format_version}\n[General]\nMode:0\n[Difficulty]\nSliderMultiplier:1\n[TimingPoints]\n0,500,4,2,1,80,1,0\n[HitObjects]\n{x},{y},{start},2,{hit_sound},{},1,0\n",
        sanitized_path.join("|"),
    );
    let mut beatmap = Beatmap::from_bytes(synthetic.as_bytes()).ok()?;
    let object = beatmap.hit_objects.first_mut()?;
    let HitObjectKind::Slider(slider) = &mut object.kind else {
        return None;
    };
    *slider.path.expected_dist_mut() = None;
    Some(slider.path.curve().dist())
}

pub(super) fn source_object_counts(source: &str) -> ObjectCounts {
    let mut counts = ObjectCounts::default();
    for (section, line) in SourceLineScanner::new(source) {
        if section != "HitObjects" || line.is_empty() {
            continue;
        }
        let Some((_object, kind)) = parse_raw_hit_object(line) else {
            continue;
        };
        counts.circles += u32::from(kind & 1 != 0);
        counts.sliders += u32::from(kind & 2 != 0);
        counts.spinners += u32::from(kind & 8 != 0);
        counts.holds += u32::from(kind & 128 != 0);
    }
    counts
}

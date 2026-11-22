use super::syntax::*;
use super::*;

#[derive(Clone, Copy)]
struct SourceTimingRow {
    time: f64,
    beat_length: Option<f64>,
    timing_change: bool,
    kiai: bool,
}

fn source_timing_rows(source: &str) -> Vec<SourceTimingRow> {
    let mut rows = Vec::new();
    for (section, line) in SourceLineScanner::new(source) {
        if section != "TimingPoints" || line.is_empty() {
            continue;
        }
        let fields = line.split(',');
        let length = fields.clone().count();
        if length < 2 {
            continue;
        }
        let get = |index| fields.clone().nth(index);
        let Some(time) = get(0).and_then(|value| parse_float_prefix(value, MAX_PARSE_VALUE)) else {
            continue;
        };
        let timing_change = length < 7 || get(6) == Some("1");
        let optional_fields_valid = (length < 3
            || get(2)
                .and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))
                .is_some_and(|value| value >= 1))
            && (length < 4
                || get(3)
                    .and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))
                    .is_some())
            && (length < 5
                || get(4)
                    .and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))
                    .is_some())
            && (length < 6
                || get(5)
                    .and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))
                    .is_some())
            && (length < 8
                || get(7)
                    .and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))
                    .is_some());
        if !optional_fields_valid {
            continue;
        }
        let beat_length = match get(1).map(|value| parse_float_allow_nan(value, MAX_PARSE_VALUE)) {
            Some(Ok(value)) => value,
            _ => continue,
        };
        if timing_change && beat_length.is_none() {
            continue;
        }
        let effects = get(7)
            .and_then(|value| parse_integer_prefix(value, MAX_PARSE_VALUE))
            .unwrap_or(0);
        rows.push(SourceTimingRow {
            time,
            beat_length,
            timing_change,
            kiai: effects & 1 != 0,
        });
    }
    rows
}

pub(super) fn source_slider_control_points(source: &str, object_time: f64) -> (f64, f64) {
    let rows = source_timing_rows(source);
    let mut timings: Vec<(f64, f64)> = Vec::new();
    let mut difficulties: Vec<(f64, f64)> = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        let time = rows[index].time;
        let mut end = index + 1;
        while end < rows.len() && rows[end].time == time {
            end += 1;
        }
        let group = &rows[index..end];
        if let Some(row) = group.iter().find(|row| row.timing_change)
            && !timings.iter().any(|(installed, _)| *installed == time)
        {
            timings.push((time, row.beat_length.unwrap().clamp(6.0, 60_000.0)));
        }
        let difficulty = group
            .iter()
            .rev()
            .find(|row| !row.timing_change)
            .or_else(|| group.iter().find(|row| row.timing_change));
        if let Some(row) = difficulty
            && !difficulties.iter().any(|(installed, _)| *installed == time)
        {
            let velocity = row
                .beat_length
                .filter(|value| *value < 0.0)
                .map_or(1.0, |value| 100.0 / -value);
            difficulties.push((time, velocity));
        }
        index = end;
    }
    let first_timing = timings.iter().copied().min_by(|a, b| {
        if a.0 == b.0 {
            std::cmp::Ordering::Equal
        } else {
            a.0.total_cmp(&b.0)
        }
    });
    let active_timing = timings
        .iter()
        .copied()
        .filter(|(time, _)| *time <= object_time)
        .max_by(|a, b| {
            if a.0 == b.0 {
                std::cmp::Ordering::Equal
            } else {
                a.0.total_cmp(&b.0)
            }
        });
    let active_difficulty = difficulties
        .iter()
        .copied()
        .filter(|(time, _)| *time <= object_time)
        .max_by(|a, b| {
            if a.0 == b.0 {
                std::cmp::Ordering::Equal
            } else {
                a.0.total_cmp(&b.0)
            }
        });
    (
        active_timing
            .or(first_timing)
            .map_or(1000.0, |(_, beat_length)| beat_length),
        active_difficulty.map_or(1.0, |(_, velocity)| velocity),
    )
}

pub(super) struct SourceTimingData {
    pub(super) beat_lengths: Vec<BeatLengthPoint>,
    pub(super) effects: Vec<(f64, bool)>,
}

pub(super) fn source_timing_data(source: &str) -> SourceTimingData {
    let rows = source_timing_rows(source);
    let mut output = SourceTimingData {
        beat_lengths: Vec::new(),
        effects: Vec::new(),
    };
    let mut index = 0;
    while index < rows.len() {
        let time = rows[index].time;
        let mut end = index + 1;
        while end < rows.len() && rows[end].time == time {
            end += 1;
        }
        let group = &rows[index..end];
        if let Some(row) = group.iter().find(|row| row.timing_change)
            && !output
                .beat_lengths
                .iter()
                .any(|installed| installed.start == time)
        {
            output.beat_lengths.push(BeatLengthPoint {
                start: time,
                beat_len: row.beat_length.unwrap(),
            });
        }
        if !output
            .effects
            .iter()
            .any(|(installed, _)| *installed == time)
        {
            let selected = group
                .iter()
                .rev()
                .find(|row| !row.timing_change)
                .unwrap_or(&group[0]);
            output.effects.push((time, selected.kiai));
        }
        index = end;
    }
    output.beat_lengths.sort_by(|a, b| {
        if a.start == b.start {
            std::cmp::Ordering::Equal
        } else {
            a.start.total_cmp(&b.start)
        }
    });
    output.effects.sort_by(|a, b| {
        if a.0 == b.0 {
            std::cmp::Ordering::Equal
        } else {
            a.0.total_cmp(&b.0)
        }
    });
    output
}

fn event_is_break(value: &str) -> bool {
    if matches!(value.as_bytes().first(), Some(b' ' | b'_')) {
        return false;
    }
    let value = js_trim(value);
    value == "Break" || parse_integer_prefix(value, MAX_PARSE_VALUE) == Some(2)
}

pub(super) fn source_breaks(source: &str) -> Vec<BreakRange> {
    let mut breaks = Vec::new();
    for (section, line) in SourceLineScanner::new(source) {
        if section != "Events" || line.is_empty() {
            continue;
        }
        let mut fields = line.split(',');
        let Some(kind) = fields.next() else {
            continue;
        };
        if !event_is_break(kind) {
            continue;
        }
        let (Some(start), Some(end)) = (
            fields
                .next()
                .and_then(|value| parse_float_prefix(value, MAX_PARSE_VALUE)),
            fields
                .next()
                .and_then(|value| parse_float_prefix(value, MAX_PARSE_VALUE)),
        ) else {
            continue;
        };
        let end = end.max(start);
        breaks.push(BreakRange {
            has_effect: end - start >= 650.0,
            start,
            end,
        });
    }
    breaks
}

pub(super) fn source_kiai_ranges(points: &[(f64, bool)]) -> Vec<TimeRange> {
    let mut ranges: Vec<TimeRange> = Vec::new();
    for &(time, kiai) in points {
        let ended = ranges.last().is_none_or(|range| range.end != -1.0);
        if !kiai && !ended {
            ranges.last_mut().expect("open range exists").end = time;
        } else if kiai && ended {
            ranges.push(TimeRange {
                start: time,
                end: -1.0,
            });
        }
    }
    ranges
}
#[derive(Debug, Clone, Copy)]
pub(super) struct BeatLengthPoint {
    pub(super) start: f64,
    pub(super) beat_len: f64,
}

pub(super) fn bpm_summary(points: &[BeatLengthPoint], map_end: f64, clock_rate: f64) -> BpmSummary {
    if points.is_empty() {
        let default_bpm = 60.0 * clock_rate;
        return BpmSummary {
            common: default_bpm,
            minimum: default_bpm,
            maximum: default_bpm,
        };
    }
    let minimum_beat_len = points
        .iter()
        .map(|point| point.beat_len.clamp(6.0, 60_000.0))
        .fold(f64::INFINITY, f64::min);
    let maximum_beat_len = points
        .iter()
        .map(|point| point.beat_len.clamp(6.0, 60_000.0))
        .fold(f64::NEG_INFINITY, f64::max);
    let mut groups: Vec<(f64, f64)> = Vec::new();
    for (index, point) in points.iter().enumerate() {
        let clamped = point.beat_len.clamp(6.0, 60_000.0);
        let rounded = (clamped * 1000.0).round_ties_even() / 1000.0;
        let group_index =
            if let Some(index) = groups.iter().position(|(beat_len, _)| *beat_len == rounded) {
                index
            } else {
                groups.push((rounded, 0.0));
                groups.len() - 1
            };
        if point.start > map_end {
            continue;
        }
        let start = if index == 0 { 0.0 } else { point.start };
        let end = points.get(index + 1).map_or(map_end, |next| next.start);
        groups[group_index].1 += end - start;
    }
    let mut common = groups[0];
    for group in groups.iter().copied().skip(1) {
        if group.1 > common.1 {
            common = group;
        }
    }
    BpmSummary {
        common: 60_000.0 / common.0 * clock_rate,
        minimum: 60_000.0 / maximum_beat_len * clock_rate,
        maximum: 60_000.0 / minimum_beat_len * clock_rate,
    }
}

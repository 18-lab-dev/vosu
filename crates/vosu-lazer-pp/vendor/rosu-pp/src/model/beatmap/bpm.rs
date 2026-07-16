use std::collections::HashMap;

use crate::model::{control_point::TimingPoint, hit_object::HitObject};

pub fn bpm(last_hit_object: Option<&HitObject>, timing_points: &[TimingPoint]) -> f64 {
    // This is incorrect if the last object is a slider since there
    // is no reasonable way to get the slider end time at this point.
    let last_time = last_hit_object
        .map(HitObject::end_time)
        .or_else(|| timing_points.last().map(|t| t.time))
        .unwrap_or(0.0);

    let mut bpm_points = BeatLenDuration::new(last_time);

    // * osu-stable forced the first control point to start at 0.
    // * This is reproduced here to maintain compatibility around
    // * osu!mania scroll speed and song select display.
    match timing_points {
        [curr] => bpm_points.add(curr.beat_len, 0.0, last_time),
        [curr, next, ..] => bpm_points.add(curr.beat_len, 0.0, next.time),
        [] => {}
    }

    timing_points
        .iter()
        .skip(1)
        .zip(timing_points.iter().skip(2).map(|t| t.time))
        .for_each(|(curr, next_time)| bpm_points.add(curr.beat_len, curr.time, next_time));

    if let [.., _, curr] = timing_points {
        bpm_points.add(curr.beat_len, curr.time, last_time);
    }

    let most_common_beat_len = bpm_points
        .map
        .into_iter()
        // * Match LINQ's stable OrderByDescending: an equal-duration tie keeps
        // * the beat length whose rounded group appeared first.
        .max_by(|(_, (duration_a, first_a)), (_, (duration_b, first_b))| {
            duration_a
                .total_cmp(duration_b)
                .then_with(|| first_b.cmp(first_a))
        })
        .map_or(0.0, |(beat_len, _)| f64::from_bits(beat_len));

    if most_common_beat_len == 0.0 {
        return TimingPoint::DEFAULT_BPM;
    }

    // * Rounding can move the selected value just outside the raw timing
    // * range. Clamp it exactly as the pinned osu! Beatmap implementation.
    let (min_beat_len, max_beat_len) = timing_points
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), point| {
            (min.min(point.beat_len), max.max(point.beat_len))
        });
    let most_common_beat_len = most_common_beat_len.clamp(min_beat_len, max_beat_len);

    60_000.0 / most_common_beat_len
}

/// Maps `beat_len` to a cumulative duration
struct BeatLenDuration {
    last_time: f64,
    map: HashMap<u64, (f64, usize)>,
    next_order: usize,
}

impl BeatLenDuration {
    fn new(last_time: f64) -> Self {
        Self {
            last_time,
            map: HashMap::default(),
            next_order: 0,
        }
    }

    fn add(&mut self, beat_len: f64, curr_time: f64, next_time: f64) {
        let beat_len = (1000.0 * beat_len).round_ties_even() / 1000.0;
        let entry = self.map.entry(beat_len.to_bits()).or_insert_with(|| {
            let order = self.next_order;
            self.next_order += 1;
            (0.0, order)
        });

        if curr_time <= self.last_time {
            entry.0 += next_time - curr_time;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_common_beat_length_is_clamped_to_raw_timing_range() {
        let timing_points = [TimingPoint::new(0.0, 10.0004)];
        let actual = bpm(None, &timing_points);
        let expected = 60_000.0 / 10.0004;

        assert_eq!(actual, expected);
    }
}

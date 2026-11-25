use crate::{
    chart::{Bpm, ChartFacts, Difficulty, Objects, Times},
    dto::{MapBreak, MapTimingPoint},
    helpers::js_number_text,
};
use vosu_osu_calc::beatmap::{DifficultyValues, ParsedBeatmap, TimeRange};

/// Accurate kiai ranges are retained here; the pinned wire builder still emits [].
#[derive(Debug)]
pub struct DerivedChart {
    pub facts: ChartFacts,
    pub kiai: Vec<TimeRange>,
}
pub(crate) fn project(map: &ParsedBeatmap, audio: Option<f64>) -> DerivedChart {
    let graph = map.build_graph(audio, map.clock_rate());
    let data = graph.series.first().map_or(&[][..], |s| s.data.as_slice());
    let mut strains = indexmap::IndexMap::with_capacity(graph.x_axis.len());
    for (i, time) in graph.x_axis.iter().enumerate() {
        let value = data.get(i).copied().unwrap_or(f64::NAN);
        strains.insert(
            js_number_text(*time),
            if value <= 0.0 { 0.0 } else { value },
        );
    }
    fn array_index(key: &str) -> Option<u32> {
        if !matches!(key.as_bytes(), [b'0'] | [b'1'..=b'9', ..]) {
            return None;
        }
        key.parse::<u32>().ok().filter(|v| *v != u32::MAX)
    }
    strains.sort_by(|a, _, b, _| match (array_index(a), array_index(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let difficulty = |v: &DifficultyValues| Difficulty {
        approach_rate: v.approach_rate,
        overall_difficulty: v.overall_difficulty,
        circle_size: v.circle_size,
        drain_rate: v.drain_rate,
        ..Difficulty::default()
    };
    DerivedChart {
        facts: ChartFacts {
            original: difficulty(&map.engine.original_difficulty),
            converted: difficulty(&map.engine.converted_difficulty),
            difficulty: Difficulty {
                stars: map.engine.difficulty.stars,
                max_combo: map.engine.difficulty.max_combo,
                ..Difficulty::default()
            },
            bpm: Bpm {
                common: map.bpm.common,
                minimum: map.bpm.minimum,
                maximum: map.bpm.maximum,
            },
            times: Times {
                full: map.times.full,
                first_object: map.times.first_object,
                preview: map.times.preview,
            },
            objects: Objects {
                circles: map.object_counts.circles,
                sliders: map.object_counts.sliders,
                spinners: map.object_counts.spinners,
            },
            strains,
            breaks: map
                .breaks
                .iter()
                .map(|r| MapBreak {
                    start_time: r.start,
                    end_time: r.end,
                    has_effect: r.has_effect,
                })
                .collect(),
            timing_points: map
                .timing_points
                .iter()
                .map(|p| MapTimingPoint {
                    start_time: if p.start == 0.0 || p.start.is_nan() {
                        0.0
                    } else {
                        p.start
                    },
                    bpm: None,
                    beat_length: p.beat_length,
                })
                .collect(),
        },
        kiai: map.kiais.to_vec(),
    }
}

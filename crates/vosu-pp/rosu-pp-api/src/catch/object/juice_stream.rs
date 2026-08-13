use std::vec::Drain;

use rosu_map::section::{
    general::GameMode,
    hit_objects::{CurveBuffers, PathControlPoint, SliderEvent, SliderEventType, SliderEventsIter},
};

use crate::{
    Beatmap,
    catch::attributes::ObjectCountBuilder,
    model::{
        control_point::{DifficultyPoint, TimingPoint},
        hit_object::{Pos, Slider},
    },
    util::get_precision_adjusted_beat_len,
};

pub struct JuiceStream<'a> {
    pub control_points: &'a [PathControlPoint], // needed for applying hr offset
    pub nested_objects: Drain<'a, NestedJuiceStreamObject>,
}

impl<'a> JuiceStream<'a> {
    pub const BASE_SCORING_DIST: f64 = 100.0;

    pub fn new(
        effective_pos: Pos,
        start_time: f64,
        slider: &'a Slider,
        map: &Beatmap,
        count: &mut ObjectCountBuilder,
        bufs: &'a mut JuiceStreamBufs,
    ) -> Self {
        let slider_multiplier = map.slider_multiplier;
        let slider_tick_rate = map.slider_tick_rate;

        let beat_len = map
            .timing_point_at(start_time)
            .map_or(TimingPoint::DEFAULT_BEAT_LEN, |point| point.beat_len);

        let slider_velocity = map
            .difficulty_point_at(start_time)
            .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
                point.slider_velocity
            });

        let path = slider.borrowed_legacy_curve(GameMode::Catch, effective_pos, &mut bufs.curve);
        let velocity = JuiceStream::BASE_SCORING_DIST * slider_multiplier
            / get_precision_adjusted_beat_len(slider_velocity, beat_len);
        let scoring_dist = velocity * beat_len;

        let tick_dist_multiplier = if map.version < 8 {
            slider_velocity.recip()
        } else {
            1.0
        };

        let tick_dist = scoring_dist / slider_tick_rate * tick_dist_multiplier;

        let span_count = slider.span_count() as f64;
        let duration = span_count * path.dist() / velocity;
        let span_duration = duration / span_count;
        let legacy_start_time = start_time.trunc();
        let legacy_duration = (start_time + duration).trunc() - legacy_start_time;
        let events = SliderEventsIter::new(
            start_time,
            span_duration,
            velocity,
            tick_dist,
            path.dist(),
            slider.span_count() as i32,
            &mut bufs.ticks,
        );

        let mut last_event: Option<SliderEvent> = None;

        for e in events {
            if let Some(last_event) = last_event {
                let mut tiny_droplets = 0;
                let since_last_tick = e.time.trunc() - last_event.time.trunc();

                if since_last_tick > 80.0 {
                    let mut time_between_tiny = since_last_tick;
                    while time_between_tiny > 100.0 {
                        time_between_tiny /= 2.0;
                    }
                    let mut t = time_between_tiny;
                    while t < since_last_tick - 1.0 {
                        tiny_droplets += 1;
                        let nested_time = (t + last_event.time.trunc()).trunc();
                        let progress = legacy_path_progress(
                            nested_time,
                            legacy_start_time,
                            legacy_duration,
                            slider.span_count(),
                        );
                        bufs.nested_objects.push(NestedJuiceStreamObject {
                            pos: path.position_at(progress).x,
                            start_time: nested_time,
                            kind: NestedJuiceStreamObjectKind::TinyDroplet,
                        });
                        t += time_between_tiny;
                    }
                }
                count.record_tiny_droplets(tiny_droplets);
            }

            last_event = Some(e.clone());
            let kind = match e.kind {
                SliderEventType::Tick => {
                    count.record_droplet();
                    NestedJuiceStreamObjectKind::Droplet
                }
                SliderEventType::Head | SliderEventType::Repeat | SliderEventType::Tail => {
                    count.record_fruit();
                    NestedJuiceStreamObjectKind::Fruit
                }
                SliderEventType::LastTick => continue,
            };
            let nested_time = e.time.trunc();
            let progress = legacy_path_progress(
                nested_time,
                legacy_start_time,
                legacy_duration,
                slider.span_count(),
            );
            bufs.nested_objects.push(NestedJuiceStreamObject {
                pos: path.position_at(progress).x,
                start_time: nested_time,
                kind,
            });
        }

        Self {
            control_points: slider.control_points.as_ref(),
            nested_objects: bufs.nested_objects.drain(..),
        }
    }
}
/// Calculates the prepared end time of a catch juice stream.
///
/// Catch conversion uses the path's legacy osu!stable approximation together
/// with the active timing point and slider velocity. This is intentionally
/// shared with gradual source-boundary processing instead of relying on the
/// raw standard slider end time.
pub(crate) fn prepared_end_time(
    effective_pos: Pos,
    start_time: f64,
    slider: &Slider,
    map: &Beatmap,
    bufs: &mut CurveBuffers,
) -> f64 {
    let beat_len = map
        .timing_point_at(start_time)
        .map_or(TimingPoint::DEFAULT_BEAT_LEN, |point| point.beat_len);

    let slider_velocity = map
        .difficulty_point_at(start_time)
        .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
            point.slider_velocity
        });

    let path = slider.borrowed_legacy_curve(GameMode::Catch, effective_pos, bufs);
    let velocity = JuiceStream::BASE_SCORING_DIST * map.slider_multiplier
        / get_precision_adjusted_beat_len(slider_velocity, beat_len);

    start_time + slider.span_count() as f64 * path.dist() / velocity
}

#[derive(Debug)]
pub struct NestedJuiceStreamObject {
    pub pos: f32,
    pub start_time: f64,
    pub kind: NestedJuiceStreamObjectKind,
}

#[derive(Debug)]
pub enum NestedJuiceStreamObjectKind {
    Fruit,
    Droplet,
    TinyDroplet,
}

fn legacy_path_progress(time: f64, start_time: f64, duration: f64, span_count: usize) -> f64 {
    let span_progress = ((time - start_time) / duration).clamp(0.0, 1.0) * span_count as f64;
    let span_idx = span_progress.floor() as usize;

    if span_idx == span_count {
        return (span_count % 2) as f64;
    }

    let progress = span_progress - span_idx as f64;

    if span_idx % 2 == 0 {
        progress
    } else {
        1.0 - progress
    }
}

pub struct JuiceStreamBufs {
    pub nested_objects: Vec<NestedJuiceStreamObject>,
    pub curve: CurveBuffers,
    pub ticks: Vec<SliderEvent>,
}

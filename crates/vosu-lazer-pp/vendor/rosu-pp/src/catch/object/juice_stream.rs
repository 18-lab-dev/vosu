use std::vec::Drain;

use rosu_map::{
    section::{
        general::GameMode,
        hit_objects::{
            CurveBuffers, PathControlPoint, SliderEvent, SliderEventType, SliderEventsIter,
        },
    },
    util::Pos,
};

use crate::{
    Beatmap,
    catch::attributes::ObjectCountBuilder,
    model::{
        control_point::{DifficultyPoint, TimingPoint},
        hit_object::Slider,
    },
};

pub(crate) fn precision_adjusted_beat_len(slider_velocity: f64, beat_len: f64) -> f64 {
    let slider_velocity_as_beat_len = -100.0 / slider_velocity;
    let bpm_multiplier = if slider_velocity_as_beat_len < 0.0 {
        f64::from(((-slider_velocity_as_beat_len) as f32).clamp(10.0, 1_000.0)) / 100.0
    } else {
        1.0
    };

    beat_len * bpm_multiplier
}

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
            / precision_adjusted_beat_len(slider_velocity, beat_len);
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
                        let path_progress = legacy_path_progress(
                            nested_time,
                            legacy_start_time,
                            legacy_duration,
                            slider.span_count(),
                        );
                        let nested = NestedJuiceStreamObject {
                            pos: path.position_at(path_progress).x,
                            x_offset: 0.0,
                            start_time: nested_time,
                            kind: NestedJuiceStreamObjectKind::TinyDroplet,
                        };

                        bufs.nested_objects.push(nested);

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
            let nested = NestedJuiceStreamObject {
                pos: path
                    .position_at(legacy_path_progress(
                        nested_time,
                        legacy_start_time,
                        legacy_duration,
                        slider.span_count(),
                    ))
                    .x,
                x_offset: 0.0,
                start_time: nested_time,
                kind,
            };

            bufs.nested_objects.push(nested);
        }

        Self {
            control_points: slider.control_points.as_ref(),
            nested_objects: bufs.nested_objects.drain(..),
        }
    }
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

#[derive(Debug)]
pub struct NestedJuiceStreamObject {
    pub pos: f32,
    pub x_offset: f32,
    pub start_time: f64,
    pub kind: NestedJuiceStreamObjectKind,
}

#[derive(Debug)]
pub enum NestedJuiceStreamObjectKind {
    Fruit,
    Droplet,
    TinyDroplet,
}

pub struct JuiceStreamBufs {
    pub nested_objects: Vec<NestedJuiceStreamObject>,
    pub curve: CurveBuffers,
    pub ticks: Vec<SliderEvent>,
}

#[cfg(test)]
mod tests {
    use rosu_map::section::hit_objects::{SliderEventType, SliderEventsIter};

    use super::precision_adjusted_beat_len;

    #[test]
    fn precision_adjustment_uses_catch_clamp() {
        assert_eq!(precision_adjusted_beat_len(0.001, 500.0), 5_000.0);
        assert_eq!(precision_adjusted_beat_len(100.0, 500.0), 50.0);
    }

    #[test]
    fn perfect_curve_event_timing_preserves_fractional_duration() {
        let start_time = 19_184.0;
        let beat_len = 638.298_947_368_422;
        let velocity = 100.0 * 0.9 / precision_adjusted_beat_len(1.0, beat_len);
        let span_duration = 150.0 / velocity;
        let mut ticks = Vec::new();
        let events: Vec<_> = SliderEventsIter::new(
            start_time,
            span_duration,
            velocity,
            90.0,
            150.0,
            1,
            &mut ticks,
        )
        .collect();

        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            [
                SliderEventType::Head,
                SliderEventType::Tick,
                SliderEventType::LastTick,
                SliderEventType::Tail,
            ],
        );
        assert_eq!(events[1].time.trunc(), 19_822.0);
        assert_eq!(events[2].time.trunc(), 20_211.0);
        assert_eq!(events[3].time.trunc(), 20_247.0);

        let since_last_tick = f64::from(events[2].time as i32 - events[1].time as i32);
        let time_between_tiny = since_last_tick / 4.0;
        assert_eq!((events[1].time + 3.0 * time_between_tiny).trunc(), 20_114.0,);
    }
}

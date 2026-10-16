use crate::model::memory::{StableReplayFrame, StableReplayFrameBatch};
use crate::model::states::{KeyOverlayButton, KeypressButton};

/// Reducer-owned cursor: history eviction must not discard accumulated down-edges.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct StableKeys {
    cursor: i64,
    time: Option<f64>,
    identity: Option<(i64, usize)>,
}

fn mask(mode: i64, frame: &StableReplayFrame) -> u32 {
    let bits = frame.buttons;
    match mode {
        0 => {
            u32::from(bits & 5 != 0)
                | (u32::from(bits & 10 != 0) << 1)
                | (u32::from(bits & 16 != 0) << 2)
        }
        1 => {
            u32::from(bits & 2 != 0)
                | (u32::from(bits & 1 != 0) << 1)
                | (u32::from(bits & 4 != 0) << 2)
                | (u32::from(bits & 8 != 0) << 3)
        }
        // Stable does not record left/right actions. Do not invent presses from
        // displacement (held inputs at walls and opposing keys are unknowable).
        2 => u32::from(bits == 1) << 2,
        3 => frame.mania_keys(),
        _ => 0,
    }
}

impl StableKeys {
    pub(super) fn apply(
        &mut self,
        mode: i64,
        columns: usize,
        time: f64,
        batch: &StableReplayFrameBatch,
        keys: &mut Vec<KeypressButton>,
    ) {
        let count = match mode {
            0 | 2 => 3,
            1 => 4,
            3 => columns.min(20),
            _ => 0,
        };
        // A reset re-baselines from the retained window, so once history has been evicted
        // it permanently loses earlier counts; clock jitter must not trigger it.
        let rewound = self.time.is_some_and(|newest| {
            newest - time > crate::model::memory::StableReplayFrames::CLOCK_STEP_BACK_MS as f64
        });
        let reset = self.identity != Some((mode, count)) || rewound || batch.index < self.cursor;
        if reset {
            self.cursor = 0;
            keys.clear();
            keys.extend((0..count).map(|index| KeypressButton {
                name: KeyOverlayButton::lazer_name(index),
                is_pressed: false,
                count: 0,
                pressed_at: None,
                released_at: None,
            }));
        }
        self.identity = Some((mode, count));
        // Keep the newest clock across jitter, so a slow drift cannot hide a rewind.
        self.time = Some(match self.time {
            Some(newest) if !reset => newest.max(time),
            _ => time,
        });
        let expired = batch.frames_since(self.cursor).is_none();
        if expired {
            self.cursor = batch.start_index;
        }
        let mut baseline = expired;
        if let Some(frames) = batch.frames_since(self.cursor) {
            for frame in frames {
                if frame.time as f64 > time {
                    break;
                }
                self.cursor += 1;
                if frame.time == -12_345 && frame.y == -500.0 {
                    continue;
                }
                let state = mask(mode, frame);
                for (index, key) in keys.iter_mut().enumerate() {
                    let pressed = state & (1 << index) != 0;
                    if baseline {
                        key.is_pressed = pressed;
                    } else if key.is_pressed != pressed {
                        key.is_pressed = pressed;
                        if pressed {
                            key.count += 1;
                            key.pressed_at = Some(frame.time as f64);
                        } else {
                            key.released_at = Some(frame.time as f64);
                        }
                    }
                }
                baseline = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame(time: i64, bits: u8, columns: u32) -> StableReplayFrame {
        StableReplayFrame {
            time,
            buttons: bits,
            buttons_raw: u32::from(bits),
            x: columns as f32,
            y: 0.0,
        }
    }

    fn batch(start: i64, frames: Vec<StableReplayFrame>) -> StableReplayFrameBatch {
        StableReplayFrameBatch {
            start_index: start,
            index: start + frames.len() as i64,
            frames: Arc::new(frames.into()),
        }
    }

    #[test]
    fn ruleset_masks_match_legacy_conversion_and_smoke_is_included() {
        for (mode, bit, expected) in [
            (0, 1, 1),
            (0, 4, 1),
            (0, 2, 2),
            (0, 8, 2),
            (0, 16, 4),
            (1, 2, 1),
            (1, 1, 2),
            (1, 4, 4),
            (1, 8, 8),
            (2, 0, 0),
            (2, 1, 4),
            (2, 5, 0),
        ] {
            assert_eq!(mask(mode, &frame(0, bit, 0)), expected);
        }
        assert_eq!(mask(3, &frame(0, 0, 0x15555)), 0x15555);
    }

    #[test]
    fn all_mania_variants_have_ordered_names_and_columns() {
        for columns in (1..=10).chain((2..=20).step_by(2)) {
            let mut state = StableKeys::default();
            let mut keys = Vec::new();
            state.apply(
                3,
                columns,
                10.0,
                &batch(0, vec![frame(5, 0, (1 << columns) - 1)]),
                &mut keys,
            );
            assert_eq!(keys.len(), columns);
            for (i, key) in keys.iter().enumerate() {
                assert_eq!(key.name.as_ref(), format!("B{}", i + 1));
                assert!(key.is_pressed);
                assert_eq!(key.count, 1);
            }
        }
    }

    #[test]
    fn transitions_between_polls_are_counted_at_frame_time_and_future_is_not_used() {
        let frames = batch(
            0,
            vec![
                frame(10, 1, 0),
                frame(12, 0, 0),
                frame(15, 4, 0),
                frame(18, 0, 0),
                frame(30, 1, 0),
            ],
        );
        let mut state = StableKeys::default();
        let mut keys = Vec::new();
        state.apply(0, 0, 20.0, &frames, &mut keys);
        assert_eq!(
            (
                keys[0].count,
                keys[0].is_pressed,
                keys[0].pressed_at,
                keys[0].released_at
            ),
            (2, false, Some(15.0), Some(18.0))
        );
        state.apply(0, 0, 25.0, &frames, &mut keys);
        assert_eq!(keys[0].count, 2);
        state.apply(0, 0, 30.0, &frames, &mut keys);
        assert_eq!(keys[0].count, 3);
        // A rewind beyond clock jitter (retry, seek) recounts from the new clock.
        state.apply(0, 0, 1_100.0, &frames, &mut keys);
        state.apply(0, 0, 12.0, &frames, &mut keys);
        assert_eq!((keys[0].count, keys[0].released_at), (1, Some(12.0)));
        state.apply(0, 0, 40.0, &batch(0, vec![frame(35, 1, 0)]), &mut keys);
        assert_eq!(keys[0].count, 1); // retry shrank the list
    }

    #[test]
    fn expired_cursor_resynchronises_without_recounting_a_held_key() {
        let mut state = StableKeys::default();
        let mut keys = Vec::new();
        state.apply(0, 0, 10.0, &batch(0, vec![frame(5, 1, 0)]), &mut keys);
        let frames = batch(
            8192,
            vec![frame(20, 1, 0), frame(22, 0, 0), frame(25, 1, 0)],
        );
        state.apply(0, 0, 30.0, &frames, &mut keys);
        assert_eq!(
            (keys[0].count, keys[0].pressed_at, keys[0].released_at),
            (2, Some(25.0), Some(22.0))
        );
        state.apply(0, 0, 31.0, &frames, &mut keys);
        assert_eq!(keys[0].count, 2);
    }

    #[test]
    fn clock_jitter_keeps_counts_after_history_eviction() {
        let mut state = StableKeys::default();
        let mut keys = Vec::new();
        let early: Vec<_> = (0..40)
            .map(|index| frame(index * 10, (index % 2) as u8, 0))
            .collect();
        state.apply(0, 0, 400.0, &batch(0, early), &mut keys);
        assert_eq!(keys[0].count, 20);
        // The window has since evicted those frames; stable's clock then reads 1 ms back.
        let frames = batch(
            9_000,
            vec![frame(500, 1, 0), frame(510, 0, 0), frame(520, 1, 0)],
        );
        // The first retained frame is a baseline (the key was already held), not a press.
        state.apply(0, 0, 515.0, &frames, &mut keys);
        assert_eq!(keys[0].count, 20);
        state.apply(0, 0, 514.0, &frames, &mut keys);
        assert_eq!(keys[0].count, 20);
        state.apply(0, 0, 525.0, &frames, &mut keys);
        assert_eq!((keys[0].count, keys[0].pressed_at), (21, Some(520.0)));
    }
}

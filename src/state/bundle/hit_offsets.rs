use super::*;
use std::collections::VecDeque;

/// Replay evidence is bounded independently of the length of a play.
#[derive(Debug, Clone, Default)]
pub(super) struct StableHitOffsets {
    pub heads: Option<Vec<(f64, f32, f32)>>,
    judged: Vec<bool>,
    frame_index: i64,
    buttons: Option<u8>,
    presses: VecDeque<crate::model::memory::StableReplayFrame>,
    pending: VecDeque<(f64, Instant)>,
}

impl StableHitOffsets {
    pub fn update(
        &mut self,
        errors: &[f64],
        batch: Option<&StableReplayFrameBatch>,
        time: f64,
        now: Instant,
    ) -> Vec<f64> {
        if let Some(batch) = batch {
            if self.frame_index < batch.start_index {
                self.frame_index = batch.start_index;
                self.buttons = None; // Never invent a down edge across an expired cursor.
            }
            if let Some(frames) = batch.frames_since(self.frame_index) {
                for frame in frames {
                    if frame.time as f64 > time + 1.0 {
                        break;
                    }
                    if frame.time >= 0 {
                        if let Some(old) = self.buttons
                            && frame.x.is_finite()
                            && frame.y.is_finite()
                        {
                            // Keyboard replay flags include their mouse bit (5/10).
                            // Two independent actions in one frame can judge two heads.
                            let down = frame.buttons & !old;
                            for mask in [5, 10] {
                                if down & mask != 0 {
                                    self.presses.push_back(*frame);
                                }
                            }
                        }
                        self.buttons = Some(frame.buttons);
                    }
                    self.frame_index += 1;
                }
            }
        }
        while self.presses.len() > 8192 {
            self.presses.pop_front();
        }
        for &error in errors {
            if self.pending.len() == 256 {
                self.pending.pop_front();
            }
            self.pending.push_back((error, now));
        }
        let mut output = Vec::new();
        let Some(heads) = &self.heads else {
            self.pending
                .retain(|(_, at)| now.saturating_duration_since(*at) < Duration::from_millis(250));
            return output;
        };
        if self.judged.len() != heads.len() {
            self.judged.resize(heads.len(), false);
        }
        let mut remaining = VecDeque::new();
        while let Some((error, at)) = self.pending.pop_front() {
            let matched = self
                .presses
                .iter()
                .enumerate()
                .find_map(|(press_index, frame)| {
                    let target = frame.time as f64 - error;
                    let first = heads.partition_point(|head| head.0 < target - 1.0);
                    heads
                        .iter()
                        .enumerate()
                        .skip(first)
                        .take_while(|(_, head)| head.0 <= target + 1.0)
                        .find(|(index, _)| !self.judged[*index])
                        .map(|(index, head)| {
                            (
                                press_index,
                                index,
                                f64::from(frame.x - head.1),
                                f64::from(frame.y - head.2),
                            )
                        })
                });
            if let Some((press, object, x, y)) = matched {
                self.judged[object] = true;
                self.presses.drain(..=press);
                // A later judgement must not be blocked or reordered by missing evidence.
                remaining.clear();
                output.extend_from_slice(&[x, y, error]);
            } else if now.saturating_duration_since(at) < Duration::from_millis(250) {
                remaining.push_back((error, at));
            }
        }
        self.pending = remaining;
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::memory::{StableReplayFrame, StableReplayFrames};

    fn batch(frames: &[(i64, f32, f32, u8)]) -> StableReplayFrameBatch {
        let frames: Vec<_> = frames
            .iter()
            .map(|&(time, x, y, buttons)| StableReplayFrame {
                time,
                x,
                y,
                buttons,
                buttons_raw: u32::from(buttons),
            })
            .collect();
        StableReplayFrameBatch {
            start_index: 0,
            index: frames.len() as i64,
            frames: Arc::new(StableReplayFrames::from(frames)),
        }
    }

    #[test]
    fn circles_slider_head_notelock_gap_and_unmatched() {
        let mut matcher = StableHitOffsets {
            heads: Some(vec![
                (100.0, 50.0, 80.0),
                (200.0, 10.0, 20.0),
                (300.0, 70.0, 90.0),
            ]),
            ..Default::default()
        };
        let frames = batch(&[
            (0, 0.0, 0.0, 0),
            (105, 52.0, 77.0, 5),
            (110, 52.0, 77.0, 0),
            (295, 71.0, 94.0, 10),
        ]);
        assert_eq!(
            matcher.update(&[5.0, 999.0, -5.0], Some(&frames), 300.0, Instant::now()),
            [2.0, -3.0, 5.0, 1.0, 4.0, -5.0]
        );
        assert_eq!(matcher.judged, [true, false, true]);
    }

    #[test]
    fn replay_future_and_pending_evidence_are_bounded() {
        let now = Instant::now();
        let frames = batch(&[(0, 0.0, 0.0, 0), (105, 52.0, 77.0, 5)]);
        let mut matcher = StableHitOffsets {
            heads: Some(vec![(100.0, 50.0, 80.0)]),
            ..Default::default()
        };
        assert!(matcher.update(&[5.0], Some(&frames), 50.0, now).is_empty());
        assert_eq!(
            matcher.update(&[], Some(&frames), 105.0, now),
            [2.0, -3.0, 5.0]
        );
        matcher.update(&[123.0; 300], None, 105.0, now);
        assert_eq!(matcher.pending.len(), 256);
        matcher.update(&[], None, 105.0, now + Duration::from_millis(251));
        assert!(matcher.pending.is_empty());
    }

    #[test]
    fn calculator_stacking_and_hr_displayed_frame() {
        let source = "osu file format v14\n[General]\nMode:0\nStackLeniency:0.7\n[Difficulty]\nCircleSize:5\nApproachRate:5\nOverallDifficulty:5\n[HitObjects]\n100,100,100,1,0,0:0:0:0:\n100,100,200,1,0,0:0:0:0:\n300,200,400,2,0,L|350:200,1,50\n";
        let map = vosu_pp::LiveLazerEngine.parse_str(source).unwrap();
        let heads = map.osu_stacked_heads(0);
        assert_eq!(heads.len(), 3);
        assert!(heads[0].1 < 100.0 && heads[0].2 < 100.0);
        assert_eq!(heads[1], (200.0, 100.0, 100.0));
        assert_eq!(heads[2], (400.0, 300.0, 200.0));
        let hr = map.osu_stacked_heads(16);
        assert_eq!(hr[1], (200.0, 100.0, 284.0));
        // Stable's HR autoplay cursor showed the raw position is reflected and the
        // stack offset is then applied unreflected: stacks still lean up-left, by
        // the same amount on both axes.
        assert!(hr[0].1 < 100.0);
        assert!(((hr[0].2 - 284.0) - (hr[0].1 - 100.0)).abs() < 1e-3);
        let easy = map.osu_stacked_heads(2);
        assert!(
            easy[0].1 < heads[0].1,
            "Easy's larger circles change stacking scale"
        );
        let old = vosu_pp::LiveLazerEngine
            .parse_str(&source.replace("format v14", "format v5"))
            .unwrap();
        let old_heads = old.osu_stacked_heads(0);
        assert_eq!(old_heads.len(), 3);
        assert!(old_heads[0].1 < 100.0);
        let mut matcher = StableHitOffsets {
            heads: Some(hr),
            ..Default::default()
        };
        let frames = batch(&[(0, 0.0, 0.0, 0), (200, 103.0, 282.0, 1)]);
        assert_eq!(
            matcher.update(&[0.0], Some(&frames), 200.0, Instant::now()),
            [3.0, -2.0, 0.0]
        );
    }
}

#[cfg(test)]
mod ordering_tests {
    use super::*;
    #[test]
    fn simultaneous_heads_follow_map_order_and_missing_evidence_expires() {
        let now = Instant::now();
        let frames = vec![
            crate::model::memory::StableReplayFrame {
                time: 0,
                x: 0.0,
                y: 0.0,
                buttons: 0,
                buttons_raw: 0,
            },
            crate::model::memory::StableReplayFrame {
                time: 100,
                x: 50.0,
                y: 60.0,
                buttons: 15,
                buttons_raw: 15,
            },
        ];
        let batch = StableReplayFrameBatch {
            start_index: 0,
            index: 2,
            frames: Arc::new(frames.into()),
        };
        let mut matcher = StableHitOffsets {
            heads: Some(vec![(100.0, 49.0, 58.0), (100.0, 47.0, 56.0)]),
            ..Default::default()
        };
        assert_eq!(
            matcher.update(&[0.0, 0.0], Some(&batch), 100.0, now),
            [1.0, 2.0, 0.0, 3.0, 4.0, 0.0]
        );
        assert!(matcher.update(&[0.0], Some(&batch), 100.0, now).is_empty());
        matcher.update(&[], None, 100.0, now + Duration::from_secs(1));
        assert!(matcher.pending.is_empty());
    }
}

use std::collections::{HashMap, VecDeque, hash_map::Entry};

use super::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StableReplayTransition {
    time: i64,
    lane: u8,
    kind: ManiaJudgementKind,
}

type StableJudgementOrder = (i64, usize, u8);
type StableJudgementMatch = (usize, usize, ManiaJudgementKind, StableJudgementOrder);

const STABLE_MANIA_METADATA_MAX_FAILURES: u64 = 10;
const STABLE_MANIA_METADATA_MAX_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub(super) enum StableManiaMetadata {
    Pending,
    Ready(Vec<ManiaJudgementEvent>),
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Default)]
enum StableManiaMetadataState {
    #[default]
    Uninitialized,
    Pending {
        target_checksum: Option<String>,
        started_at: Instant,
        failure_baseline: u64,
    },
    Ready {
        target_checksum: Option<String>,
    },
    Unavailable {
        target_checksum: Option<String>,
    },
}

impl StableManiaMetadataState {
    fn target_checksum(&self) -> Option<Option<&str>> {
        match self {
            Self::Uninitialized => None,
            Self::Pending {
                target_checksum, ..
            }
            | Self::Ready { target_checksum }
            | Self::Unavailable { target_checksum } => Some(target_checksum.as_deref()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct StableManiaHitErrorState {
    metadata: StableManiaMetadataState,
    events: Vec<ManiaJudgementEvent>,
    used_events: Vec<bool>,
    transition_indexes: HashMap<StableReplayTransition, usize>,
    used_transitions: Vec<bool>,
    pending: VecDeque<f64>,
    replay_frame_index: i64,
    last_keys: u32,
    last_order: Option<StableJudgementOrder>,
}

impl StableManiaHitErrorState {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn needs_metadata(&self, target_checksum: Option<&str>) -> bool {
        match &self.metadata {
            StableManiaMetadataState::Uninitialized | StableManiaMetadataState::Pending { .. } => {
                true
            }
            StableManiaMetadataState::Ready {
                target_checksum: current,
            }
            | StableManiaMetadataState::Unavailable {
                target_checksum: current,
            } => current.as_deref() != target_checksum,
        }
    }

    fn apply_frames(&mut self, batch: &StableReplayFrameBatch) -> bool {
        let Some(expected_len) = batch.index.checked_sub(batch.start_index) else {
            return false;
        };
        if batch.start_index < 0
            || expected_len < 0
            || expected_len as usize != batch.frames.len()
            || batch.index < self.replay_frame_index
            || batch.start_index > self.replay_frame_index
        {
            return false;
        }
        let Some(offset) = self
            .replay_frame_index
            .checked_sub(batch.start_index)
            .map(|value| value as usize)
            .filter(|offset| *offset <= batch.frames.len())
        else {
            return false;
        };
        for frame in &batch.frames[offset..] {
            if frame.y == -500.0 {
                continue;
            }
            let keys = frame.keys & ((1 << 18) - 1);
            let changed = self.last_keys ^ keys;
            for lane in 0..18 {
                let mask = 1 << lane;
                if changed & mask == 0 {
                    continue;
                }
                let transition = StableReplayTransition {
                    time: frame.time,
                    lane,
                    kind: if keys & mask == 0 {
                        ManiaJudgementKind::Release
                    } else {
                        ManiaJudgementKind::Press
                    },
                };
                if let Entry::Vacant(entry) = self.transition_indexes.entry(transition) {
                    entry.insert(self.used_transitions.len());
                    self.used_transitions.push(false);
                }
            }
            self.last_keys = keys;
        }
        self.replay_frame_index = batch.index;
        true
    }

    fn prepare_target(
        &mut self,
        target_checksum: Option<&str>,
        calculation_failures: u64,
        now: Instant,
    ) {
        if self.metadata.target_checksum() == Some(target_checksum) {
            return;
        }
        let target_checksum = target_checksum.map(str::to_owned);
        let can_rebind = matches!(
            &self.metadata,
            StableManiaMetadataState::Uninitialized | StableManiaMetadataState::Pending { .. }
        ) || matches!(&self.metadata, StableManiaMetadataState::Ready { .. })
            && self.last_order.is_none();
        if can_rebind {
            self.events.clear();
            self.used_events.clear();
            self.last_order = None;
            self.metadata = StableManiaMetadataState::Pending {
                target_checksum,
                started_at: now,
                failure_baseline: calculation_failures,
            };
        } else {
            self.events.clear();
            self.used_events.clear();
            self.metadata = StableManiaMetadataState::Unavailable { target_checksum };
        }
    }

    fn disable(&mut self) {
        let target_checksum = self.metadata.target_checksum().flatten().map(str::to_owned);
        self.metadata = StableManiaMetadataState::Unavailable { target_checksum };
    }

    fn observe_metadata(
        &mut self,
        observation: StableManiaMetadata,
        calculation_failures: u64,
        now: Instant,
    ) {
        if matches!(
            &self.metadata,
            StableManiaMetadataState::Ready { .. } | StableManiaMetadataState::Unavailable { .. }
        ) {
            return;
        }
        match observation {
            StableManiaMetadata::Ready(events) => {
                let target_checksum = self.metadata.target_checksum().flatten().map(str::to_owned);
                self.used_events = vec![false; events.len()];
                self.events = events;
                self.metadata = StableManiaMetadataState::Ready { target_checksum };
            }
            StableManiaMetadata::Unavailable => self.disable(),
            StableManiaMetadata::Pending => {
                let exhausted = match &self.metadata {
                    StableManiaMetadataState::Pending {
                        started_at,
                        failure_baseline,
                        ..
                    } => {
                        now.saturating_duration_since(*started_at) >= STABLE_MANIA_METADATA_MAX_WAIT
                            || calculation_failures.saturating_sub(*failure_baseline)
                                > STABLE_MANIA_METADATA_MAX_FAILURES
                    }
                    StableManiaMetadataState::Uninitialized
                    | StableManiaMetadataState::Ready { .. }
                    | StableManiaMetadataState::Unavailable { .. } => false,
                };
                if exhausted {
                    self.disable();
                }
            }
        }
    }

    fn next_judgement(&self, value: f64) -> Result<Option<StableJudgementMatch>, ()> {
        if !value.is_finite() || value.fract() != 0.0 {
            return Err(());
        }
        let error = value as i64;
        let mut best = None;
        for (event_index, event) in self.events.iter().enumerate() {
            if self.used_events[event_index] {
                continue;
            }
            let Some(time) = event.target.checked_add(error) else {
                continue;
            };
            let transition = StableReplayTransition {
                time,
                lane: event.lane,
                kind: event.kind,
            };
            let Some(&transition_index) = self.transition_indexes.get(&transition) else {
                continue;
            };
            if self.used_transitions[transition_index] {
                continue;
            }
            let order = (
                time,
                event.object_index,
                u8::from(event.kind == ManiaJudgementKind::Release),
            );
            if self.last_order.is_some_and(|last| order < last)
                || best
                    .as_ref()
                    .is_some_and(|(_, _, _, best_order)| order >= *best_order)
            {
                continue;
            }
            best = Some((event_index, transition_index, event.kind, order));
        }
        Ok(best)
    }

    pub(super) fn partition(
        &mut self,
        values: &[f64],
        batch: &StableReplayFrameBatch,
        target_checksum: Option<&str>,
        calculation_failures: u64,
        metadata: Option<StableManiaMetadata>,
        now: Instant,
    ) -> (Vec<f64>, Vec<f64>) {
        self.prepare_target(target_checksum, calculation_failures, now);
        if !self.apply_frames(batch) {
            self.disable();
        }
        self.pending.extend(values.iter().copied());
        if let Some(metadata) = metadata {
            self.observe_metadata(metadata, calculation_failures, now);
        }

        let mut press = Vec::new();
        let mut release = Vec::new();
        if matches!(&self.metadata, StableManiaMetadataState::Unavailable { .. }) {
            press.extend(self.pending.drain(..));
            return (press, release);
        }
        if !matches!(&self.metadata, StableManiaMetadataState::Ready { .. }) {
            return (press, release);
        }
        while let Some(&value) = self.pending.front() {
            let judgement = match self.next_judgement(value) {
                Ok(Some(judgement)) => judgement,
                Ok(None) => break,
                Err(()) => {
                    self.disable();
                    press.extend(self.pending.drain(..));
                    break;
                }
            };
            let (event_index, transition_index, kind, order) = judgement;
            self.used_events[event_index] = true;
            self.used_transitions[transition_index] = true;
            self.last_order = Some(order);
            self.pending.pop_front();
            match kind {
                ManiaJudgementKind::Press => press.push(value),
                ManiaJudgementKind::Release => release.push(value),
            }
        }
        (press, release)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameplayState {
    pub is_default_state: bool,
    pub is_key_overlay_default_state: bool,
    pub failed: bool,
    pub retries: i64,
    pub player_name: String,
    pub mode: i64,
    pub keypress_mode: i64,
    pub mods: CalculateMods,
    pub score: f64,
    pub player_hp_smooth: f64,
    pub player_hp: f64,
    pub accuracy: f64,
    pub statistics: Statistics,
    pub maximum_statistics: Statistics,
    pub combo: i64,
    pub max_combo: i64,
    pub combo_previous: i64,
    pub hit_miss_previous: i64,
    pub slider_breaks: i64,
    pub unstable_rate: f64,
    pub grade_current: String,
    pub grade_expected: String,
    pub key_overlay: Vec<KeyOverlayButton>,
    pub keypresses: Vec<KeypressButton>,
    pub keypress_identity_mode: Option<i64>,
    pub hit_errors: HitErrorArray,
    pub total_hit_errors: f64,
    pub previous_hit_error_index: i64,
    pub previous_play_time: f64,
    pub leaderboard_visible: bool,
    pub leaderboard_player: LeaderboardPlayerState,
    pub leaderboard_scores: Vec<LeaderboardPlayer>,
    pub calculated: ProgressiveAttributes,
}

impl Default for GameplayState {
    fn default() -> Self {
        let statistics = default_statistics();
        Self {
            is_default_state: true,
            is_key_overlay_default_state: true,
            failed: false,
            retries: 0,
            player_name: String::new(),
            mode: 0,
            keypress_mode: 0,
            mods: default_calculated_mods(),
            score: 0.0,
            player_hp_smooth: 0.0,
            player_hp: 0.0,
            accuracy: 100.0,
            statistics: statistics.clone(),
            maximum_statistics: statistics,
            combo: 0,
            max_combo: 0,
            combo_previous: 0,
            hit_miss_previous: 0,
            slider_breaks: 0,
            unstable_rate: 0.0,
            grade_current: String::new(),
            grade_expected: String::new(),
            key_overlay: Vec::new(),
            keypresses: Vec::new(),
            keypress_identity_mode: None,
            hit_errors: HitErrorArray::default(),
            total_hit_errors: 0.0,
            previous_hit_error_index: 0,
            previous_play_time: 0.0,
            leaderboard_visible: false,
            leaderboard_player: LeaderboardPlayerState::default(),
            leaderboard_scores: Vec::new(),
            calculated: ProgressiveAttributes::default(),
        }
    }
}

impl GameplayState {
    pub(super) fn reset(&mut self, retry: bool, source: ClientType) {
        let grade_mode = self.mode;
        let grade_mods = self.mods.array.clone();
        let retained = retry.then(|| {
            (
                self.is_default_state,
                self.retries,
                self.player_name.clone(),
                self.mode,
                self.keypress_mode,
                self.mods.clone(),
                self.leaderboard_visible,
                self.leaderboard_player.clone(),
                self.leaderboard_scores.clone(),
            )
        });
        *self = Self::default();
        self.grade_current = calculate_grade(
            source == ClientType::Lazer,
            &grade_mods,
            grade_mode as i32,
            self.accuracy,
            &self.statistics,
        );
        self.grade_expected.clone_from(&self.grade_current);
        if let Some((
            is_default_state,
            retries,
            player_name,
            mode,
            keypress_mode,
            mods,
            visible,
            player,
            scores,
        )) = retained
        {
            self.is_default_state = is_default_state;
            self.retries = retries;
            self.player_name = player_name;
            self.mode = mode;
            self.keypress_mode = keypress_mode;
            self.mods = mods;
            self.leaderboard_visible = visible;
            self.leaderboard_player = player;
            self.leaderboard_scores = scores;
        }
    }

    pub(super) fn reset_quick(&mut self) {
        self.previous_play_time = 0.0;
        self.calculated = ProgressiveAttributes::default();
    }

    pub(super) fn reset_paused_regular(&mut self, score: Option<&Score>) {
        self.is_default_state = false;
        if let Some(score) = score {
            self.mode = score.mode;
            if !score.mods.array.is_empty() {
                self.mods.clone_from(&score.mods);
            }
        }
        self.failed = false;
        self.retries = 0;
        self.player_name.clear();
        self.score = 0.0;
        self.player_hp_smooth = 0.0;
        self.player_hp = 0.0;
        self.accuracy = 100.0;
        self.statistics = default_statistics();
        self.maximum_statistics = default_statistics();
        self.combo = 0;
        self.max_combo = 0;
        self.combo_previous = 0;
        self.hit_miss_previous = 0;
        self.slider_breaks = 0;
        self.grade_current.clear();
        self.grade_expected.clear();
        self.leaderboard_visible = false;
        self.leaderboard_player = LeaderboardPlayerState::default();
        self.leaderboard_scores.clear();
        self.calculated = ProgressiveAttributes::default();
    }

    pub(super) fn reset_hit_errors(&mut self) {
        self.hit_errors.all.clear();
        self.hit_errors.press.clear();
        self.hit_errors.release.clear();
        self.total_hit_errors = 0.0;
        self.previous_hit_error_index = 0;
    }

    pub(super) fn reset_keys(&mut self) {
        self.keypress_identity_mode = None;
        if self.is_key_overlay_default_state {
            return;
        }
        for key in &mut self.key_overlay {
            key.is_pressed = false;
            key.count = 0;
        }
        for key in &mut self.keypresses {
            key.is_pressed = false;
            key.count = 0;
            key.pressed_at = None;
            key.released_at = None;
        }
        self.is_key_overlay_default_state = true;
    }

    pub(super) fn apply_score(&mut self, result: &Score, source: ClientType, object_count: i64) {
        self.is_default_state = false;
        self.failed = result.failed;
        self.retries = result.retries;
        self.player_name.clone_from(&result.player_name);
        self.mods = result.mods.clone();
        self.mode = result.mode;
        self.keypress_mode = result.mode;
        self.score = result.score;
        self.player_hp_smooth = result.player_hp_smooth;
        self.player_hp = result.player_hp;
        self.accuracy = result.accuracy;
        self.statistics = result.statistics.clone();
        self.maximum_statistics = result.maximum_statistics.clone();
        self.combo = result.combo;
        self.max_combo = result.max_combo;
        if self.max_combo > 0 {
            self.update_unstable_rate();
        }
        if self.combo_previous > self.max_combo {
            self.combo_previous = 0;
        }
        if self.combo < self.combo_previous && self.statistics.miss == self.hit_miss_previous {
            self.slider_breaks += 1;
        }
        self.hit_miss_previous = self.statistics.miss;
        self.combo_previous = self.combo;
        self.grade_current = calculate_grade(
            source == ClientType::Lazer,
            &self.mods.array,
            self.mode as i32,
            self.accuracy,
            &self.statistics,
        );
        let mut expected = self.statistics.clone();
        expected.great += (object_count
            - expected.great
            - expected.good
            - expected.ok
            - expected.meh
            - expected.miss)
            .max(0);
        self.grade_expected = calculate_grade(
            source == ClientType::Lazer,
            &self.mods.array,
            self.mode as i32,
            self.accuracy,
            &expected,
        );
    }

    fn update_unstable_rate(&mut self) {
        let base = self.calculate_ur();
        self.unstable_rate = if self.mods.number & 64 == 64 {
            base / 1.5
        } else if self.mods.number & 256 == 256 {
            base * 1.33
        } else {
            base
        };
    }

    pub(super) fn calculate_ur(&self) -> f64 {
        if self.hit_errors.all.is_empty() {
            return 0.0;
        }
        let mean = self.total_hit_errors / self.hit_errors.all.len() as f64;
        let variance = self
            .hit_errors
            .all
            .iter()
            .map(|hit| (hit - mean).powi(2))
            .sum::<f64>()
            / self.hit_errors.all.len() as f64;
        variance.sqrt() * 10.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultScreenState {
    pub raw: RawResultScreen,
    pub grade: String,
    pub pp: f64,
    pub fc_pp: f64,
    pub previous_beatmap: String,
}

impl Default for ResultScreenState {
    fn default() -> Self {
        Self {
            raw: RawResultScreen {
                online_id: 0.0,
                player_name: String::new(),
                mods: default_calculated_mods(),
                mode: 0,
                accuracy: 0.0,
                max_combo: 0,
                score: 0.0,
                statistics: default_statistics(),
                maximum_statistics: default_statistics(),
                date: String::new(),
            },
            grade: String::new(),
            pp: 0.0,
            fc_pp: 0.0,
            previous_beatmap: String::new(),
        }
    }
}

impl ResultScreenState {
    pub(super) fn apply(&mut self, raw: &RawResultScreen, source: ClientType) {
        self.raw = raw.clone();
        self.grade = calculate_grade(
            source == ClientType::Lazer,
            &raw.mods.array,
            raw.mode as i32,
            raw.accuracy,
            &raw.statistics,
        );
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TourneyUserState {
    pub accuracy: f64,
    pub ranked_score: f64,
    pub play_count: i64,
    pub global_rank: i64,
    pub pp: f64,
    pub name: String,
    pub country: String,
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TourneyManagerState {
    pub is_default_state: bool,
    pub scoreboard: RawTourney,
    pub messages: Vec<TourneyManagerChatItem>,
    pub user: Option<TourneyUserState>,
}

impl Default for TourneyManagerState {
    fn default() -> Self {
        Self {
            is_default_state: true,
            scoreboard: RawTourney {
                ipc_state: 0,
                left_stars: 0,
                right_stars: 0,
                best_of: 0,
                stars_visible: false,
                score_visible: false,
                first_team_name: String::new(),
                second_team_name: String::new(),
                first_team_score: 0,
                second_team_score: 0,
            },
            messages: Vec::new(),
            user: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserState {
    pub profile: Option<RawUser>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LazerMultiSpectatingState {
    pub data: Option<LazerSpectator>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RankedPlayState {
    pub info: Option<RawRankedPlay>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegularState {
    pub global: GlobalState,
    pub settings: SettingsState,
    pub beatmap: BeatmapPPState,
    pub menu: MenuState,
    pub bass_density: BassDensityState,
    pub gameplay: GameplayState,
    pub result_screen: ResultScreenState,
    pub tourney: TourneyManagerState,
    pub user: UserState,
    pub lazer_multi_spectating: LazerMultiSpectatingState,
    pub ranked_play: RankedPlayState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreciseState {
    pub status: i64,
    pub play_time: f64,
    pub mode: i64,
    pub key_overlay: Arc<Vec<KeyOverlayButton>>,
    pub keypresses: Arc<Vec<KeypressButton>>,
    pub hit_errors: Arc<HitErrorArray>,
    pub total_hit_errors: f64,
    pub committed_hit_error_index: i64,
    pub unstable_rate: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateBundleSnapshot {
    pub source: ClientType,
    pub generation: u64,
    pub reset_epoch: u64,
    pub regular: Arc<RegularState>,
    pub precise: Arc<PreciseState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalculationPhase {
    Menu,
    Editor,
    Gameplay,
    Result,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCalculationKey {
    pub instance_generation: u64,
    pub reset_epoch: u64,
    pub checksum: String,
    pub mode: i64,
    pub mods_checksum: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::memory::StableReplayFrame;

    #[test]
    fn stable_mania_errors_wait_for_replay_transitions_and_partition_release_judgements() {
        let events = vec![
            ManiaJudgementEvent {
                target: 100,
                lane: 0,
                object_index: 0,
                kind: ManiaJudgementKind::Press,
            },
            ManiaJudgementEvent {
                target: 200,
                lane: 1,
                object_index: 1,
                kind: ManiaJudgementKind::Press,
            },
            ManiaJudgementEvent {
                target: 400,
                lane: 1,
                object_index: 1,
                kind: ManiaJudgementKind::Release,
            },
            ManiaJudgementEvent {
                target: 250,
                lane: 2,
                object_index: 2,
                kind: ManiaJudgementKind::Press,
            },
        ];
        let frames = vec![
            StableReplayFrame {
                time: -12_345,
                keys: 256,
                y: -500.0,
            },
            StableReplayFrame {
                time: 103,
                keys: 1,
                y: 0.0,
            },
            StableReplayFrame {
                time: 110,
                keys: 0,
                y: 0.0,
            },
            StableReplayFrame {
                time: 190,
                keys: 2,
                y: 0.0,
            },
            StableReplayFrame {
                time: 350,
                keys: 0,
                y: 0.0,
            },
        ];
        let mut state = StableManiaHitErrorState::default();
        let now = Instant::now();

        let first = state.partition(
            &[3.0, -10.0, -50.0],
            &StableReplayFrameBatch {
                start_index: 0,
                index: 4,
                frames: frames[..4].to_vec(),
            },
            Some("map"),
            0,
            Some(StableManiaMetadata::Pending),
            now,
        );
        assert_eq!(first, (Vec::new(), Vec::new()));

        let second = state.partition(
            &[],
            &StableReplayFrameBatch {
                start_index: 0,
                index: 5,
                frames,
            },
            Some("map"),
            0,
            Some(StableManiaMetadata::Ready(events)),
            now + STABLE_MANIA_METADATA_MAX_WAIT,
        );
        assert_eq!(second, (vec![3.0, -10.0], vec![-50.0]));
    }

    #[test]
    fn pending_metadata_falls_back_after_target_local_failure_limit() {
        let mut state = StableManiaHitErrorState::default();
        let now = Instant::now();
        let batch = StableReplayFrameBatch {
            start_index: 0,
            index: 0,
            frames: Vec::new(),
        };

        assert_eq!(
            state.partition(
                &[4.0],
                &batch,
                Some("map"),
                100,
                Some(StableManiaMetadata::Pending),
                now,
            ),
            (Vec::new(), Vec::new())
        );
        for failures in 101..=110 {
            assert_eq!(
                state.partition(
                    &[],
                    &batch,
                    Some("map"),
                    failures,
                    Some(StableManiaMetadata::Pending),
                    now,
                ),
                (Vec::new(), Vec::new())
            );
        }
        assert_eq!(
            state.partition(
                &[],
                &batch,
                Some("map"),
                111,
                Some(StableManiaMetadata::Pending),
                now,
            ),
            (vec![4.0], Vec::new())
        );
    }

    #[test]
    fn pending_metadata_falls_back_at_deadline_and_stays_press_only() {
        let mut state = StableManiaHitErrorState::default();
        let now = Instant::now();
        let batch = StableReplayFrameBatch {
            start_index: 0,
            index: 0,
            frames: Vec::new(),
        };

        assert_eq!(
            state.partition(
                &[5.0],
                &batch,
                Some("map"),
                0,
                Some(StableManiaMetadata::Pending),
                now,
            ),
            (Vec::new(), Vec::new())
        );
        assert_eq!(
            state.partition(
                &[],
                &batch,
                Some("map"),
                0,
                Some(StableManiaMetadata::Pending),
                now + STABLE_MANIA_METADATA_MAX_WAIT - Duration::from_millis(1),
            ),
            (Vec::new(), Vec::new())
        );
        assert_eq!(
            state.partition(
                &[],
                &batch,
                Some("map"),
                0,
                Some(StableManiaMetadata::Pending),
                now + STABLE_MANIA_METADATA_MAX_WAIT,
            ),
            (vec![5.0], Vec::new())
        );
        assert_eq!(
            state.partition(
                &[6.0],
                &batch,
                Some("map"),
                0,
                None,
                now + STABLE_MANIA_METADATA_MAX_WAIT,
            ),
            (vec![6.0], Vec::new())
        );
    }

    #[test]
    fn provisional_target_rebind_preserves_pending_errors_and_replay_stream() {
        let events = vec![
            ManiaJudgementEvent {
                target: 200,
                lane: 1,
                object_index: 0,
                kind: ManiaJudgementKind::Press,
            },
            ManiaJudgementEvent {
                target: 400,
                lane: 1,
                object_index: 0,
                kind: ManiaJudgementKind::Release,
            },
        ];
        let frames = vec![
            StableReplayFrame {
                time: 200,
                keys: 2,
                y: 0.0,
            },
            StableReplayFrame {
                time: 390,
                keys: 0,
                y: 0.0,
            },
        ];
        for provisional_target in [None, Some("old-map")] {
            let mut state = StableManiaHitErrorState::default();
            let now = Instant::now();
            assert_eq!(
                state.partition(
                    &[-10.0],
                    &StableReplayFrameBatch {
                        start_index: 0,
                        index: 2,
                        frames: frames.clone(),
                    },
                    provisional_target,
                    5,
                    Some(StableManiaMetadata::Pending),
                    now,
                ),
                (Vec::new(), Vec::new())
            );
            assert_eq!(
                state.partition(
                    &[],
                    &StableReplayFrameBatch {
                        start_index: 2,
                        index: 2,
                        frames: Vec::new(),
                    },
                    Some("current-map"),
                    0,
                    Some(StableManiaMetadata::Ready(events.clone())),
                    now,
                ),
                (Vec::new(), vec![-10.0])
            );
        }
    }
}

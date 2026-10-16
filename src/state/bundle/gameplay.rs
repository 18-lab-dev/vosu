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
/// Replay frames are read after the hit-error list in the same capture, so a judgement's key
/// transition is normally present when its error arrives. These bounds only stop an error the
/// map metadata cannot explain from withholding every later error from both arrays.
const STABLE_MANIA_MATCH_GRACE_MS: i64 = 250;
const STABLE_MANIA_MATCH_MAX_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq)]
struct PendingStableError {
    value: f64,
    replay_time: Option<i64>,
    enqueued_at: Instant,
}

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
    pending: VecDeque<PendingStableError>,
    replay_frame_index: i64,
    latest_replay_time: Option<i64>,
    last_keys: u32,
    last_time: Option<i64>,
    pub(super) key: Option<crate::model::states::KeyHitErrors>,
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
        {
            return false;
        }
        // Frames before the retained window are gone, for example after attaching late in
        // a long play. Their transitions cannot be recovered, so errors that needed them
        // fall back to presses through the usual grace bound; later errors still match.
        // The first retained frame is only a key-state baseline, not a set of transitions.
        let mut baseline = batch.start_index > self.replay_frame_index;
        if baseline {
            self.replay_frame_index = batch.start_index;
        }
        for frame in batch.frames_since(self.replay_frame_index).unwrap() {
            if frame.y == -500.0 {
                continue;
            }
            self.latest_replay_time = Some(
                self.latest_replay_time
                    .map_or(frame.time, |latest| latest.max(frame.time)),
            );
            let keys = frame.mania_keys() & ((1 << 18) - 1);
            if std::mem::take(&mut baseline) {
                self.last_keys = keys;
                continue;
            }
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
            && self.last_time.is_none();
        if can_rebind {
            self.events.clear();
            self.used_events.clear();
            self.last_time = None;
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
            // Stable appends every judgement of one input frame at that frame's time, but not
            // necessarily in object order (two holds released in one frame arrive tail-last
            // first), so only the judgement time is monotonic.
            if self.last_time.is_some_and(|last| time < last) {
                continue;
            }
            let order = (
                time,
                event.object_index,
                u8::from(event.kind == ManiaJudgementKind::Release),
            );
            if best
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
        let replay_time = self.latest_replay_time;
        self.pending
            .extend(values.iter().map(|&value| PendingStableError {
                value,
                replay_time,
                enqueued_at: now,
            }));
        if let Some(metadata) = metadata {
            self.observe_metadata(metadata, calculation_failures, now);
        }

        let mut press = Vec::new();
        let mut release = Vec::new();
        if matches!(&self.metadata, StableManiaMetadataState::Unavailable { .. }) {
            press.extend(self.pending.drain(..).map(|pending| pending.value));
            if let Some(key) = &mut self.key {
                key.press_columns.resize(press.len(), None);
            }
            return (press, release);
        }
        if !matches!(&self.metadata, StableManiaMetadataState::Ready { .. }) {
            return (press, release);
        }
        while let Some(&pending) = self.pending.front() {
            let judgement = match self.next_judgement(pending.value) {
                Ok(Some(judgement)) => judgement,
                Ok(None) => {
                    if !self.evidence_expired(&pending, now) {
                        break;
                    }
                    // The original array already holds this error; publishing it as a press
                    // keeps every later error flowing instead of withholding the rest of the play.
                    self.pending.pop_front();
                    press.push(pending.value);
                    if let Some(key) = &mut self.key {
                        key.append(pending.value, None, false);
                    }
                    continue;
                }
                Err(()) => {
                    self.disable();
                    press.extend(self.pending.drain(..).map(|pending| pending.value));
                    if let Some(key) = &mut self.key {
                        key.press_columns.resize(press.len(), None);
                    }
                    break;
                }
            };
            let (event_index, transition_index, kind, (time, ..)) = judgement;
            self.used_events[event_index] = true;
            self.used_transitions[transition_index] = true;
            self.last_time = Some(time);
            self.pending.pop_front();
            if let Some(key) = &mut self.key {
                key.append(
                    pending.value,
                    Some(self.events[event_index].lane as usize),
                    kind == ManiaJudgementKind::Release,
                );
            }
            match kind {
                ManiaJudgementKind::Press => press.push(pending.value),
                ManiaJudgementKind::Release => release.push(pending.value),
            }
        }
        (press, release)
    }

    /// Publishes pending errors whose wait has expired when no fresh gameplay capture reaches
    /// `partition` (results screen, unreadable hit-error list), so the bound holds after the
    /// play ends. It never admits new errors: those still require a fresh Play capture.
    pub(super) fn flush_expired(&mut self, now: Instant) -> Vec<f64> {
        if let StableManiaMetadataState::Pending { started_at, .. } = &self.metadata
            && now.saturating_duration_since(*started_at) >= STABLE_MANIA_METADATA_MAX_WAIT
        {
            self.disable();
        }
        match &self.metadata {
            StableManiaMetadataState::Unavailable { .. } => self
                .pending
                .drain(..)
                .map(|pending| pending.value)
                .collect(),
            StableManiaMetadataState::Ready { .. } => {
                let mut press = Vec::new();
                while let Some(&pending) = self.pending.front()
                    && self.evidence_expired(&pending, now)
                {
                    self.pending.pop_front();
                    press.push(pending.value);
                }
                press
            }
            StableManiaMetadataState::Uninitialized | StableManiaMetadataState::Pending { .. } => {
                Vec::new()
            }
        }
    }

    fn evidence_expired(&self, pending: &PendingStableError, now: Instant) -> bool {
        now.saturating_duration_since(pending.enqueued_at) >= STABLE_MANIA_MATCH_MAX_WAIT
            || pending
                .replay_time
                .zip(self.latest_replay_time)
                .is_some_and(|(enqueued, latest)| {
                    latest >= enqueued.saturating_add(STABLE_MANIA_MATCH_GRACE_MS)
                })
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
    pub catcher: Option<crate::model::memory::Catcher>,
    pub total_hit_errors: f64,
    pub total_hit_error_squares: f64,
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
            catcher: None,
            total_hit_errors: 0.0,
            total_hit_error_squares: 0.0,
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

    pub(super) fn reset_hit_errors(&mut self) {
        self.hit_errors.all.clear();
        self.hit_errors.press.clear();
        self.hit_errors.release.clear();
        self.hit_errors.key = None;
        self.hit_errors.offsets.clear();
        self.total_hit_errors = 0.0;
        self.total_hit_error_squares = 0.0;
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
        self.unstable_rate = self.calculate_ur() / self.mods.rate;
    }

    pub(super) fn calculate_ur(&self) -> f64 {
        let count = self.hit_errors.all.len();
        if count == 0 {
            return 0.0;
        }
        let average = self.total_hit_errors / count as f64;
        let variance = self.total_hit_error_squares / count as f64 - average * average;
        let variance = if variance.is_nan() {
            variance
        } else {
            variance.max(0.0)
        };
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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoomState {
    pub info: Option<RawLazerRoom>,
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
    pub room: RoomState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreciseState {
    pub status: i64,
    pub play_time: f64,
    pub mode: i64,
    pub key_overlay: Arc<Vec<KeyOverlayButton>>,
    pub keypresses: Arc<Vec<KeypressButton>>,
    pub hit_errors: Arc<HitErrorArray>,
    pub catcher: Option<crate::model::memory::Catcher>,
    pub total_hit_errors: f64,
    pub committed_hit_error_index: i64,
    pub unstable_rate: f64,
}

impl PreciseState {
    #[must_use]
    pub fn active_catcher(&self) -> Option<crate::model::memory::Catcher> {
        self.catcher
            .filter(|_| self.status == GameState::Play.value() as i64 && self.mode == 2)
    }
    pub(crate) fn hit_offsets(&self) -> &[f64] {
        if self.status == GameState::Play.value() as i64 && self.mode == 0 {
            &self.hit_errors.offsets
        } else {
            &[]
        }
    }
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
                x: 256.0,
                buttons: 0,
                buttons_raw: 0,
                y: -500.0,
            },
            StableReplayFrame {
                time: 103,
                x: 1.0,
                buttons: 0,
                buttons_raw: 0,
                y: 0.0,
            },
            StableReplayFrame {
                time: 110,
                x: 0.0,
                buttons: 0,
                buttons_raw: 0,
                y: 0.0,
            },
            StableReplayFrame {
                time: 190,
                x: 2.0,
                buttons: 0,
                buttons_raw: 0,
                y: 0.0,
            },
            StableReplayFrame {
                time: 350,
                x: 0.0,
                buttons: 0,
                buttons_raw: 0,
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
                frames: std::sync::Arc::new(frames[..4].to_vec().into()),
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
                frames: std::sync::Arc::new(frames.into()),
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
            frames: Default::default(),
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
            frames: Default::default(),
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
                x: 2.0,
                buttons: 0,
                buttons_raw: 0,
                y: 0.0,
            },
            StableReplayFrame {
                time: 390,
                x: 0.0,
                buttons: 0,
                buttons_raw: 0,
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
                        frames: std::sync::Arc::new(frames.clone().into()),
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
                        frames: Default::default(),
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

    fn hold(lane: u8, object_index: usize, start: i64, end: i64) -> [ManiaJudgementEvent; 2] {
        [
            ManiaJudgementEvent {
                target: start,
                lane,
                object_index,
                kind: ManiaJudgementKind::Press,
            },
            ManiaJudgementEvent {
                target: end,
                lane,
                object_index,
                kind: ManiaJudgementKind::Release,
            },
        ]
    }

    fn frames(keys: &[(i64, u32)]) -> StableReplayFrameBatch {
        StableReplayFrameBatch {
            start_index: 0,
            index: keys.len() as i64,
            frames: std::sync::Arc::new(
                keys.iter()
                    .map(|&(time, keys)| StableReplayFrame {
                        time,
                        x: keys as f32,
                        buttons: 0,
                        buttons_raw: 0,
                        y: 0.0,
                    })
                    .collect::<Vec<_>>()
                    .into(),
            ),
        }
    }

    /// Recorded from a live osu!stable chord of four holds released early: lanes 2 and 0
    /// release in separate frames at the same millisecond, and stable appends the lane-2
    /// tail (a later object) first. The previous object-order check withheld every error
    /// after it for the rest of the play.
    #[test]
    fn stable_same_millisecond_releases_classify_in_input_order() {
        let events = [
            hold(0, 0, 22_560, 22_960),
            hold(1, 1, 22_560, 23_060),
            hold(2, 2, 22_560, 23_260),
            hold(3, 3, 22_560, 22_860),
        ]
        .concat();
        let batch = frames(&[
            (22_505, 2),
            (22_506, 3),
            (22_515, 7),
            (22_516, 15),
            (22_528, 13),
            (22_536, 9),
            (22_536, 8),
            (22_539, 0),
        ]);
        let mut state = StableManiaHitErrorState {
            key: Some(Default::default()),
            ..Default::default()
        };
        assert_eq!(
            state.partition(
                &[-55.0, -54.0, -45.0, -44.0, -532.0, -724.0, -424.0, -321.0],
                &batch,
                Some("map"),
                0,
                Some(StableManiaMetadata::Ready(events)),
                Instant::now(),
            ),
            (
                vec![-55.0, -54.0, -45.0, -44.0],
                vec![-532.0, -724.0, -424.0, -321.0]
            )
        );
        let key = state.key.unwrap();
        assert_eq!(
            key.press,
            [vec![-54.0], vec![-55.0], vec![-45.0], vec![-44.0]]
        );
        assert_eq!(
            key.release,
            [vec![-424.0], vec![-532.0], vec![-724.0], vec![-321.0]]
        );
    }

    #[test]
    fn stable_key_errors_preserve_mirrored_displayed_lanes() {
        let mut state = StableManiaHitErrorState {
            key: Some(Default::default()),
            ..Default::default()
        };
        let events = hold(3, 0, 1_000, 1_400).to_vec();
        assert_eq!(
            state.partition(
                &[-5.0, 3.0],
                &frames(&[(995, 8), (1403, 0)]),
                Some("mirror"),
                0,
                Some(StableManiaMetadata::Ready(events)),
                Instant::now(),
            ),
            (vec![-5.0], vec![3.0])
        );
        let key = state.key.unwrap();
        assert_eq!(key.press, [vec![], vec![], vec![], vec![-5.0]]);
        assert_eq!(key.release, [vec![], vec![], vec![], vec![3.0]]);
    }

    #[test]
    fn unexplained_stable_error_is_published_as_press_without_withholding_later_errors() {
        let events = hold(0, 0, 1_000, 1_400).to_vec();
        let now = Instant::now();
        let mut state = StableManiaHitErrorState::default();
        // 77 matches no transition; -5 is the hold's press at 995.
        assert_eq!(
            state.partition(
                &[77.0, -5.0],
                &frames(&[(995, 1)]),
                Some("map"),
                0,
                Some(StableManiaMetadata::Ready(events.clone())),
                now,
            ),
            (Vec::new(), Vec::new())
        );
        // The replay stream passing the grace window settles the unexplained error.
        let mut advanced = frames(&[(995, 1), (1_244, 1), (1_245, 1)]);
        advanced.start_index = 0;
        assert_eq!(
            state.partition(&[], &advanced, Some("map"), 0, None, now),
            (vec![77.0, -5.0], Vec::new())
        );

        // Without replay progress (for example while paused), wall time settles it instead.
        let mut paused = StableManiaHitErrorState::default();
        let batch = frames(&[(995, 1)]);
        assert_eq!(
            paused.partition(
                &[77.0, -5.0],
                &batch,
                Some("map"),
                0,
                Some(StableManiaMetadata::Ready(events)),
                now,
            ),
            (Vec::new(), Vec::new())
        );
        let unchanged = StableReplayFrameBatch {
            start_index: 1,
            index: 1,
            frames: Default::default(),
        };
        assert_eq!(
            paused.partition(
                &[],
                &unchanged,
                Some("map"),
                0,
                None,
                now + STABLE_MANIA_MATCH_MAX_WAIT - Duration::from_millis(1),
            ),
            (Vec::new(), Vec::new())
        );
        assert_eq!(
            paused.partition(
                &[],
                &unchanged,
                Some("map"),
                0,
                None,
                now + STABLE_MANIA_MATCH_MAX_WAIT,
            ),
            (vec![77.0, -5.0], Vec::new())
        );
    }

    #[test]
    fn late_attach_after_frame_eviction_still_classifies_retained_transitions() {
        let mut events = hold(0, 0, 1_000, 1_400).to_vec();
        events.extend(hold(1, 1, 5_000, 5_400));
        // Attached after frames 0..50 left the retained window: the first hold's
        // transitions are gone; the window starts with both keys up.
        let mut window = frames(&[(4_000, 0), (4_995, 2), (5_404, 0), (5_700, 0)]);
        window.start_index = 50;
        window.index = 54;
        let now = Instant::now();
        let mut state = StableManiaHitErrorState::default();
        let first = state.partition(
            &[-3.0, 7.0, -5.0, 4.0],
            &window,
            Some("map"),
            0,
            Some(StableManiaMetadata::Ready(events)),
            now,
        );
        let settled = state.partition(
            &[],
            &window,
            Some("map"),
            0,
            None,
            now + STABLE_MANIA_MATCH_MAX_WAIT,
        );
        let press: Vec<_> = first.0.into_iter().chain(settled.0).collect();
        let release: Vec<_> = first.1.into_iter().chain(settled.1).collect();
        // The evicted hold falls back to presses; the retained one still splits.
        assert_eq!((press, release), (vec![-3.0, 7.0, -5.0], vec![4.0]));
    }
}

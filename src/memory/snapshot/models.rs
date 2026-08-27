use std::sync::Arc;

use crate::model::enums::ClientType;
use crate::model::memory::{
    AudioVelocityBase, Gameplay, Global, GlobalPrecise, HitErrors, KeyOverlay, LazerSpectator,
    Leaderboard, Menu, Mp3Length, RankedPlay, ResultScreen, Settings, Tourney, TourneyChat,
    TourneyUser, User,
};
use crate::model::states::CalculateMods;
use crate::process::{MemoryRead, ReadStatus};

/// A branch which may not have been reached by the source loop.
///
/// This is intentionally different from `MemoryRead::NotReady`: `None` means no
/// read was attempted, while `Some(Ok(NotReady))` records an attempted pending
/// read.
pub type ConditionalRead<T> = Option<MemoryRead<T>>;

/// Identity shared by all samples produced for one live process incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceEpoch {
    pub instance_generation: u64,
    pub reset_epoch: u64,
}

impl InstanceEpoch {
    #[must_use]
    pub const fn new(instance_generation: u64, reset_epoch: u64) -> Self {
        Self {
            instance_generation,
            reset_epoch,
        }
    }
}

/// Capture identity for one permit-driven regular read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegularCaptureContext {
    pub source: ClientType,
    pub epoch: InstanceEpoch,
    pub produced_sequence: u64,
    pub base_precise_global_sequence: u64,
}

impl RegularCaptureContext {
    #[must_use]
    pub const fn new(
        source: ClientType,
        instance_generation: u64,
        reset_epoch: u64,
        produced_sequence: u64,
    ) -> Self {
        Self {
            source,
            epoch: InstanceEpoch::new(instance_generation, reset_epoch),
            produced_sequence,
            base_precise_global_sequence: 0,
        }
    }

    #[must_use]
    pub const fn with_base_precise_global_sequence(mut self, sequence: u64) -> Self {
        self.base_precise_global_sequence = sequence;
        self
    }
}

/// Reducer state captured before a precise read starts.
///
/// `mods` is independently owned by this context. Once the enclosing result is
/// published through `Arc`, callers can only borrow it and cannot mutate its
/// `String`, `Vec`, or nested mod settings.
#[derive(Debug, Clone, PartialEq)]
pub struct PreciseCaptureContext {
    source: ClientType,
    epoch: InstanceEpoch,
    mode: i64,
    mods: Arc<CalculateMods>,
    start_index: i64,
    base_accepted_precise_sequence: u64,
}

impl PreciseCaptureContext {
    #[must_use]
    pub fn new(
        source: ClientType,
        instance_generation: u64,
        reset_epoch: u64,
        mode: i64,
        mods: CalculateMods,
        start_index: i64,
        base_accepted_precise_sequence: u64,
    ) -> Self {
        Self::new_shared(
            source,
            instance_generation,
            reset_epoch,
            mode,
            Arc::new(mods),
            start_index,
            base_accepted_precise_sequence,
        )
    }

    pub(crate) fn new_shared(
        source: ClientType,
        instance_generation: u64,
        reset_epoch: u64,
        mode: i64,
        mods: Arc<CalculateMods>,
        start_index: i64,
        base_accepted_precise_sequence: u64,
    ) -> Self {
        Self {
            source,
            epoch: InstanceEpoch::new(instance_generation, reset_epoch),
            mode,
            mods,
            start_index,
            base_accepted_precise_sequence,
        }
    }

    #[must_use]
    pub const fn source(&self) -> ClientType {
        self.source
    }

    #[must_use]
    pub const fn epoch(&self) -> InstanceEpoch {
        self.epoch
    }

    #[must_use]
    pub const fn mode(&self) -> i64 {
        self.mode
    }

    #[must_use]
    pub fn mods(&self) -> &CalculateMods {
        self.mods.as_ref()
    }

    pub(crate) fn shared_mods(&self) -> Arc<CalculateMods> {
        self.mods.clone()
    }

    #[must_use]
    pub const fn start_index(&self) -> i64 {
        self.start_index
    }

    #[must_use]
    pub const fn base_accepted_precise_sequence(&self) -> u64 {
        self.base_accepted_precise_sequence
    }

    /// Exact stale/drop check required before a reducer accepts a suffix.
    #[must_use]
    pub fn matches(
        &self,
        source: ClientType,
        epoch: InstanceEpoch,
        mode: i64,
        mods: &CalculateMods,
        committed_hit_error_index: i64,
        accepted_precise_sequence: u64,
    ) -> bool {
        self.source == source
            && self.epoch == epoch
            && self.mode == mode
            && self.mods.as_ref() == mods
            && self.start_index == committed_hit_error_index
            && self.base_accepted_precise_sequence == accepted_precise_sequence
    }
}

/// A complete regular raw-memory observation.
///
/// Only `global` is attempted before any early-continue gate in both producers.
/// Every later field is conditional on preceding readiness, status, folder,
/// tournament/spectating role, or client source. Fields are private so an
/// `Arc<RawRegularSnapshot>` is deeply immutable to consumers.
#[derive(Debug)]
pub struct RawRegularSnapshot {
    pub(super) context: RegularCaptureContext,
    pub(super) global: MemoryRead<Global>,
    pub(super) menu: ConditionalRead<Menu>,
    pub(super) settings: ConditionalRead<Settings>,
    pub(super) user: ConditionalRead<User>,
    pub(super) mp3_length: ConditionalRead<Mp3Length>,
    pub(super) result_screen: ConditionalRead<ResultScreen>,
    pub(super) gameplay: ConditionalRead<Gameplay>,
    pub(super) audio_velocity_base: ConditionalRead<AudioVelocityBase>,
    pub(super) leaderboard: ConditionalRead<Leaderboard>,
    pub(super) ranked_play: ConditionalRead<RankedPlay>,
    pub(super) tourney: ConditionalRead<Tourney>,
    pub(super) tourney_chat: ConditionalRead<TourneyChat>,
    pub(super) tourney_user: ConditionalRead<TourneyUser>,
    pub(super) lazer_spectating: ConditionalRead<LazerSpectator>,
}

impl RawRegularSnapshot {
    #[must_use]
    pub const fn context(&self) -> RegularCaptureContext {
        self.context
    }

    #[must_use = "the global memory read must be inspected for readiness or errors"]
    pub fn global(&self) -> &MemoryRead<Global> {
        &self.global
    }

    #[must_use]
    pub fn menu(&self) -> Option<&MemoryRead<Menu>> {
        self.menu.as_ref()
    }

    #[must_use]
    pub fn settings(&self) -> Option<&MemoryRead<Settings>> {
        self.settings.as_ref()
    }

    #[must_use]
    pub fn user(&self) -> Option<&MemoryRead<User>> {
        self.user.as_ref()
    }

    #[must_use]
    pub fn mp3_length(&self) -> Option<&MemoryRead<Mp3Length>> {
        self.mp3_length.as_ref()
    }

    #[must_use]
    pub fn result_screen(&self) -> Option<&MemoryRead<ResultScreen>> {
        self.result_screen.as_ref()
    }

    #[must_use]
    pub fn gameplay(&self) -> Option<&MemoryRead<Gameplay>> {
        self.gameplay.as_ref()
    }

    #[must_use]
    pub fn audio_velocity_base(&self) -> Option<&MemoryRead<AudioVelocityBase>> {
        self.audio_velocity_base.as_ref()
    }

    #[must_use]
    pub fn leaderboard(&self) -> Option<&MemoryRead<Leaderboard>> {
        self.leaderboard.as_ref()
    }

    #[must_use]
    pub fn ranked_play(&self) -> Option<&MemoryRead<RankedPlay>> {
        self.ranked_play.as_ref()
    }

    #[must_use]
    pub fn tourney(&self) -> Option<&MemoryRead<Tourney>> {
        self.tourney.as_ref()
    }

    #[must_use]
    pub fn tourney_chat(&self) -> Option<&MemoryRead<TourneyChat>> {
        self.tourney_chat.as_ref()
    }

    #[must_use]
    pub fn tourney_user(&self) -> Option<&MemoryRead<TourneyUser>> {
        self.tourney_user.as_ref()
    }

    #[must_use]
    pub fn lazer_spectating(&self) -> Option<&MemoryRead<LazerSpectator>> {
        self.lazer_spectating.as_ref()
    }
}

/// Why a precise result is not admissible even before comparing it with the
/// reducer's current context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreciseSequenceError {
    ProducedSequenceNotNewer,
    EndingIndexBeforeStart,
    HitErrorEndingIndexMismatch,
    HitErrorSuffixExceedsConsumedRange,
}

/// Complete latest-only precise observation and its unacknowledged suffix base.
#[derive(Debug)]
pub struct RawPreciseSnapshot {
    pub(super) capture: Arc<PreciseCaptureContext>,
    pub(super) produced_sequence: u64,
    pub(super) ending_index: i64,
    pub(super) global: MemoryRead<GlobalPrecise>,
    pub(super) key_overlay: ConditionalRead<KeyOverlay>,
    pub(super) hit_errors: ConditionalRead<HitErrors>,
}

impl RawPreciseSnapshot {
    #[must_use]
    pub fn capture(&self) -> &PreciseCaptureContext {
        self.capture.as_ref()
    }

    #[must_use]
    pub const fn produced_sequence(&self) -> u64 {
        self.produced_sequence
    }

    #[must_use]
    pub const fn ending_index(&self) -> i64 {
        self.ending_index
    }

    #[must_use = "the global memory read must be inspected for readiness or errors"]
    pub fn global(&self) -> &MemoryRead<GlobalPrecise> {
        &self.global
    }

    #[must_use]
    pub fn key_overlay(&self) -> Option<&MemoryRead<KeyOverlay>> {
        self.key_overlay.as_ref()
    }

    /// Complete suffix read from `capture.start_index()` through
    /// `ending_index()`. An error/status is retained without advancing either
    /// end at the reducer.
    #[must_use]
    pub fn hit_errors(&self) -> Option<&MemoryRead<HitErrors>> {
        self.hit_errors.as_ref()
    }

    /// Structural validation independent of the reducer's current state.
    pub fn validate_sequence(&self) -> Result<(), PreciseSequenceError> {
        if self.produced_sequence <= self.capture.base_accepted_precise_sequence {
            return Err(PreciseSequenceError::ProducedSequenceNotNewer);
        }
        if let Some(Ok(ReadStatus::Ready(suffix))) = &self.hit_errors {
            if suffix.index != self.ending_index {
                return Err(PreciseSequenceError::HitErrorEndingIndexMismatch);
            }
            if self.ending_index < self.capture.start_index {
                if !suffix.errors.all.is_empty() {
                    return Err(PreciseSequenceError::EndingIndexBeforeStart);
                }
            } else {
                let consumed = usize::try_from(self.ending_index - self.capture.start_index)
                    .map_err(|_| PreciseSequenceError::HitErrorSuffixExceedsConsumedRange)?;
                if suffix.errors.all.len() > consumed {
                    return Err(PreciseSequenceError::HitErrorSuffixExceedsConsumedRange);
                }
            }
        } else if self.ending_index < self.capture.start_index {
            return Err(PreciseSequenceError::EndingIndexBeforeStart);
        }
        Ok(())
    }

    /// Full reducer acceptance predicate. Context mismatch and malformed/stale
    /// sequence both reject without advancing the acknowledged sequence/index.
    #[must_use]
    pub fn can_accept(
        &self,
        source: ClientType,
        epoch: InstanceEpoch,
        mode: i64,
        mods: &CalculateMods,
        committed_hit_error_index: i64,
        accepted_precise_sequence: u64,
    ) -> bool {
        self.validate_sequence().is_ok()
            && self.capture.matches(
                source,
                epoch,
                mode,
                mods,
                committed_hit_error_index,
                accepted_precise_sequence,
            )
    }
}

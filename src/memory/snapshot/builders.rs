use std::sync::Arc;

use crate::model::memory::{
    AudioVelocityBase, Gameplay, Global, GlobalPrecise, HitErrors, KeyOverlay, LazerRoom,
    LazerSpectator, Leaderboard, Menu, Mp3Length, RankedPlay, ResultScreen, Settings, Tourney,
    TourneyChat, TourneyUser, User,
};
use crate::process::{MemoryRead, ReadStatus};

use super::models::{
    PreciseCaptureContext, RawPreciseSnapshot, RawRegularSnapshot, RegularCaptureContext,
};

/// Consuming builder for one regular snapshot. It never exposes the mutable
/// snapshot and publishes directly as `Arc`.
#[derive(Debug)]
pub struct RawRegularSnapshotBuilder {
    snapshot: RawRegularSnapshot,
}

impl RawRegularSnapshotBuilder {
    #[must_use]
    pub fn new(context: RegularCaptureContext, global: MemoryRead<Global>) -> Self {
        Self {
            snapshot: RawRegularSnapshot {
                context,
                global,
                menu: None,
                settings: None,
                user: None,
                mp3_length: None,
                result_screen: None,
                gameplay: None,
                audio_velocity_base: None,
                leaderboard: None,
                ranked_play: None,
                room: None,
                tourney: None,
                tourney_chat: None,
                tourney_user: None,
                lazer_spectating: None,
            },
        }
    }

    #[must_use]
    pub fn menu(mut self, read: MemoryRead<Menu>) -> Self {
        self.snapshot.menu = Some(read);
        self
    }

    #[must_use]
    pub fn settings(mut self, read: MemoryRead<Settings>) -> Self {
        self.snapshot.settings = Some(read);
        self
    }

    #[must_use]
    pub fn user(mut self, read: MemoryRead<User>) -> Self {
        self.snapshot.user = Some(read);
        self
    }

    #[must_use]
    pub fn mp3_length(mut self, read: MemoryRead<Mp3Length>) -> Self {
        self.snapshot.mp3_length = Some(read);
        self
    }

    #[must_use]
    pub fn result_screen(mut self, read: MemoryRead<ResultScreen>) -> Self {
        self.snapshot.result_screen = Some(read);
        self
    }

    #[must_use]
    pub fn gameplay(mut self, read: MemoryRead<Gameplay>) -> Self {
        self.snapshot.gameplay = Some(read);
        self
    }

    #[must_use]
    pub fn audio_velocity_base(mut self, read: MemoryRead<AudioVelocityBase>) -> Self {
        self.snapshot.audio_velocity_base = Some(read);
        self
    }

    #[must_use]
    pub fn leaderboard(mut self, read: MemoryRead<Leaderboard>) -> Self {
        self.snapshot.leaderboard = Some(read);
        self
    }

    #[must_use]
    pub fn ranked_play(mut self, read: MemoryRead<RankedPlay>) -> Self {
        self.snapshot.ranked_play = Some(read);
        self
    }

    #[must_use]
    pub fn tourney(mut self, read: MemoryRead<Tourney>) -> Self {
        self.snapshot.tourney = Some(read);
        self
    }

    #[must_use]
    pub fn tourney_chat(mut self, read: MemoryRead<TourneyChat>) -> Self {
        self.snapshot.tourney_chat = Some(read);
        self
    }

    #[must_use]
    pub fn tourney_user(mut self, read: MemoryRead<TourneyUser>) -> Self {
        self.snapshot.tourney_user = Some(read);
        self
    }

    #[must_use]
    pub fn lazer_spectating(mut self, read: MemoryRead<LazerSpectator>) -> Self {
        self.snapshot.lazer_spectating = Some(read);
        self
    }
    #[must_use]
    pub fn room(mut self, read: MemoryRead<LazerRoom>) -> Self {
        self.snapshot.room = Some(read);
        self
    }

    #[must_use]
    pub fn publish(self) -> Arc<RawRegularSnapshot> {
        Arc::new(self.snapshot)
    }
}

/// Consuming precise builder. `ending_index` advances to a successful suffix's
/// reported next index and otherwise remains at the captured start.
#[derive(Debug)]
pub struct RawPreciseSnapshotBuilder {
    snapshot: RawPreciseSnapshot,
}

impl RawPreciseSnapshotBuilder {
    #[must_use]
    pub fn new(
        capture: PreciseCaptureContext,
        produced_sequence: u64,
        global: MemoryRead<GlobalPrecise>,
    ) -> Self {
        Self::new_shared(Arc::new(capture), produced_sequence, global)
    }

    pub(crate) fn new_shared(
        capture: Arc<PreciseCaptureContext>,
        produced_sequence: u64,
        global: MemoryRead<GlobalPrecise>,
    ) -> Self {
        let ending_index = capture.start_index();
        Self {
            snapshot: RawPreciseSnapshot {
                capture,
                produced_sequence,
                ending_index,
                global,
                key_overlay: None,
                hit_errors: None,
                catcher: None,
            },
        }
    }

    #[must_use]
    pub fn key_overlay(mut self, read: MemoryRead<KeyOverlay>) -> Self {
        self.snapshot.key_overlay = Some(read);
        self
    }

    #[must_use]
    pub fn catcher(mut self, read: MemoryRead<crate::model::memory::Catcher>) -> Self {
        self.snapshot.catcher = Some(read);
        self
    }

    #[must_use]
    pub fn hit_errors(mut self, read: MemoryRead<HitErrors>) -> Self {
        if let Ok(ReadStatus::Ready(suffix)) = &read {
            self.snapshot.ending_index = suffix.index;
        }
        self.snapshot.hit_errors = Some(read);
        self
    }

    #[must_use]
    pub fn publish(self) -> Arc<RawPreciseSnapshot> {
        Arc::new(self.snapshot)
    }
}

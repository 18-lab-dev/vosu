use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use super::{
    PreciseSnapshotReader, ReadContext, RegularCaptureRequest, RegularSnapshotReader,
    StateWorkerError,
};
use crate::memory::snapshot::{
    PreciseCaptureContext, RawPreciseSnapshot, RawPreciseSnapshotBuilder, RawRegularSnapshot,
    RawRegularSnapshotBuilder,
};
use crate::memory::{ReaderSynchronization, SynchronizedMemoryReader};
use crate::model::enums::{ClientType, GameState};

/// Production raw capture adapter. It preserves source ordering while retaining
/// every MemoryRead status in the immutable snapshot.
pub struct SourceRegularReader<R> {
    source: ClientType,
    reader: R,
    synchronization: Arc<Mutex<ReaderSynchronization>>,
}

impl<R> SourceRegularReader<R> {
    pub fn new(
        source: ClientType,
        reader: R,
        synchronization: Arc<Mutex<ReaderSynchronization>>,
    ) -> Self {
        Self {
            source,
            reader,
            synchronization,
        }
    }
}

impl<R: SynchronizedMemoryReader + 'static> RegularSnapshotReader for SourceRegularReader<R> {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError> {
        let plan = &request.plan;
        self.reader
            .synchronize_capture_config(plan.show_mp_commands, plan.read_mania_scroll_speed);
        let global = self.reader.global();
        let global_not_ready = matches!(global, Ok(crate::process::ReadStatus::NotReady));
        let snapshot = RawRegularSnapshotBuilder::new(request.context, global);
        if global_not_ready && self.source == ClientType::Lazer {
            *self.synchronization.lock().map_err(|_| {
                StateWorkerError::Capture("reader synchronization lock poisoned".into())
            })? = self.reader.reader_synchronization();
            return Ok(snapshot.publish());
        }

        let menu = self.reader.menu(&plan.previous_checksum);
        let menu_not_ready = matches!(menu, Ok(crate::process::ReadStatus::NotReady));
        let fresh_folder = match &menu {
            Ok(crate::process::ReadStatus::Ready(crate::model::memory::Menu::Full(value))) => {
                Some(!value.folder.is_empty())
            }
            _ => None,
        };
        let menu_keeps_first_object_time = match &menu {
            Ok(crate::process::ReadStatus::Ready(crate::model::memory::Menu::Full(value))) => {
                value.checksum == plan.previous_checksum
            }
            _ => true,
        };
        let mut snapshot = snapshot.menu(menu);
        if menu_not_ready {
            *self.synchronization.lock().map_err(|_| {
                StateWorkerError::Capture("reader synchronization lock poisoned".into())
            })? = self.reader.reader_synchronization();
            return Ok(snapshot.publish());
        }

        let (status, play_time) = (plan.status, plan.gameplay_time);
        if fresh_folder.unwrap_or(plan.read_mp3_length) || self.source == ClientType::Lazer {
            snapshot = snapshot.mp3_length(self.reader.mp3_length());
        }
        if status == GameState::ResultScreen.value() as i64 {
            let result = self.reader.result_screen();
            let result_not_ready = matches!(result, Ok(crate::process::ReadStatus::NotReady));
            snapshot = snapshot.result_screen(result);
            if result_not_ready {
                *self.synchronization.lock().map_err(|_| {
                    StateWorkerError::Capture("reader synchronization lock poisoned".into())
                })? = self.reader.reader_synchronization();
                return Ok(snapshot.publish());
            }
        }
        if self.source == ClientType::Lazer {
            snapshot = snapshot.ranked_play(self.reader.ranked_play());
            snapshot = snapshot.room(self.reader.room());
        }
        snapshot = snapshot.settings(self.reader.settings());

        if status == GameState::Play.value() as i64 {
            let first_object_time =
                if self.source == ClientType::Stable && menu_keeps_first_object_time {
                    plan.first_object_time
                } else {
                    None
                };
            let gameplay = self.reader.gameplay(play_time, first_object_time);
            let gameplay_mode = match &gameplay {
                Ok(crate::process::ReadStatus::Ready(value)) => Some(value.mode),
                _ => None,
            };
            snapshot = snapshot.gameplay(gameplay);
            if let Some(mode) = gameplay_mode {
                snapshot = snapshot.leaderboard(self.reader.leaderboard(mode));
            }
        } else if status == GameState::Menu.value() as i64 {
            snapshot = snapshot.audio_velocity_base(self.reader.audio_velocity_base());
        } else if status == GameState::Tourney.value() as i64 && self.source == ClientType::Stable {
            let tourney = self.reader.tourney();
            let tourney_ready = matches!(tourney, Ok(crate::process::ReadStatus::Ready(_)));
            snapshot = snapshot.tourney(tourney);
            if tourney_ready {
                snapshot = snapshot.tourney_chat(self.reader.tourney_chat(&plan.tourney_messages));
            }
        } else if status == GameState::Lobby.value() as i64 && self.source == ClientType::Lazer {
            snapshot =
                snapshot.lazer_spectating(self.reader.spectating_data(plan.show_mp_commands));
        }
        if plan.read_tourney_user && self.source == ClientType::Stable {
            snapshot = snapshot.tourney_user(self.reader.tourney_user());
        }
        snapshot = snapshot.user(self.reader.user());

        *self.synchronization.lock().map_err(|_| {
            StateWorkerError::Capture("reader synchronization lock poisoned".into())
        })? = self.reader.reader_synchronization();
        Ok(snapshot.publish())
    }
}

pub enum KeyOverlayUpdates {
    Direct(watch::Receiver<bool>),
    Config(watch::Receiver<Arc<crate::config::Config>>),
}

impl From<watch::Receiver<bool>> for KeyOverlayUpdates {
    fn from(value: watch::Receiver<bool>) -> Self {
        Self::Direct(value)
    }
}

impl From<watch::Receiver<Arc<crate::config::Config>>> for KeyOverlayUpdates {
    fn from(value: watch::Receiver<Arc<crate::config::Config>>) -> Self {
        Self::Config(value)
    }
}

impl KeyOverlayUpdates {
    fn current(&self) -> bool {
        match self {
            Self::Direct(value) => *value.borrow(),
            Self::Config(value) => value.borrow().enable_key_overlay,
        }
    }

    fn mania_key_hit_errors(&self) -> bool {
        match self {
            Self::Direct(_) => false,
            Self::Config(value) => value.borrow().enable_mania_key_hit_errors,
        }
    }

    fn hit_offsets(&self) -> bool {
        match self {
            Self::Direct(_) => false,
            Self::Config(value) => value.borrow().enable_hit_offsets,
        }
    }
}

pub struct SourcePreciseReader<R> {
    pub(super) reader: R,
    enable_key_overlay: KeyOverlayUpdates,
    capture_template: Option<Arc<PreciseCaptureContext>>,
}

impl<R> SourcePreciseReader<R> {
    pub fn new(reader: R, enable_key_overlay: impl Into<KeyOverlayUpdates>) -> Self {
        Self {
            reader,
            enable_key_overlay: enable_key_overlay.into(),
            capture_template: None,
        }
    }

    fn capture_context(&mut self, context: &ReadContext) -> Arc<PreciseCaptureContext> {
        if let Some(capture) = &self.capture_template
            && capture.matches(
                context.source,
                context.epoch(),
                context.mode,
                &context.mods,
                context.committed_hit_error_index,
                context.accepted_precise_sequence,
            )
        {
            return capture.clone();
        }

        let mods = self
            .capture_template
            .as_ref()
            .filter(|capture| capture.mods() == &context.mods)
            .map(|capture| capture.shared_mods())
            .unwrap_or_else(|| Arc::new(context.mods.clone()));
        let capture = Arc::new(PreciseCaptureContext::new_shared(
            context.source,
            context.instance_generation,
            context.reset_epoch,
            context.mode,
            mods,
            context.committed_hit_error_index,
            context.accepted_precise_sequence,
        ));
        self.capture_template = Some(capture.clone());
        capture
    }
}

impl<R: SynchronizedMemoryReader + 'static> PreciseSnapshotReader for SourcePreciseReader<R> {
    fn capture_precise(
        &mut self,
        context: &ReadContext,
        produced_sequence: u64,
        synchronization: ReaderSynchronization,
    ) -> Result<Arc<RawPreciseSnapshot>, StateWorkerError> {
        self.reader
            .synchronize_precise(synchronization, context.mode);
        let global = self.reader.global_precise();
        let playing_time = match &global {
            Ok(crate::process::ReadStatus::Ready(value))
                if value.status == GameState::Play.value() as i64 =>
            {
                Some(value.time)
            }
            _ => None,
        };
        let capture = self.capture_context(context);
        let mut snapshot =
            RawPreciseSnapshotBuilder::new_shared(capture, produced_sequence, global);
        if let Some(play_time) = playing_time {
            if play_time >= 150.0 && self.enable_key_overlay.current() {
                snapshot = snapshot.key_overlay(self.reader.key_overlay(context.mode));
            }
            let enabled = self.enable_key_overlay.mania_key_hit_errors();
            self.reader.set_mania_key_hit_errors(enabled);
            let offsets_enabled = self.enable_key_overlay.hit_offsets();
            self.reader.set_hit_offsets(offsets_enabled);
            snapshot = snapshot.enable_hit_offsets(offsets_enabled);
            let mut errors = self.reader.hit_errors(context.committed_hit_error_index);
            if let Ok(crate::process::ReadStatus::Ready(errors)) = &mut errors {
                if enabled {
                    errors.errors.key.get_or_insert_default();
                } else {
                    errors.errors.key = None;
                }
            }
            snapshot = snapshot.hit_errors(errors);
            if context.mode == 2 {
                snapshot = snapshot.catcher(self.reader.catcher(play_time));
            }
        }
        Ok(snapshot.publish())
    }
}

//! Raw stable/lazer reader contracts.
//!
//! Reader implementations translate source-specific sentinels at this boundary.
//! Payload DTOs therefore never need to contain magic strings or booleans.

pub mod lazer;
pub use lazer::{LazerMemory, LazerPattern};

pub mod lazer_loader;
pub mod lazer_offsets;
pub mod lazer_version;
pub mod snapshot;

pub mod stable;
pub use stable::{StableMemory, StablePatterns};

use crate::model::memory::{
    AudioVelocityBase, Gameplay, Global, GlobalPrecise, HitErrors, KeyOverlay, LazerRoom,
    LazerSpectator, Leaderboard, Menu, Mp3Length, RankedPlay, ResultScreen, Settings, Tourney,
    TourneyChat, TourneyUser, User,
};
use crate::process::{MemoryError, MemoryRead, ReadStatus};

/// Complete regular/precise reader surface shared by stable and lazer clients.
///
/// Unsupported client-specific methods return `ReadStatus::Unsupported`; a valid
/// empty collection, zero, or `None` remains `ReadStatus::Ready`.
pub trait MemoryReader: Send {
    fn audio_velocity_base(&mut self) -> MemoryRead<AudioVelocityBase>;
    fn user(&mut self) -> MemoryRead<User>;
    fn result_screen(&mut self) -> MemoryRead<ResultScreen>;
    fn gameplay(&mut self, play_time: f64, first_object_time: Option<i64>) -> MemoryRead<Gameplay>;
    fn key_overlay(&mut self, mode: i64) -> MemoryRead<KeyOverlay>;
    fn hit_errors(&mut self, last_index: i64) -> MemoryRead<HitErrors>;
    fn set_mania_key_hit_errors(&mut self, _enabled: bool) {}
    fn catcher(&mut self, _play_time: f64) -> MemoryRead<crate::model::memory::Catcher> {
        Ok(ReadStatus::Unsupported("catcher"))
    }
    fn global(&mut self) -> MemoryRead<Global>;
    fn global_precise(&mut self) -> MemoryRead<GlobalPrecise>;
    fn menu(&mut self, previous_checksum: &str) -> MemoryRead<Menu>;
    fn mp3_length(&mut self) -> MemoryRead<Mp3Length>;
    fn tourney(&mut self) -> MemoryRead<Tourney>;
    fn tourney_chat(
        &mut self,
        messages: &[crate::model::states::TourneyManagerChatItem],
    ) -> MemoryRead<TourneyChat>;
    fn tourney_user(&mut self) -> MemoryRead<TourneyUser>;
    fn leaderboard(&mut self, mode: i64) -> MemoryRead<Leaderboard>;
    fn ranked_play(&mut self) -> MemoryRead<RankedPlay>;
    fn room(&mut self) -> MemoryRead<LazerRoom>;
    fn spectating_data(&mut self, show_mp_commands: bool) -> MemoryRead<LazerSpectator>;
    fn settings(&mut self) -> MemoryRead<Settings>;
}

/// Small synchronization payload copied from a regular decoder to its
/// independent precise decoder. Stable readers leave both addresses absent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReaderSynchronization {
    pub game_base: Option<usize>,
    pub current_screen: Option<usize>,
}

/// Synchronization hook for reader pairs whose decoder-local pointer caches
/// cannot be shared mutably across their blocking capture threads.
pub trait SynchronizedMemoryReader: MemoryReader {
    /// Captures only immutable, already-validated addresses.
    fn reader_synchronization(&self) -> ReaderSynchronization {
        ReaderSynchronization::default()
    }

    /// Applies live reader-side options before a regular capture.
    fn synchronize_capture_config(
        &mut self,
        _show_mp_commands: bool,
        _read_mania_scroll_speed: bool,
    ) {
    }

    /// Installs regular-reader addresses and the reducer-owned mode immediately
    /// before a precise capture. Stable readers intentionally do nothing.
    fn synchronize_precise(&mut self, _state: ReaderSynchronization, _mode: i64) {}
}

/// Stable's key-overlay wire source uses an empty string for pending data.
/// Keeping that sentinel here prevents it from being confused with a valid `[]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOverlaySample {
    Ready(KeyOverlay),
    EmptyString,
}

impl KeyOverlaySample {
    pub fn into_memory_read(self) -> MemoryRead<KeyOverlay> {
        Ok(match self {
            Self::Ready(value) => ReadStatus::Ready(value),
            Self::EmptyString => ReadStatus::NotReady,
        })
    }
}

/// The source's tournament-chat `false` means no change, while `[]` is a
/// committed, changed value that clears the chat list.
#[derive(Debug, Clone, PartialEq)]
pub enum TourneyChatSample {
    Changed(TourneyChat),
    UnchangedFalse,
}

impl TourneyChatSample {
    pub fn into_memory_read(self) -> MemoryRead<TourneyChat> {
        Ok(match self {
            Self::Changed(value) => ReadStatus::Ready(value),
            Self::UnchangedFalse => ReadStatus::Unchanged,
        })
    }
}

/// Whether a last-good slot changed after a reader call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionStatus {
    Applied,
    NotReady,
    Unchanged,
    Unsupported(&'static str),
    Error(MemoryError),
}

/// A slot that distinguishes "never read" from a committed `Ready(None)`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LastGood<T> {
    #[default]
    Missing,
    Present(T),
}

impl<T> LastGood<T> {
    #[must_use]
    pub const fn new() -> Self {
        Self::Missing
    }

    #[must_use]
    pub fn get(&self) -> Option<&T> {
        match self {
            Self::Missing => None,
            Self::Present(value) => Some(value),
        }
    }

    #[must_use]
    pub fn into_inner(self) -> Option<T> {
        match self {
            Self::Missing => None,
            Self::Present(value) => Some(value),
        }
    }

    /// Atomically replaces the slot only after a complete `Ready` payload exists.
    /// Every other status, including hard failures and unsupported methods, keeps
    /// the prior value intact.
    pub fn apply(&mut self, read: MemoryRead<T>) -> RetentionStatus {
        match read {
            Ok(ReadStatus::Ready(value)) => {
                *self = Self::Present(value);
                RetentionStatus::Applied
            }
            Ok(ReadStatus::NotReady) => RetentionStatus::NotReady,
            Ok(ReadStatus::Unchanged) => RetentionStatus::Unchanged,
            Ok(ReadStatus::Unsupported(reason)) => RetentionStatus::Unsupported(reason),
            Err(error) => RetentionStatus::Error(error),
        }
    }
}

/// All public methods have independent error-report histories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ReaderMethod {
    AudioVelocityBase,
    User,
    ResultScreen,
    Gameplay,
    KeyOverlay,
    HitErrors,
    Global,
    GlobalPrecise,
    Menu,
    Mp3Length,
    Tourney,
    TourneyChat,
    TourneyUser,
    Leaderboard,
    RankedPlay,
    Settings,
}

impl ReaderMethod {
    const COUNT: usize = 16;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportLevel {
    Debug,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorReport {
    pub method: ReaderMethod,
    pub consecutive_errors: u32,
    pub level: ReportLevel,
}

/// Bounded per-method counters implementing the source's first-ten reporting rule.
///
/// A counter resets only after a payload was actually committed. Pending,
/// unchanged, unsupported, checksum-only, and other caller-declared no-op results
/// leave it untouched.
#[derive(Debug, Clone)]
pub struct ErrorReportCounters {
    counts: [u32; ReaderMethod::COUNT],
    debug_threshold: u32,
    maximum: u32,
}

impl Default for ErrorReportCounters {
    fn default() -> Self {
        Self::new(10, u32::MAX)
    }
}

impl ErrorReportCounters {
    #[must_use]
    pub fn new(debug_threshold: u32, maximum: u32) -> Self {
        Self {
            counts: [0; ReaderMethod::COUNT],
            debug_threshold,
            maximum: maximum.max(debug_threshold.saturating_add(1)),
        }
    }

    #[must_use]
    pub fn count(&self, method: ReaderMethod) -> u32 {
        self.counts[method as usize]
    }

    pub fn record_error(&mut self, method: ReaderMethod) -> ErrorReport {
        let count = &mut self.counts[method as usize];
        *count = count.saturating_add(1).min(self.maximum);
        ErrorReport {
            method,
            consecutive_errors: *count,
            level: if *count <= self.debug_threshold {
                ReportLevel::Debug
            } else {
                ReportLevel::Error
            },
        }
    }

    /// Call only when a `Ready` payload was accepted into state.
    pub fn record_committed_success(&mut self, method: ReaderMethod) {
        self.counts[method as usize] = 0;
    }

    /// Updates reporting state from a retention decision. Only `Applied` is a
    /// committed success; pending/no-op/unsupported outcomes preserve the count.
    pub fn observe_retention(
        &mut self,
        method: ReaderMethod,
        status: &RetentionStatus,
    ) -> Option<ErrorReport> {
        match status {
            RetentionStatus::Applied => {
                self.record_committed_success(method);
                None
            }
            RetentionStatus::Error(_) => Some(self.record_error(method)),
            RetentionStatus::NotReady
            | RetentionStatus::Unchanged
            | RetentionStatus::Unsupported(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::memory::MatchmakingStats;

    fn hard_error() -> MemoryError {
        MemoryError::Backend {
            operation: "fixture",
            message: "failed".to_owned(),
        }
    }

    #[test]
    fn ready_zero_empty_and_none_are_payloads() {
        let mut zero = LastGood::new();
        assert_eq!(
            zero.apply(Ok(ReadStatus::Ready(0_i64))),
            RetentionStatus::Applied
        );
        assert_eq!(zero.get(), Some(&0));

        let mut empty = LastGood::new();
        assert_eq!(
            empty.apply(Ok(ReadStatus::Ready(Vec::<u8>::new()))),
            RetentionStatus::Applied
        );
        assert_eq!(empty.get(), Some(&Vec::new()));

        let mut intentional_none = LastGood::new();
        assert_eq!(
            intentional_none.apply(Ok(ReadStatus::Ready(None::<MatchmakingStats>))),
            RetentionStatus::Applied
        );
        assert!(matches!(intentional_none, LastGood::Present(None)));
    }

    #[test]
    fn non_ready_statuses_and_errors_retain_last_good() {
        let mut value = LastGood::Present(String::from("last-good"));
        assert_eq!(
            value.apply(Ok(ReadStatus::NotReady)),
            RetentionStatus::NotReady
        );
        assert_eq!(value.get().map(String::as_str), Some("last-good"));
        assert_eq!(
            value.apply(Ok(ReadStatus::Unchanged)),
            RetentionStatus::Unchanged
        );
        assert_eq!(value.get().map(String::as_str), Some("last-good"));
        assert_eq!(
            value.apply(Ok(ReadStatus::Unsupported("not on stable"))),
            RetentionStatus::Unsupported("not on stable")
        );
        assert_eq!(value.get().map(String::as_str), Some("last-good"));
        assert!(matches!(
            value.apply(Err(hard_error())),
            RetentionStatus::Error(_)
        ));
        assert_eq!(value.get().map(String::as_str), Some("last-good"));

        assert_eq!(
            value.apply(Ok(ReadStatus::Ready(String::from("complete replacement")))),
            RetentionStatus::Applied
        );
        assert_eq!(
            value.get().map(String::as_str),
            Some("complete replacement")
        );
    }

    #[test]
    fn key_overlay_empty_string_differs_from_ready_empty_overlay() {
        assert_eq!(
            KeyOverlaySample::EmptyString.into_memory_read(),
            Ok(ReadStatus::NotReady)
        );
        assert_eq!(
            KeyOverlaySample::Ready(Vec::new()).into_memory_read(),
            Ok(ReadStatus::Ready(Vec::new()))
        );
    }

    #[test]
    fn chat_false_differs_from_changed_empty_chat() {
        assert_eq!(
            TourneyChatSample::UnchangedFalse.into_memory_read(),
            Ok(ReadStatus::Unchanged)
        );
        assert_eq!(
            TourneyChatSample::Changed(Vec::new()).into_memory_read(),
            Ok(ReadStatus::Ready(Vec::new()))
        );
    }

    #[test]
    fn error_reports_are_per_method_bounded_and_reset_only_on_commit() {
        let mut reports = ErrorReportCounters::new(2, 3);
        assert_eq!(
            reports.record_error(ReaderMethod::Menu).level,
            ReportLevel::Debug
        );
        assert_eq!(
            reports.record_error(ReaderMethod::Menu).level,
            ReportLevel::Debug
        );
        assert_eq!(
            reports.record_error(ReaderMethod::Menu).level,
            ReportLevel::Error
        );
        assert_eq!(
            reports.record_error(ReaderMethod::Menu).consecutive_errors,
            3
        );
        assert_eq!(reports.count(ReaderMethod::User), 0);

        let mut retained = LastGood::Present(5_i64);
        let pending = retained.apply(Ok(ReadStatus::NotReady));
        assert_eq!(
            reports.observe_retention(ReaderMethod::Menu, &pending),
            None
        );
        assert_eq!(reports.count(ReaderMethod::Menu), 3);
        reports.record_committed_success(ReaderMethod::User);
        assert_eq!(reports.count(ReaderMethod::Menu), 3);

        let committed = retained.apply(Ok(ReadStatus::Ready(6)));
        assert_eq!(
            reports.observe_retention(ReaderMethod::Menu, &committed),
            None
        );
        assert_eq!(reports.count(ReaderMethod::Menu), 0);

        let failure = retained.apply(Err(hard_error()));
        let report = reports
            .observe_retention(ReaderMethod::Menu, &failure)
            .unwrap();
        assert_eq!(report.level, ReportLevel::Debug);
    }
}

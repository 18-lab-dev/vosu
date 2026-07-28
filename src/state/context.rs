use std::error::Error;
use std::fmt::{Display, Formatter};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::memory::ReaderSynchronization;
#[cfg(test)]
use crate::memory::snapshot::PreciseCaptureContext;
use crate::memory::snapshot::{
    InstanceEpoch, RawPreciseSnapshot, RawRegularSnapshot, RegularCaptureContext,
};
use crate::model::enums::{ClientType, GameState};
use crate::model::states::{CalculateMods, TourneyManagerChatItem};
use crate::process::MemoryError;

pub type TransitionFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, StateWorkerError>> + Send + 'a>>;

#[derive(Debug)]
pub enum StateWorkerError {
    Capture(String),
    MemoryPermissionDenied {
        address: usize,
        len: usize,
        message: String,
    },
    Transition(String),
    Publish(String),
    ChannelClosed(&'static str),
    ThreadPanicked(&'static str),
    InvalidRegularContext,
}

impl Display for StateWorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capture(message) => write!(f, "memory capture failed: {message}"),
            Self::MemoryPermissionDenied {
                address,
                len,
                message,
            } => write!(
                f,
                "memory capture permission denied reading {len} bytes at {address:#x}: {message}"
            ),
            Self::Transition(message) => write!(f, "state transition failed: {message}"),
            Self::Publish(message) => write!(f, "state publication failed: {message}"),
            Self::ChannelClosed(channel) => write!(f, "state worker channel closed: {channel}"),
            Self::ThreadPanicked(worker) => write!(f, "{worker} reader thread panicked"),
            Self::InvalidRegularContext => {
                f.write_str("regular snapshot belongs to another instance or sequence")
            }
        }
    }
}

impl Error for StateWorkerError {}

impl From<MemoryError> for StateWorkerError {
    fn from(error: MemoryError) -> Self {
        match error {
            MemoryError::PermissionDenied {
                address,
                len,
                message,
            } => Self::MemoryPermissionDenied {
                address,
                len,
                message,
            },
            error => Self::Capture(error.to_string()),
        }
    }
}

/// Reducer-owned context copied into every precise capture.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadContext {
    pub source: ClientType,
    pub instance_generation: u64,
    pub reset_epoch: u64,
    pub status: i64,
    pub play_time: f64,
    pub mode: i64,
    pub mods: CalculateMods,
    pub committed_hit_error_index: i64,
    pub accepted_precise_sequence: u64,
    pub latest_precise_global_sequence: u64,
}

impl ReadContext {
    pub(super) fn epoch(&self) -> InstanceEpoch {
        InstanceEpoch::new(self.instance_generation, self.reset_epoch)
    }

    #[cfg(test)]
    pub(super) fn precise_capture(&self) -> PreciseCaptureContext {
        PreciseCaptureContext::new(
            self.source,
            self.instance_generation,
            self.reset_epoch,
            self.mode,
            self.mods.clone(),
            self.committed_hit_error_index,
            self.accepted_precise_sequence,
        )
    }
}

/// Source branches selected by the reducer from its committed mutable state.
#[derive(Debug, Clone)]
pub struct RegularCapturePlan {
    pub previous_checksum: String,
    pub read_mp3_length: bool,
    pub status: i64,
    pub gameplay_time: f64,
    pub first_object_time: Option<i64>,
    pub tourney_messages: Arc<[TourneyManagerChatItem]>,
    pub read_tourney_user: bool,
    pub read_mania_scroll_speed: bool,
    pub show_mp_commands: bool,
}

impl Default for RegularCapturePlan {
    fn default() -> Self {
        Self {
            previous_checksum: String::new(),
            read_mp3_length: false,
            status: GameState::Menu.value() as i64,
            gameplay_time: 0.0,
            first_object_time: None,
            tourney_messages: Arc::from([]),
            read_tourney_user: false,
            read_mania_scroll_speed: true,
            show_mp_commands: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegularCaptureRequest {
    pub context: RegularCaptureContext,
    pub plan: RegularCapturePlan,
}

pub trait RegularSnapshotReader: Send + 'static {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError>;
}

pub trait PreciseSnapshotReader: Send + 'static {
    fn capture_precise(
        &mut self,
        context: &ReadContext,
        produced_sequence: u64,
        synchronization: ReaderSynchronization,
    ) -> Result<Arc<RawPreciseSnapshot>, StateWorkerError>;
}

pub struct RegularTransition<P> {
    pub publication: P,
    pub context: ReadContext,
    pub next_capture: RegularCapturePlan,
}

pub struct PreciseTransition<P> {
    pub publication: P,
    pub context: Option<ReadContext>,
}

/// Implemented by the later StateBundle. The worker calls these methods only on
/// its single reducer task, so mutable state can never be entered concurrently.
pub trait SnapshotTransition: Send + 'static {
    type Publication: Send + Sync + 'static;

    fn apply_regular<'a>(
        &'a mut self,
        snapshot: Arc<RawRegularSnapshot>,
    ) -> TransitionFuture<'a, RegularTransition<Self::Publication>>;

    fn apply_precise<'a>(
        &'a mut self,
        snapshot: Arc<RawPreciseSnapshot>,
    ) -> TransitionFuture<'a, PreciseTransition<Self::Publication>>;
}

pub trait SnapshotPublisher<P>: Send + Sync + 'static {
    fn publish(&self, publication: P) -> Result<(), StateWorkerError>;
}

#[derive(Debug, Clone, Copy)]
pub struct PollIntervals {
    pub regular: Duration,
    pub precise: Duration,
}

#[derive(Clone)]
pub enum PollIntervalUpdates {
    Direct(watch::Receiver<PollIntervals>),
    Config(watch::Receiver<Arc<crate::config::Config>>),
}

impl From<watch::Receiver<PollIntervals>> for PollIntervalUpdates {
    fn from(value: watch::Receiver<PollIntervals>) -> Self {
        Self::Direct(value)
    }
}

impl From<watch::Receiver<Arc<crate::config::Config>>> for PollIntervalUpdates {
    fn from(value: watch::Receiver<Arc<crate::config::Config>>) -> Self {
        Self::Config(value)
    }
}

pub(crate) fn duration_from_millis_saturating(milliseconds: f64) -> Duration {
    let seconds = milliseconds / 1_000.0;
    if !seconds.is_finite() || seconds >= Duration::MAX.as_secs_f64() {
        Duration::MAX
    } else if seconds <= 0.0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(seconds)
    }
}

impl PollIntervalUpdates {
    pub(super) fn current(&self) -> PollIntervals {
        match self {
            Self::Direct(value) => *value.borrow(),
            Self::Config(value) => {
                let value = value.borrow();
                PollIntervals {
                    regular: duration_from_millis_saturating(value.poll_rate),
                    precise: duration_from_millis_saturating(value.precise_data_poll_rate),
                }
            }
        }
    }
}

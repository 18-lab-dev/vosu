//! Bounded local archive preparation and selected-client handoff (catalog D8).
//!
//! Blocking preparation/stable operations must run on an owned blocking worker.
//! The dispatch owner bounds admission, retains the selected session generation,
//! cancels on detach/shutdown and joins preparation before returning a terminal
//! result. No registry locks may be held while calling these filesystem APIs.
//! Recheck that generation immediately before commit/submit, never resolve focus
//! again. Cancellation is cooperative up to the final commit/spawn boundary.

pub mod archive;
pub mod lazer;
pub mod stable;

pub use archive::{ArchiveLimits, ValidatedArchive};
pub use lazer::LazerTarget;
pub use stable::StableStage;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Deliberately path-free: filesystem/decoder diagnostics never cross the action
/// boundary, including filenames embedded in a malicious ZIP's error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    #[error("invalid local beatmap source")]
    InvalidSource,
    #[error("unsafe or invalid beatmap archive")]
    InvalidArchive,
    #[error("beatmap archive exceeds import limits")]
    LimitExceeded,
    #[error("beatmap import cancelled")]
    Cancelled,
    #[error("selected client import context unavailable")]
    InvalidTarget,
    #[error("beatmap import destination already exists")]
    DestinationExists,
    #[error("beatmap import I/O failed")]
    Io,
    #[error("selected client handoff failed")]
    Handoff,
}

/// Only the two successful handoff paths can construct a completion result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportResult {
    client: &'static str,
    disposition: &'static str,
    #[serde(rename = "refreshRequired")]
    refresh_required: bool,
}
impl ImportResult {
    const fn queued() -> Self {
        Self {
            client: "stable",
            disposition: "queued",
            refresh_required: true,
        }
    }
    const fn submitted() -> Self {
        Self {
            client: "lazer",
            disposition: "submitted",
            refresh_required: false,
        }
    }
}
fn check_cancel(cancel: &CancellationToken) -> Result<(), ImportError> {
    if cancel.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;

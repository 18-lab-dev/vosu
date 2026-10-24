//! Crash-atomic, single-flight self-update lifecycle.
//!
//! The application owns when to invoke this module and how to terminate its
//! server.  Everything from release validation through durable recovery is
//! kept here so an interrupted update always has enough information to make a
//! deterministic decision on the next start.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::net::streaming::{NetworkError, StreamingClient};

pub const USER_AGENT: &str = concat!("vosu/", env!("CARGO_PKG_VERSION"));
pub const INSTALL_RECORD_NAME: &str = ".vosu-update.json";
pub const LATEST_RELEASE_API: &str = "https://api.github.com/repos/18-lab-dev/vosu/releases/latest";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
#[cfg(windows)]
pub const RELEASE_PLATFORM: &str = "windows";
#[cfg(target_os = "linux")]
pub const RELEASE_PLATFORM: &str = "linux";
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_EXPANDED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("update already in progress")]
    AlreadyRunning,
    #[error("another updater owner is still alive")]
    LiveOwner,
    #[error("release metadata is malformed: {0}")]
    Metadata(String),
    #[error("release asset is missing")]
    NoFiles,
    #[error("downloaded archive digest did not match release metadata")]
    DigestMismatch,
    #[error("update transaction is quarantined: {0}")]
    Quarantined(String),
    #[error("readiness authorization failed")]
    ReadinessAuthorization,
    #[error("candidate failed to become ready")]
    CandidateFailed,
    #[error("update helper failed to acknowledge durable ownership")]
    HelperHandoff,
    #[error("network error: {0}")]
    Network(#[from] NetworkError),
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("invalid internal updater arguments: {0}")]
    InternalArguments(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseDecision {
    Latest(Version),
    Update(ValidatedRelease),
    NoFiles,
    SuppressedForced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedRelease {
    pub version: Version,
    pub archive_url: String,
    pub digest: [u8; 32],
    pub asset_name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    Created,
    Metadata,
    Downloading,
    Verified,
    Prepared,
    HandedOff,
    Swapped,
    Rollback,
    Ready,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    /// OS process creation identity, not elapsed time. PID reuse therefore
    /// cannot steal or block an update transaction.
    pub start: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum EncodedOsString {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}

impl EncodedOsString {
    pub fn capture(value: OsString) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Self::Unix(value.into_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(value.encode_wide().collect())
        }
    }

    pub fn restore(&self) -> io::Result<OsString> {
        match self {
            Self::Unix(bytes) => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStringExt;
                    Ok(OsString::from_vec(bytes.clone()))
                }
                #[cfg(not(unix))]
                {
                    let _ = bytes;
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Unix argument on Windows",
                    ))
                }
            }
            Self::Windows(units) => {
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStringExt;
                    Ok(OsString::from_wide(units))
                }
                #[cfg(not(windows))]
                {
                    let _ = units;
                    Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Windows argument on Unix",
                    ))
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallRecord {
    pub nonce: String,
    pub token: String,
    pub owner: ProcessIdentity,
    pub helper: Option<ProcessIdentity>,
    pub original_args: Vec<EncodedOsString>,
    pub working_directory: EncodedOsString,
    pub target: PathBuf,
    pub old_digest: String,
    pub archive_digest: Option<String>,
    pub new_digest: Option<String>,
    pub phase: InstallPhase,
    pub work: PathBuf,
    pub part: PathBuf,
    pub staging: PathBuf,
    pub backup: PathBuf,
    pub helper_path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    CleanAndContinue,
    ResumeCandidate,
    RestoreRollback,
    FinishCleanup,
    Quarantine,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateCut {
    BeforeRecordCreate,
    AfterRecordCreate,
    BeforePhase(InstallPhase),
    AfterPhase(InstallPhase),
    BeforePartDownload,
    AfterPartDownload,
    BeforeVerification,
    AfterVerification,
    BeforeStaging,
    AfterStaging,
    BeforeHelperCopy,
    AfterHelperCopy,
    BeforeHandoff,
    AfterHandoff,
    BeforeSwap,
    AfterSwap,
    BeforeRollback,
    AfterRollback,
    BeforeCleanup,
    AfterCleanup,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupReadiness {
    pub record_path: PathBuf,
    pub transaction_token: String,
    pub address: SocketAddr,
}

impl StartupReadiness {
    /// Call only after config/log initialization, application construction,
    /// and the runtime listener have all completed successfully.
    pub fn signal_ready(&self) -> Result<(), UpdateError> {
        signal_ready(&self.record_path, &self.transaction_token, self.address)
    }

    /// Call after signalling readiness. This waits for the exact helper owner
    /// to exit and token-validates cleanup of all update artifacts.
    pub fn finish_cleanup<P: UpdatePlatform>(&self, platform: &P) -> Result<(), UpdateError> {
        finish_ready_cleanup(&self.record_path, &self.transaction_token, platform)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupIntent {
    Ordinary,
    Helper {
        record_path: PathBuf,
        transaction_token: String,
    },
    Candidate(StartupReadiness),
    Rollback(StartupReadiness),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartupDispatch {
    Ordinary { recovery: Option<RecoveryAction> },
    HelperCompleted,
    Candidate(StartupReadiness),
    Rollback(StartupReadiness),
}

mod helper;
mod platform;
mod prepare;
mod release;
mod startup;
mod transaction;

pub use helper::*;
pub use platform::*;
pub use prepare::*;
pub use release::*;
pub use startup::*;
pub use transaction::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    include!("updater/tests_release.rs");
    include!("updater/tests_transaction.rs");
    include!("updater/tests_platform.rs");
}

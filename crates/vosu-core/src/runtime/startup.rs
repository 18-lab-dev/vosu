//! Startup outcome helpers shared by `run_cli` and the binary's exit status.

use std::{future::Future, io};

use crate::app::StartupOutcome;

#[derive(Debug)]
pub(super) struct StartupOwnerConflict;

impl std::fmt::Display for StartupOwnerConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("another updater owner is still alive")
    }
}

impl std::error::Error for StartupOwnerConflict {}

/// Identifies the typed startup ownership conflict mapped by the binary to the
/// updater contract's dedicated exit status 75.
pub fn is_startup_owner_conflict(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.downcast_ref::<StartupOwnerConflict>().is_some())
}

pub(super) fn recovery_error_is_nonfatal(error: &crate::updater::UpdateError) -> bool {
    matches!(error, crate::updater::UpdateError::Quarantined(_))
}

pub(super) async fn finish_runtime_run(
    run_result: io::Result<StartupOutcome>,
    shutdown: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let shutdown_result = shutdown.await;
    match (run_result, shutdown_result) {
        (Err(run_error), _) => Err(run_error),
        (Ok(_), Err(shutdown_error)) => Err(shutdown_error),
        (Ok(_), Ok(())) => Ok(()),
    }
}

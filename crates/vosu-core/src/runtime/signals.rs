//! Process shutdown signals that cancel the daemon's shared shutdown token.

use std::{future::Future, io};

use tokio_util::sync::CancellationToken;

pub(super) const TEST_STOP_FILE_ENV: &str = "VOSU_TEST_STOP_FILE";

pub(super) async fn cancel_on_signal(
    shutdown: CancellationToken,
    signal: impl Future<Output = io::Result<()>>,
) -> io::Result<()> {
    let result = tokio::select! {
        () = shutdown.cancelled() => Ok(()),
        result = signal => result,
    };
    shutdown.cancel();
    result
}

pub(super) async fn run_signal_cancellation(shutdown: CancellationToken) -> io::Result<()> {
    #[cfg(feature = "test-harness")]
    if let Some(path) = std::env::var_os(TEST_STOP_FILE_ENV) {
        return cancel_on_signal(shutdown, async move {
            loop {
                if tokio::fs::try_exists(&path).await? {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await;
    }
    cancel_on_signal(shutdown, shutdown_signal()).await
}

#[cfg(unix)]
async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        received = interrupt.recv() => {
            received.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SIGINT stream closed"))
        }
        received = terminate.recv() => {
            received.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "SIGTERM stream closed"))
        }
    }
}

#[cfg(windows)]
async fn shutdown_signal() -> io::Result<()> {
    use tokio::signal::windows::ctrl_break;

    let mut break_signal = ctrl_break()?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        received = break_signal.recv() => {
            received.ok_or_else(|| io::Error::new(
                io::ErrorKind::BrokenPipe,
                "CTRL_BREAK stream closed",
            ))
        }
    }
}

#[cfg(not(any(unix, windows)))]
async fn shutdown_signal() -> io::Result<()> {
    tokio::signal::ctrl_c().await
}

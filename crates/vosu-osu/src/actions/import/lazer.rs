use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio_util::sync::CancellationToken;
use vosu_plugin_api::SessionAttached;

use super::{ImportError, ImportResult, ValidatedArchive, check_cancel};

/// Frozen attached-client launch context. No global executable lookup, inherited
/// daemon environment, shell, or content-store writes.
///
/// The session owner captures cwd/environment from this attached process on its
/// cold worker. This includes AppImage/display/XDG/storage context on Linux and
/// the selected user's context on Windows. Missing facts fail
/// closed rather than launching an unrelated installation. Keep the associated
/// session generation outside this value and recheck it before `submit`.
pub struct LazerTarget {
    executable: PathBuf,
    cwd: PathBuf,
    environment: Vec<(OsString, OsString)>,
}
impl LazerTarget {
    pub fn from_attached(
        attached: &SessionAttached,
        cwd: PathBuf,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<Self, ImportError> {
        let executable = attached
            .path
            .as_ref()
            .map(PathBuf::from)
            .ok_or(ImportError::InvalidTarget)?;
        if attached.pid == 0
            || !executable.is_absolute()
            || !cwd.is_absolute()
            || environment.is_empty()
            || !executable.is_file()
            || !cwd.is_dir()
            || environment.iter().any(|(key, value)| {
                key.is_empty()
                    || key.to_string_lossy().contains(['=', '\0'])
                    || value.to_string_lossy().contains('\0')
            })
            || attached.argv.as_ref().is_some_and(|args| {
                args.iter().any(|arg| {
                    arg.starts_with("--tournament") || arg.starts_with("--debug-client-id")
                })
            })
            || attached.command_line.as_ref().is_some_and(|args| {
                args.contains("--tournament") || args.contains("--debug-client-id")
            })
        {
            return Err(ImportError::InvalidTarget);
        }
        Ok(Self {
            executable,
            cwd,
            environment,
        })
    }

    pub(super) fn command(&self, archive: &Path) -> Command {
        let mut command = Command::new(&self.executable);
        command
            // osu.Desktop's IPC importer requires the archive first and treats
            // every argument as an import filename. In particular there is no
            // --storage argument; context comes from the selected process.
            .arg(archive)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(self.environment.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// The owner must await this future through completion (including shutdown),
    /// not abandon it. The child and private validated source live in this call;
    /// it waits/reaps the child, with a fixed deadline, and kills/reaps on timeout.
    /// Cancellation after spawn is past the irreversible submission boundary.
    pub async fn submit(
        self,
        archive: ValidatedArchive,
        cancel: &CancellationToken,
    ) -> Result<ImportResult, ImportError> {
        let mut command = self.command(archive.snapshot.path());
        check_cancel(cancel)?;
        let mut child =
            vosu_sys::process_spawn::spawn_tokio(&mut command).map_err(|_| ImportError::Handoff)?;
        let outcome = tokio::time::timeout(Duration::from_secs(30), child.wait()).await;
        let result = match outcome {
            Ok(Ok(status)) if status.success() => Ok(ImportResult::submitted()),
            Ok(_) => Err(ImportError::Handoff),
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                Err(ImportError::Handoff)
            }
        };
        // Explicit lifetime: the subprocess may open its argument at any point
        // before exiting; do not unlink the validated archive early.
        drop(archive);
        result
    }
}

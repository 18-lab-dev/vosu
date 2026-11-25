//! The platform half of a launch: starting one plugin host with exactly its
//! channel objects, and the process handle supervision waits on and kills.
//!
//! - Linux (`super::super::linux`, M5 L0): `/proc/self/exe` from the
//!   dedicated launcher thread, `no_new_privs` and the parent-death signal
//!   set before `execve`, held through a pidfd.
//! - Windows (`super::super::windows_launch`, M5 W0): the image path
//!   captured at startup, created suspended into a strict kill-on-close job
//!   before resume, held through its process handle and job.

use std::io;
use std::sync::Arc;

use super::super::channel::Endpoint;
use super::super::control::{StartupFailure, StartupStage};

/// One started plugin host as supervision holds it. Identity is the handle
/// (pidfd or process handle), never the PID.
pub(super) trait HostProcess: Send + Sync {
    fn pid(&self) -> u32;
    /// Kills the process (and on Windows its job). Succeeds if it already
    /// exited.
    fn kill(&self) -> io::Result<()>;
    /// Waits for the exit, reaping it, and returns its status: the exit
    /// code, or on Linux the negated terminating signal.
    fn wait(&self) -> io::Result<i64>;
}

/// A started child: core's channel end and its process.
pub(super) struct Started {
    pub endpoint: Endpoint,
    pub process: Arc<dyn HostProcess>,
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;

    use vosu_sys::plugin_process::{PluginHostChild, SpawnError};

    use super::super::super::linux::{HostSpawner, LaunchError, SpawnedHost};
    use super::*;

    /// What a launch starts: this executable through `/proc/self/exe`.
    pub struct LaunchTarget {
        spawner: HostSpawner,
    }

    impl LaunchTarget {
        pub(in super::super) fn current(build_identity: &str) -> io::Result<Self> {
            Ok(Self {
                spawner: HostSpawner::start(build_identity)?,
            })
        }

        #[cfg_attr(
            not(feature = "test-harness"),
            expect(dead_code, reason = "only test harnesses start another executable")
        )]
        pub(in super::super) fn executable(path: &Path, build_identity: &str) -> io::Result<Self> {
            Ok(Self {
                spawner: HostSpawner::with_executable(path.to_owned(), build_identity.to_owned())?,
            })
        }

        pub(in super::super) fn spawn(&self) -> Result<Started, StartupFailure> {
            let SpawnedHost { child, channel } = self.spawner.spawn().map_err(spawn_failure)?;
            Ok(Started {
                endpoint: channel,
                process: Arc::new(Child(child)),
            })
        }
    }

    fn spawn_failure(error: LaunchError) -> StartupFailure {
        let errno = match &error {
            LaunchError::Objects(source) => source.raw_os_error(),
            LaunchError::Spawn(SpawnError::Os { source, .. }) => source.raw_os_error(),
            LaunchError::Spawn(SpawnError::Child { errno, .. }) => Some(*errno),
            _ => None,
        };
        StartupFailure {
            stage: StartupStage::Spawn,
            code: "spawn_failed".to_owned(),
            errno,
            message: error.to_string(),
        }
    }

    struct Child(PluginHostChild);

    impl HostProcess for Child {
        fn pid(&self) -> u32 {
            self.0.pid()
        }

        fn kill(&self) -> io::Result<()> {
            self.0.kill()
        }

        fn wait(&self) -> io::Result<i64> {
            use std::os::unix::process::ExitStatusExt;
            let status = self.0.wait()?;
            Ok(match (status.code(), status.signal()) {
                (Some(code), _) => i64::from(code),
                (None, Some(signal)) => -i64::from(signal),
                (None, None) => i64::MIN,
            })
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use vosu_sys::plugin_process::PluginHostProcess;

    use super::super::super::entry::COMMAND;
    use super::super::super::windows_launch::{Launched, launch};
    use super::*;

    /// What a launch starts: the image path captured when the launcher was
    /// created, so a later update of the file is caught by the child's
    /// build-identity check rather than silently mixed in.
    pub struct LaunchTarget {
        program: PathBuf,
        build_identity: String,
    }

    impl LaunchTarget {
        pub(in super::super) fn current(build_identity: &str) -> io::Result<Self> {
            Self::executable(&std::env::current_exe()?, build_identity)
        }

        pub(in super::super) fn executable(path: &Path, build_identity: &str) -> io::Result<Self> {
            Ok(Self {
                program: path.to_owned(),
                build_identity: build_identity.to_owned(),
            })
        }

        pub(in super::super) fn spawn(&self) -> Result<Started, StartupFailure> {
            let arguments = [OsStr::new(COMMAND), OsStr::new(&self.build_identity)];
            let Launched { endpoint, process } =
                launch(&self.program, &arguments).map_err(|error| StartupFailure {
                    stage: match error.startup_stage() {
                        "job" => StartupStage::Job,
                        _ => StartupStage::Spawn,
                    },
                    code: "spawn_failed".to_owned(),
                    errno: None,
                    message: error.to_string(),
                })?;
            Ok(Started {
                endpoint,
                process: Arc::new(Child(process)),
            })
        }
    }

    struct Child(PluginHostProcess);

    impl HostProcess for Child {
        fn pid(&self) -> u32 {
            self.0.pid()
        }

        fn kill(&self) -> io::Result<()> {
            self.0.kill()
        }

        fn wait(&self) -> io::Result<i64> {
            loop {
                if let Some(code) = self.0.wait(None)? {
                    return Ok(i64::from(code));
                }
            }
        }
    }
}

pub use imp::LaunchTarget;

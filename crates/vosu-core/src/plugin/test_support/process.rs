//! Process-backend fixtures: a development package writer and a
//! [`ProcessLauncher`] whose endpoints record what core sends them instead
//! of spawning a child. They exercise core's half of the execution seam —
//! validation, launch requests, dispatch routing, readiness failure and
//! loss — without a `vosu plugin-host` process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vosu_plugin_api::vosu_status;

use crate::identity::PluginInstanceId;
use crate::plugin::error::HostError;
use crate::plugin::execution::{
    InstanceLoss, LossCause, MessageRef, PluginIngress, ProcessEndpoint, ProcessLaunch,
    ProcessLauncher,
};

/// The `[targets.*]` key for the running host (`docs/v2/plugin-abi.md` §8).
pub fn host_target_triple() -> &'static str {
    if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Writes a development package into `dir`: a game plugin `plugin_id`
/// matching `process_name`, with its own namespace schema, one `run` action
/// and `library` copied to `lib/<library_name>`. `runtime` is the manifest's
/// `runtime` value (`"process"` or `"in-process"`).
pub fn write_development_package(
    dir: &Path,
    plugin_id: &str,
    process_name: &str,
    runtime: &str,
    library_name: &str,
    library: &[u8],
) -> std::io::Result<()> {
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='{runtime}'\n\n[match]\nprocess_names=['{process_name}']\n\n[[provides]]\nid='game:fixture'\nversion='*'\n[[provides]]\nid='field:{plugin_id}'\nversion='1.0.0'\nschema='schema.json'\n[[provides]]\nid='action:{plugin_id}.run'\nversion='1.0.0'\n\n[[actions]]\nname='run'\nversion='1.0.0'\ntitle='Run'\nargs_schema='args.json'\nresult_schema='result.json'\nsession='none'\n\n[targets.{target}]\nlibrary='lib/{library_name}'\n",
        target = host_target_triple(),
    );
    let schema = format!(
        r#"{{"title":"{plugin_id}","version":"1.0.0","type":"object","properties":{{"value":{{"type":"integer"}}}}}}"#
    );
    std::fs::create_dir_all(dir.join("lib"))?;
    write_file(&dir.join("manifest.toml"), manifest.as_bytes())?;
    write_file(&dir.join("schema.json"), schema.as_bytes())?;
    write_file(&dir.join("args.json"), br#"{"type":"object"}"#)?;
    write_file(&dir.join("result.json"), b"true")?;
    write_file(&dir.join("lib").join(library_name), library)
}

/// Scratch fixture output in a caller-owned directory, not user state.
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?
        .write_all(bytes)
}

/// What [`RecordingLauncher::launch`] does with the next request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchBehavior {
    /// Returns a ready endpoint.
    Ready,
    /// Reports a startup failure after terminating the endpoint it made.
    FailStartup,
    /// Reports the instance lost, then returns its endpoint as if ready:
    /// the loss lands between launch and publication.
    LoseBeforeReady,
}

/// One launch request as core issued it.
pub struct LaunchRecord {
    pub instance: PluginInstanceId,
    pub plugin_id: String,
    pub library: PathBuf,
    pub init: Vec<u8>,
    pub ingress: PluginIngress,
    pub loss: InstanceLoss,
    pub endpoint: Arc<RecordingEndpoint>,
}

/// A launcher that records every request and hands back a fresh
/// [`RecordingEndpoint`] per launch.
pub struct RecordingLauncher {
    behavior: Mutex<LaunchBehavior>,
    launches: Mutex<Vec<LaunchRecord>>,
}

impl RecordingLauncher {
    pub fn new(behavior: LaunchBehavior) -> Arc<Self> {
        Arc::new(Self {
            behavior: Mutex::new(behavior),
            launches: Mutex::new(Vec::new()),
        })
    }

    pub fn set_behavior(&self, behavior: LaunchBehavior) {
        *self.behavior.lock().unwrap_or_else(PoisonError::into_inner) = behavior;
    }

    pub fn launches(&self) -> MutexGuard<'_, Vec<LaunchRecord>> {
        self.launches.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ProcessLauncher for RecordingLauncher {
    fn launch(&self, launch: ProcessLaunch<'_>) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        let behavior = *self.behavior.lock().unwrap_or_else(PoisonError::into_inner);
        let endpoint = Arc::new(RecordingEndpoint::default());
        self.launches().push(LaunchRecord {
            instance: launch.instance,
            plugin_id: launch.manifest.id.clone(),
            library: launch.package.library().to_owned(),
            init: launch.init.to_owned(),
            ingress: launch.ingress.clone(),
            loss: launch.loss.clone(),
            endpoint: Arc::clone(&endpoint),
        });
        match behavior {
            LaunchBehavior::Ready => Ok(endpoint),
            LaunchBehavior::FailStartup => {
                endpoint.terminate();
                Err(HostError::ProcessStartupFailed(
                    "fixture child never became ready".into(),
                ))
            }
            LaunchBehavior::LoseBeforeReady => {
                launch
                    .loss
                    .report(LossCause::ProcessExited { status: Some(0) });
                Ok(endpoint)
            }
        }
    }
}

/// One host → plugin message an endpoint received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedFrame {
    pub opcode: u32,
    pub flags: u32,
    pub correlation_id: u64,
    pub payload: Vec<u8>,
}

/// Records dispatched messages and teardown calls. Dispatch after
/// termination fails promptly, like a lost channel.
#[derive(Default)]
pub struct RecordingEndpoint {
    frames: Mutex<Vec<RecordedFrame>>,
    frames_changed: Condvar,
    destroy_calls: AtomicUsize,
    terminate_calls: AtomicUsize,
    terminated: AtomicBool,
}

impl RecordingEndpoint {
    pub fn frames(&self) -> Vec<RecordedFrame> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Waits up to `timeout` for a frame with `opcode`.
    pub fn wait_for_frame(&self, opcode: u32, timeout: Duration) -> Option<RecordedFrame> {
        self.wait_for(timeout, |frame| frame.opcode == opcode)
    }

    /// Waits up to `timeout` for a frame matching `predicate`.
    pub fn wait_for(
        &self,
        timeout: Duration,
        predicate: impl Fn(&RecordedFrame) -> bool,
    ) -> Option<RecordedFrame> {
        let deadline = Instant::now() + timeout;
        let mut frames = self.frames.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(frame) = frames.iter().find(|frame| predicate(frame)) {
                return Some(frame.clone());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            frames = self
                .frames_changed
                .wait_timeout(frames, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    pub fn destroy_calls(&self) -> usize {
        self.destroy_calls.load(Ordering::Acquire)
    }

    pub fn terminate_calls(&self) -> usize {
        self.terminate_calls.load(Ordering::Acquire)
    }
}

impl ProcessEndpoint for RecordingEndpoint {
    fn dispatch(&self, message: MessageRef<'_>) -> vosu_status {
        if self.terminated.load(Ordering::Acquire) {
            return vosu_status::VOSU_ERR_NOT_READY;
        }
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(RecordedFrame {
                opcode: message.opcode,
                flags: message.flags,
                correlation_id: message.correlation_id,
                payload: message.payload.to_owned(),
            });
        self.frames_changed.notify_all();
        vosu_status::VOSU_OK
    }

    fn destroy(&self, _grace: Duration) -> Result<(), HostError> {
        self.destroy_calls.fetch_add(1, Ordering::AcqRel);
        self.terminated.store(true, Ordering::Release);
        Ok(())
    }

    fn terminate(&self) {
        self.terminate_calls.fetch_add(1, Ordering::AcqRel);
        self.terminated.store(true, Ordering::Release);
    }
}

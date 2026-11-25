//! The generation-scoped supervisor of `runtime = "process"` instances
//! (`docs/v2/plugin-abi.md` §7.6 "Loss identity", "Restart", "Destroy and
//! shutdown"; A5.3).
//!
//! One [`Supervisor`] per `PluginHost` keeps, for every process instance
//! id, what a restart needs (the validated package, its data directory and
//! dispatch pool), its [`CrashWindow`], its phase and its one cancellable
//! restart timer. It is a state machine only: every transition happens
//! under its own lock, which is a leaf (no host lock is taken, no plugin is
//! called and no thread is joined while it is held), and the host performs
//! what a transition returns afterwards (`host::supervision`).
//!
//! Each transition that makes earlier work stale bumps the entry's
//! `epoch`: a timer, a start in progress or a loss report carrying an older
//! epoch, or arriving in a phase that does not expect it, changes nothing.
//! That is what keeps a stop during a backoff delay, a manual restart
//! racing a loss, or a shutdown racing a timer from ever respawning.
//!
//! Crash events are recorded only here, and only from two places: a loss
//! of a `Running` generation (one per generation, however many signals
//! reported it) and a crash-class startup failure of an automatic restart.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::control::StartupFailure;
use super::restart::{CrashWindow, RestartDecision};
use crate::identity::PluginInstanceId;
use crate::plugin::dispatch::WorkerPool;
use crate::plugin::execution::LossCause;
use crate::plugin::registry::LoadedPlugin;

/// What starting one more generation of an instance needs.
#[derive(Clone)]
pub(crate) struct Template {
    pub plugin: LoadedPlugin,
    pub data_dir: PathBuf,
    pub pool: Option<Arc<WorkerPool>>,
}

/// A supervised instance's phase, as the management surface reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessPhase {
    /// A generation is being launched (initial, automatic or manual).
    Starting,
    /// A generation reached `READY` and was published.
    Running,
    /// Waiting `delay` before the next automatic restart. `streak` counts
    /// crash events since the window was last empty.
    Backoff { delay: Duration, streak: u32 },
    /// No automatic restart: the initial load or a manual restart failed,
    /// a deterministic startup refusal, or the fifth crash event inside the
    /// window. Only a manual restart leaves this phase.
    Failed,
    /// Stopped by the user or by shutdown; never restarted automatically.
    Stopped,
}

/// Why a supervised instance last failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureReason {
    /// A generation did not reach `READY` (§7.6 startup stages).
    Startup(StartupFailure),
    /// A running generation was lost (§7.6 "Loss identity").
    Loss(LossCause),
    /// Core refused to start it for a reason outside the child's startup,
    /// for example a matcher already registered under the same plugin id.
    Refused(String),
}

impl std::fmt::Display for FailureReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Startup(failure) => write!(formatter, "{failure}"),
            Self::Loss(cause) => write!(formatter, "lost ({})", cause.reason()),
            Self::Refused(reason) => write!(formatter, "refused: {reason}"),
        }
    }
}

/// One supervised instance's observable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessStatus {
    pub phase: ProcessPhase,
    pub last_failure: Option<FailureReason>,
    /// Generations started after the first: automatic and manual restarts.
    pub restarts: u64,
    /// Crash events currently inside the rolling window.
    pub crash_events: usize,
}

/// A pending backoff delay: aborted when dropped, unless it already fired.
pub(crate) struct RestartTimer(Option<tokio::task::JoinHandle<()>>);

impl RestartTimer {
    pub fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(task))
    }

    /// The timer fired: its task is finishing on its own.
    fn fired(mut self) {
        self.0.take();
    }
}

impl Drop for RestartTimer {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

struct Entry {
    template: Template,
    window: CrashWindow,
    phase: ProcessPhase,
    epoch: u64,
    timer: Option<RestartTimer>,
    last_failure: Option<FailureReason>,
    restarts: u64,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<PluginInstanceId, Entry>,
    shut_down: bool,
    /// Starts in progress outside the lock (initial, automatic, manual).
    starting: usize,
}

/// What the host does after a crash event or a failed start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Next {
    /// Arm a timer for `delay`, then restart if `epoch` is still current.
    Restart { epoch: u64, delay: Duration },
    /// The instance is `Failed` (or the event was stale): nothing to do.
    Nothing,
}

/// Whether a just-published generation may stay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Published {
    Keep,
    /// A stop, manual restart or shutdown overtook this start: the caller
    /// tears the new generation down at once.
    TearDown,
}

/// Why a manual restart was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartRefused {
    /// No process instance is supervised under that id.
    Unknown,
    /// A generation is already starting.
    Busy,
    ShuttingDown,
}

/// A token for one start in progress: [`Supervisor::wait_idle`] waits for
/// every one of them to drop.
pub(crate) struct StartGuard {
    supervisor: Arc<Supervisor>,
    pub epoch: u64,
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        let mut inner = self.supervisor.lock();
        inner.starting -= 1;
        drop(inner);
        self.supervisor.idle.notify_all();
    }
}

#[derive(Default)]
pub(crate) struct Supervisor {
    inner: Mutex<Inner>,
    idle: Condvar,
}

impl Supervisor {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn guard(self: &Arc<Self>, inner: &mut Inner, epoch: u64) -> StartGuard {
        inner.starting += 1;
        StartGuard {
            supervisor: Arc::clone(self),
            epoch,
        }
    }

    /// The initial load of `id`: a fresh window, phase `Starting`. An
    /// earlier entry under `id` is replaced only once it is stopped or
    /// failed; a starting, running or backing-off one is refused as busy.
    pub fn register(
        self: &Arc<Self>,
        id: PluginInstanceId,
        template: Template,
    ) -> Result<StartGuard, RestartRefused> {
        let mut inner = self.lock();
        if inner.shut_down {
            return Err(RestartRefused::ShuttingDown);
        }
        if let Some(entry) = inner.entries.get(&id)
            && !matches!(entry.phase, ProcessPhase::Stopped | ProcessPhase::Failed)
        {
            return Err(RestartRefused::Busy);
        }
        let epoch = inner.entries.get(&id).map_or(1, |entry| entry.epoch + 1);
        inner.entries.insert(
            id,
            Entry {
                template,
                window: CrashWindow::new(),
                phase: ProcessPhase::Starting,
                epoch,
                timer: None,
                last_failure: None,
                restarts: 0,
            },
        );
        Ok(self.guard(&mut inner, epoch))
    }

    /// The start under `epoch` published its generation.
    pub fn published(&self, id: PluginInstanceId, epoch: u64) -> Published {
        let mut inner = self.lock();
        let shut_down = inner.shut_down;
        match inner.entries.get_mut(&id) {
            Some(entry)
                if !shut_down && entry.epoch == epoch && entry.phase == ProcessPhase::Starting =>
            {
                entry.phase = ProcessPhase::Running;
                Published::Keep
            }
            _ => Published::TearDown,
        }
    }

    /// The start under `epoch` failed. An automatic restart's crash-class
    /// failure is one crash event; every other failure sets `Failed`.
    pub fn start_failed(
        &self,
        id: PluginInstanceId,
        epoch: u64,
        reason: FailureReason,
        automatic: bool,
        now: Instant,
    ) -> Next {
        let mut inner = self.lock();
        let shut_down = inner.shut_down;
        let Some(entry) = inner.entries.get_mut(&id) else {
            return Next::Nothing;
        };
        if entry.epoch != epoch || entry.phase != ProcessPhase::Starting {
            return Next::Nothing;
        }
        let crash = match &reason {
            FailureReason::Startup(failure) => failure.stage.is_crash_class(),
            FailureReason::Loss(_) => true,
            FailureReason::Refused(_) => false,
        };
        entry.last_failure = Some(reason);
        if shut_down {
            entry.phase = ProcessPhase::Stopped;
            return Next::Nothing;
        }
        if automatic && crash {
            Self::crash_event(entry, now)
        } else {
            entry.phase = ProcessPhase::Failed;
            Next::Nothing
        }
    }

    /// A `Running` generation of `id` was lost: one crash event. A loss in
    /// any other phase (a stop, manual restart or shutdown already took the
    /// instance) records nothing.
    pub fn lost(&self, id: PluginInstanceId, cause: LossCause, now: Instant) -> Next {
        let mut inner = self.lock();
        let shut_down = inner.shut_down;
        let Some(entry) = inner.entries.get_mut(&id) else {
            return Next::Nothing;
        };
        if entry.phase != ProcessPhase::Running {
            return Next::Nothing;
        }
        entry.last_failure = Some(FailureReason::Loss(cause));
        if shut_down {
            entry.phase = ProcessPhase::Stopped;
            return Next::Nothing;
        }
        Self::crash_event(entry, now)
    }

    fn crash_event(entry: &mut Entry, now: Instant) -> Next {
        entry.epoch += 1;
        match entry.window.record(now) {
            RestartDecision::Restart { delay, streak } => {
                entry.phase = ProcessPhase::Backoff { delay, streak };
                Next::Restart {
                    epoch: entry.epoch,
                    delay,
                }
            }
            RestartDecision::Failed => {
                entry.phase = ProcessPhase::Failed;
                Next::Nothing
            }
        }
    }

    /// Keeps `timer` as `id`'s pending delay if `epoch` is still waiting;
    /// otherwise drops (aborts) it.
    pub fn arm(&self, id: PluginInstanceId, epoch: u64, timer: RestartTimer) {
        let mut inner = self.lock();
        if let Some(entry) = inner.entries.get_mut(&id)
            && entry.epoch == epoch
            && matches!(entry.phase, ProcessPhase::Backoff { .. })
        {
            entry.timer = Some(timer);
        }
    }

    /// The delay under `epoch` elapsed: `Starting` again, with what the
    /// start needs. `None` if a stop, manual restart or shutdown made it
    /// stale.
    pub fn begin_restart(
        self: &Arc<Self>,
        id: PluginInstanceId,
        epoch: u64,
    ) -> Option<(Template, StartGuard)> {
        let mut inner = self.lock();
        if inner.shut_down {
            return None;
        }
        let entry = inner.entries.get_mut(&id)?;
        if entry.epoch != epoch || !matches!(entry.phase, ProcessPhase::Backoff { .. }) {
            return None;
        }
        if let Some(timer) = entry.timer.take() {
            timer.fired();
        }
        entry.phase = ProcessPhase::Starting;
        entry.restarts += 1;
        let template = entry.template.clone();
        Some((template, self.guard(&mut inner, epoch)))
    }

    /// A manual restart: resets the window and the latch, cancels any
    /// delay, and starts a new generation whatever the phase, unless one
    /// is already starting.
    pub fn manual_restart(
        self: &Arc<Self>,
        id: PluginInstanceId,
    ) -> Result<(Template, StartGuard), RestartRefused> {
        let mut inner = self.lock();
        if inner.shut_down {
            return Err(RestartRefused::ShuttingDown);
        }
        let entry = inner.entries.get_mut(&id).ok_or(RestartRefused::Unknown)?;
        if entry.phase == ProcessPhase::Starting {
            return Err(RestartRefused::Busy);
        }
        entry.window.reset();
        entry.timer = None;
        entry.epoch += 1;
        entry.phase = ProcessPhase::Starting;
        entry.restarts += 1;
        let (template, epoch) = (entry.template.clone(), entry.epoch);
        Ok((template, self.guard(&mut inner, epoch)))
    }

    /// The user stopped `id`: `Stopped`, any delay cancelled, any start in
    /// progress made stale. Returns whether `id` is supervised.
    pub fn stop(&self, id: PluginInstanceId) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.entries.get_mut(&id) else {
            return false;
        };
        entry.epoch += 1;
        entry.timer = None;
        entry.phase = ProcessPhase::Stopped;
        true
    }

    /// Daemon shutdown: no start begins and no delay survives. Every entry
    /// not already failed becomes `Stopped`.
    pub fn shutdown(&self) {
        let timers: Vec<RestartTimer> = {
            let mut inner = self.lock();
            inner.shut_down = true;
            inner
                .entries
                .values_mut()
                .filter_map(|entry| {
                    entry.epoch += 1;
                    if entry.phase != ProcessPhase::Failed {
                        entry.phase = ProcessPhase::Stopped;
                    }
                    entry.timer.take()
                })
                .collect()
        };
        drop(timers);
    }

    /// Waits up to `bound` until no start is in progress. Each one ends
    /// within its startup deadline plus a destroy grace, refusing or
    /// tearing down what it started once shutdown began.
    pub fn wait_idle(&self, bound: Duration) -> bool {
        let inner = self.lock();
        let (inner, timeout) = self
            .idle
            .wait_timeout_while(inner, bound, |inner| inner.starting > 0)
            .unwrap_or_else(PoisonError::into_inner);
        drop(inner);
        !timeout.timed_out()
    }

    pub fn status(&self, id: PluginInstanceId) -> Option<ProcessStatus> {
        let inner = self.lock();
        let entry = inner.entries.get(&id)?;
        Some(ProcessStatus {
            phase: entry.phase.clone(),
            last_failure: entry.last_failure.clone(),
            restarts: entry.restarts,
            crash_events: entry.window.len(),
        })
    }
}

#[cfg(test)]
mod tests;

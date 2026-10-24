use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::{Notify, mpsc, watch};

use super::{
    Delivery, PollIntervalUpdates, PreciseSnapshotReader, ReadContext, RegularCapturePlan,
    RegularCaptureRequest, RegularSnapshotReader, SnapshotPublisher, SnapshotTransition,
    StateWorkerError,
};
use crate::memory::ReaderSynchronization;
use crate::memory::snapshot::{RawPreciseSnapshot, RegularCaptureContext};

pub trait WorkerClock: Send + Sync + 'static {
    /// Returns false when cancellation interrupted the requested sleep.
    fn sleep(&self, duration: Duration) -> bool;
    fn cancel(&self);
    fn is_cancelled(&self) -> bool;
}

pub struct SystemWorkerClock {
    stopped: Mutex<bool>,
    changed: Condvar,
    #[cfg(windows)]
    _timer_resolution: super::windows_timer::TimerResolution,
}

impl Default for SystemWorkerClock {
    fn default() -> Self {
        Self {
            stopped: Mutex::new(false),
            changed: Condvar::new(),
            #[cfg(windows)]
            _timer_resolution: super::windows_timer::TimerResolution::request(),
        }
    }
}

impl WorkerClock for SystemWorkerClock {
    fn cancel(&self) {
        if let Ok(mut stopped) = self.stopped.lock() {
            *stopped = true;
            self.changed.notify_all();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.stopped.lock().map_or(true, |value| *value)
    }

    fn sleep(&self, duration: Duration) -> bool {
        let Ok(stopped) = self.stopped.lock() else {
            return false;
        };
        if *stopped {
            return false;
        }
        self.changed
            .wait_timeout_while(stopped, duration, |value| !*value)
            .map(|(value, _)| !*value)
            .unwrap_or(false)
    }
}

enum RegularPermit {
    Initial(RegularCaptureRequest),
    Committed(Duration, RegularCaptureRequest),
}

struct PreciseSlot {
    value: Mutex<Option<Result<Arc<RawPreciseSnapshot>, StateWorkerError>>>,
    changed: Notify,
}

impl PreciseSlot {
    fn replace(&self, value: Result<Arc<RawPreciseSnapshot>, StateWorkerError>) {
        if let Ok(mut slot) = self.value.lock() {
            let should_notify = slot.is_none();
            *slot = Some(value);
            if should_notify {
                self.changed.notify_one();
            }
        }
    }

    fn take(&self) -> Option<Result<Arc<RawPreciseSnapshot>, StateWorkerError>> {
        self.value.lock().ok()?.take()
    }
}

pub struct StateWorkerHandle {
    clock: Arc<dyn WorkerClock>,
    regular_thread: Option<thread::JoinHandle<()>>,
    precise_thread: Option<thread::JoinHandle<()>>,
    reducer: tokio::task::JoinHandle<Result<(), StateWorkerError>>,
}

impl StateWorkerHandle {
    pub async fn shutdown(mut self) -> Result<(), StateWorkerError> {
        self.clock.cancel();
        if !self.reducer.is_finished() {
            self.reducer.abort();
        }
        let reducer_result = match (&mut self.reducer).await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(StateWorkerError::Transition(format!(
                "reducer task failed: {error}"
            ))),
        };
        let regular = self.regular_thread.take().expect("regular thread handle");
        let precise = self.precise_thread.take().expect("precise thread handle");
        let regular_result = regular.join();
        let precise_result = precise.join();
        regular_result.map_err(|_| StateWorkerError::ThreadPanicked("regular"))?;
        precise_result.map_err(|_| StateWorkerError::ThreadPanicked("precise"))?;
        reducer_result
    }
}

impl Drop for StateWorkerHandle {
    fn drop(&mut self) {
        self.clock.cancel();
        self.reducer.abort();
        if let Some(thread) = self.regular_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = self.precise_thread.take() {
            let _ = thread.join();
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "public worker API keeps each runtime dependency explicit"
)]
pub fn spawn_state_worker<R, Q, T, P>(
    regular_reader: R,
    precise_reader: Q,
    transition: T,
    publisher: Arc<P>,
    initial_context: ReadContext,
    initial_capture: RegularCapturePlan,
    intervals: impl Into<PollIntervalUpdates>,
    synchronization: Arc<Mutex<ReaderSynchronization>>,
) -> StateWorkerHandle
where
    R: RegularSnapshotReader,
    Q: PreciseSnapshotReader,
    T: SnapshotTransition,
    P: SnapshotPublisher<T::Publication>,
{
    spawn_state_worker_with_clock(
        regular_reader,
        precise_reader,
        transition,
        publisher,
        initial_context,
        initial_capture,
        intervals.into(),
        synchronization,
        Arc::new(SystemWorkerClock::default()),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "public worker API keeps the injectable clock and runtime dependencies explicit"
)]
pub fn spawn_state_worker_with_clock<R, Q, T, P>(
    mut regular_reader: R,
    mut precise_reader: Q,
    mut transition: T,
    publisher: Arc<P>,
    initial_context: ReadContext,
    initial_capture: RegularCapturePlan,
    intervals: PollIntervalUpdates,
    synchronization: Arc<Mutex<ReaderSynchronization>>,
    clock: Arc<dyn WorkerClock>,
) -> StateWorkerHandle
where
    R: RegularSnapshotReader,
    Q: PreciseSnapshotReader,
    T: SnapshotTransition,
    P: SnapshotPublisher<T::Publication>,
{
    let (permit_tx, permit_rx) = std::sync::mpsc::sync_channel::<RegularPermit>(1);
    let (regular_tx, mut regular_rx) = mpsc::channel(1);
    let precise_slot = Arc::new(PreciseSlot {
        value: Mutex::new(None),
        changed: Notify::new(),
    });
    let shared_initial_context = Arc::new(initial_context.clone());
    let (context_tx, context_rx) = watch::channel(shared_initial_context.clone());
    let live_context = Arc::new(Mutex::new(shared_initial_context));

    let first_request = RegularCaptureRequest {
        context: RegularCaptureContext::new(
            initial_context.source,
            initial_context.instance_generation,
            initial_context.reset_epoch,
            1,
        )
        .with_base_precise_global_sequence(initial_context.latest_precise_global_sequence),
        plan: initial_capture.clone(),
    };
    permit_tx
        .send(RegularPermit::Initial(first_request))
        .expect("new regular permit channel");

    let regular_clock = clock.clone();
    let regular_context = live_context.clone();
    let regular_intervals = intervals.clone();
    let regular_thread = thread::Builder::new()
        .name("vosu-regular-reader".into())
        .spawn(move || {
            let mut last_api_capture: Option<Instant> = None;
            while !regular_clock.is_cancelled() {
                let permit = match permit_rx.recv() {
                    Ok(value) => value,
                    Err(std::sync::mpsc::RecvError) => break,
                };
                let (mut request, initial) = match permit {
                    RegularPermit::Initial(request) => (request, true),
                    RegularPermit::Committed(delay, request) => {
                        if !regular_clock.sleep(delay) {
                            break;
                        }
                        (request, false)
                    }
                };
                if let Ok(context) = regular_context.lock() {
                    request.plan.status = context.status;
                    request.plan.gameplay_time = context.play_time;
                    request.context.base_precise_global_sequence =
                        context.latest_precise_global_sequence;
                }
                // Subscriber-driven captures run faster than POLL_RATE; only
                // those spaced by at least POLL_RATE reach API consumers.
                let current = regular_intervals.current();
                let now = Instant::now();
                let api_cadence = initial
                    || current.subscription.is_none()
                    || last_api_capture.is_none_or(|at| now.duration_since(at) >= current.regular);
                if api_cadence {
                    last_api_capture = Some(now);
                }
                request.context.api_cadence = api_cadence;
                let result = regular_reader.capture_regular(&request);
                if regular_tx.blocking_send(result).is_err() {
                    break;
                }
            }
        })
        .expect("spawn regular reader");

    let precise_clock = clock.clone();
    let precise_slot_thread = precise_slot.clone();
    let mut precise_context = context_rx;
    let precise_sync = synchronization;
    let precise_intervals = intervals.clone();
    let precise_thread = thread::Builder::new()
        .name("vosu-precise-reader".into())
        .spawn(move || {
            let mut produced_sequence = 0u64;
            while !precise_clock.is_cancelled() {
                produced_sequence = produced_sequence.saturating_add(1);
                let context = precise_context.borrow_and_update().clone();
                let sync = precise_sync.lock().map(|value| *value).unwrap_or_default();
                precise_slot_thread.replace(precise_reader.capture_precise(
                    &context,
                    produced_sequence,
                    sync,
                ));
                let delay = precise_intervals
                    .current()
                    .precise_delay(context.status == 2);
                if !precise_clock.sleep(delay) {
                    break;
                }
            }
        })
        .expect("spawn precise reader");

    let reducer_clock = clock.clone();
    let reducer_context = live_context;
    let reducer = tokio::spawn(async move {
        let mut context = initial_context;
        let mut regular_sequence = 1u64;
        let mut current_capture = initial_capture;
        let mut last_api_precise: Option<Instant> = None;
        loop {
            if reducer_clock.is_cancelled() {
                return Ok(());
            }
            tokio::select! {
                biased;
                regular = regular_rx.recv() => {
                    let Some(regular) = regular else { return Ok(()) };
                    let snapshot = regular?;
                    let observed = snapshot.context();
                    let delivery = if observed.api_cadence {
                        Delivery::All
                    } else {
                        Delivery::Live
                    };
                    if observed.source != context.source
                        || observed.epoch.instance_generation != context.instance_generation
                        || observed.produced_sequence != regular_sequence
                    {
                        return Err(StateWorkerError::InvalidRegularContext);
                    }
                    if observed.epoch.reset_epoch != context.reset_epoch {
                        let replacement = RegularCaptureRequest {
                            context: RegularCaptureContext::new(
                                context.source,
                                context.instance_generation,
                                context.reset_epoch,
                                regular_sequence,
                            )
                            .with_base_precise_global_sequence(
                                context.latest_precise_global_sequence,
                            ),
                            plan: current_capture.clone(),
                        };
                        if permit_tx.send(RegularPermit::Initial(replacement)).is_err() {
                            if reducer_clock.is_cancelled() {
                                return Ok(());
                            }
                            return Err(StateWorkerError::ChannelClosed("regular permit"));
                        }
                        continue;
                    }
                    let committed = transition.apply_regular(snapshot).await?;
                    if reducer_clock.is_cancelled() {
                        return Ok(());
                    }
                    publisher.publish(committed.publication, delivery)?;
                    context = committed.context;
                    let shared_context = Arc::new(context.clone());
                    if let Ok(mut current) = reducer_context.lock() {
                        *current = shared_context.clone();
                    }
                    context_tx.send_replace(shared_context);
                    regular_sequence = regular_sequence.saturating_add(1);
                    current_capture = committed.next_capture;
                    let request = RegularCaptureRequest {
                        context: RegularCaptureContext::new(
                            context.source,
                            context.instance_generation,
                            context.reset_epoch,
                            regular_sequence,
                        )
                        .with_base_precise_global_sequence(
                            context.latest_precise_global_sequence,
                        ),
                        plan: current_capture.clone(),
                    };
                    let delay = intervals.current().regular_delay(context.status == 2);
                    if permit_tx.send(RegularPermit::Committed(delay, request)).is_err() {
                        if reducer_clock.is_cancelled() {
                            return Ok(());
                        }
                        return Err(StateWorkerError::ChannelClosed("regular permit"));
                    }
                }
                _ = precise_slot.changed.notified() => {
                    if let Some(precise) = precise_slot.take() {
                        let snapshot = precise?;
                        if !snapshot.can_accept(
                            context.source,
                            context.epoch(),
                            context.mode,
                            &context.mods,
                            context.committed_hit_error_index,
                            context.accepted_precise_sequence,
                        ) {
                            continue;
                        }
                        let precise_global = match snapshot.global() {
                            Ok(crate::process::ReadStatus::Ready(value)) => Some(*value),
                            _ => None,
                        };
                        let acknowledge_suffix = match snapshot.hit_errors() {
                            None => true,
                            Some(Ok(crate::process::ReadStatus::Ready(_))) => true,
                            Some(_) => false,
                        };
                        let ending_index = snapshot.ending_index();
                        let sequence = snapshot.produced_sequence();
                        let committed = transition.apply_precise(snapshot).await?;
                        if reducer_clock.is_cancelled() {
                            return Ok(());
                        }
                        let current = intervals.current();
                        let now = Instant::now();
                        let delivery = if current.subscription.is_none()
                            || last_api_precise
                                .is_none_or(|at| now.duration_since(at) >= current.precise)
                        {
                            last_api_precise = Some(now);
                            Delivery::Precise
                        } else {
                            Delivery::Live
                        };
                        publisher.publish(committed.publication, delivery)?;
                        if let Some(next_context) = committed.context {
                            context = next_context;
                        } else {
                            if let Some(value) = precise_global {
                                context.status = value.status;
                                context.play_time = value.time;
                                context.latest_precise_global_sequence = sequence;
                            }
                            if acknowledge_suffix {
                                context.committed_hit_error_index = ending_index;
                                context.accepted_precise_sequence = sequence;
                            }
                        }
                        let shared_context = Arc::new(context.clone());
                        if let Ok(mut current) = reducer_context.lock() {
                            *current = shared_context.clone();
                        }
                        context_tx.send_replace(shared_context);
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
        }
    });

    StateWorkerHandle {
        clock,
        regular_thread: Some(regular_thread),
        precise_thread: Some(precise_thread),
        reducer,
    }
}

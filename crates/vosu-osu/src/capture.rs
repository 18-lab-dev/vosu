//! Session-owned capture preparation and worker startup.
use std::{
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use vosu_osu_model::{
    default_calculated_mods,
    enums::{ClientType, GameState},
    launch::InstanceSettings,
};
use vosu_osu_reader::{
    memory::{
        ReaderSynchronization, SynchronizedMemoryReader,
        lazer::{LazerMemory, LazerPattern},
        lazer_offsets::LazerOffsets,
        stable::{StableMemory, StablePatterns},
    },
    process::ProcessHandle,
};
use vosu_osu_state::{
    Delivery, PollIntervalUpdates, ReadContext, ReaderSettings, RegularCapturePlan,
    SnapshotPublisher, SourcePreciseReader, SourceRegularReader, StateBundle, StateBundleSnapshot,
    StateWorkerError, StateWorkerHandle, SubscriptionDemand, spawn_state_worker,
};
use vosu_plugin_api::memory::ReadStatus;

#[derive(Clone)]
pub struct CaptureTarget {
    pub pid: u32,
    pub generation: u64,
    pub client: ClientType,
    pub process: ProcessHandle,
    pub root: PathBuf,
    pub settings: InstanceSettings,
    pub is_tournament_manager: bool,
    pub lazer_offsets: Option<Arc<LazerOffsets>>,
}

pub fn poll_duration(milliseconds: f64) -> Duration {
    if milliseconds.is_finite() && milliseconds > 0.0 {
        Duration::try_from_secs_f64(milliseconds.min(60_000.0) / 1_000.0)
            .unwrap_or(Duration::from_millis(1))
    } else {
        Duration::from_millis(1)
    }
}

pub async fn initialize_state_worker<T, F, Fut, L>(
    cancel: &CancellationToken,
    retry_delay: Duration,
    mut initialize: F,
    mut log_permission_denied: L,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>, StateWorkerError>>,
    L: FnMut(&StateWorkerError),
{
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        match initialize().await {
            Ok(Some(worker)) => return Some(worker),
            Err(error @ StateWorkerError::MemoryPermissionDenied { .. }) => {
                log_permission_denied(&error);
                return None;
            }
            Ok(None) | Err(_) => {
                tokio::select! {
                    _ = cancel.cancelled() => return None,
                    _ = tokio::time::sleep(retry_delay) => {}
                }
            }
        }
    }
}

pub enum PreparedReaders {
    Stable(Box<(StableMemory, StableMemory)>),
    Lazer(Box<(LazerMemory, LazerMemory)>),
}

pub async fn create_state_worker(
    entry: &CaptureTarget,
    reader_settings: watch::Receiver<ReaderSettings>,
    subscription_demand: watch::Receiver<SubscriptionDemand>,
    publisher: Arc<dyn SnapshotPublisher<Arc<StateBundleSnapshot>>>,
    synchronization: Arc<Mutex<ReaderSynchronization>>,
) -> Result<Option<StateWorkerHandle>, StateWorkerError> {
    let process = entry.process.clone();
    let client = entry.client;
    let offsets = entry.lazer_offsets.clone();
    let readers = tokio::task::spawn_blocking(
        move || -> Result<Option<PreparedReaders>, StateWorkerError> {
            match client {
                ClientType::Stable => {
                    let patterns =
                        match StablePatterns::resolve(&process).map_err(StateWorkerError::from)? {
                            ReadStatus::Ready(patterns) => patterns,
                            ReadStatus::NotReady
                            | ReadStatus::Unchanged
                            | ReadStatus::Unsupported(_) => return Ok(None),
                        };
                    Ok(Some(PreparedReaders::Stable(Box::new((
                        StableMemory::new(process.clone(), patterns),
                        StableMemory::new(process, patterns),
                    )))))
                }
                ClientType::Lazer => {
                    let Some(offsets) = offsets else {
                        return Ok(None);
                    };
                    let pattern = LazerPattern::source_default();
                    let mut regular =
                        LazerMemory::new(process.clone(), Arc::clone(&offsets), pattern.clone());
                    if !regular
                        .resolve_game_base()
                        .map_err(StateWorkerError::from)?
                    {
                        return Ok(None);
                    }
                    Ok(Some(PreparedReaders::Lazer(Box::new((
                        regular,
                        LazerMemory::new(process, offsets, pattern),
                    )))))
                }
            }
        },
    )
    .await
    .map_err(|error| {
        StateWorkerError::Capture(format!("reader preparation task failed: {error}"))
    })??;
    let Some(readers) = readers else {
        return Ok(None);
    };
    let intervals = PollIntervalUpdates::Config {
        config: reader_settings.clone(),
        demand: subscription_demand,
    };
    let worker = match readers {
        PreparedReaders::Stable(readers) => {
            let (regular, precise) = *readers;
            finish_state_worker(
                entry,
                reader_settings,
                intervals,
                publisher,
                synchronization,
                regular,
                precise,
            )?
        }
        PreparedReaders::Lazer(readers) => {
            let (regular, precise) = *readers;
            finish_state_worker(
                entry,
                reader_settings,
                intervals,
                publisher,
                synchronization,
                regular,
                precise,
            )?
        }
    };
    Ok(Some(worker))
}

pub fn finish_state_worker<R, Q>(
    entry: &CaptureTarget,
    reader_settings: watch::Receiver<ReaderSettings>,
    intervals: PollIntervalUpdates,
    publisher: Arc<dyn SnapshotPublisher<Arc<StateBundleSnapshot>>>,
    synchronization: Arc<Mutex<ReaderSynchronization>>,
    regular_memory: R,
    precise_memory: Q,
) -> Result<StateWorkerHandle, StateWorkerError>
where
    R: SynchronizedMemoryReader + 'static,
    Q: SynchronizedMemoryReader + 'static,
{
    let current = *reader_settings.borrow();
    let calculation = Arc::new(vosu_osu_state::state_backend::LiveStateCalculationBackend::new());
    let mut bundle = StateBundle::new_with_root(
        entry.client,
        entry.generation,
        entry.root.clone(),
        calculation,
    );
    bundle.set_roles(
        entry.is_tournament_manager,
        entry.settings.tournament_spectator,
    );
    bundle.set_calculate_pp(current.calculate_pp);
    bundle.set_capture_config(current.read_mania_scroll_speed, current.show_mp_commands);
    bundle.set_config_updates(reader_settings.clone());
    publisher.publish(bundle.snapshot(), Delivery::All)?;

    let initial_context = ReadContext {
        source: entry.client,
        instance_generation: entry.generation,
        reset_epoch: 0,
        status: GameState::Menu.value() as i64,
        play_time: 0.0,
        mode: 0,
        mods: default_calculated_mods(),
        committed_hit_error_index: 0,
        accepted_precise_sequence: 0,
        latest_precise_global_sequence: 0,
    };
    let capture = RegularCapturePlan {
        read_mania_scroll_speed: current.read_mania_scroll_speed,
        show_mp_commands: current.show_mp_commands,
        ..RegularCapturePlan::default()
    };
    let regular =
        SourceRegularReader::new(entry.client, regular_memory, Arc::clone(&synchronization));
    let precise = SourcePreciseReader::new(precise_memory, reader_settings.clone());
    Ok(spawn_state_worker(
        regular,
        precise,
        bundle,
        publisher,
        initial_context,
        capture,
        intervals,
        synchronization,
    ))
}

#[cfg(test)]
mod tests;

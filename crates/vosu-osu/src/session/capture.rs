//! Session-owned capture task. No task retains its owning session.
use super::OsuSession;
use crate::{
    capture::{CaptureTarget, create_state_worker, initialize_state_worker},
    publish::BusPublisher,
};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::watch;
use vosu_osu_reader::{memory::ReaderSynchronization, memory::lazer_loader::LogWarn};
use vosu_osu_state::{SnapshotPublisher, StateBundleSnapshot, StateWorkerError};

#[cfg(test)]
pub(super) type CaptureControls = (
    watch::Receiver<vosu_osu_state::ReaderSettings>,
    watch::Receiver<vosu_osu_state::SubscriptionDemand>,
);

impl OsuSession {
    pub async fn start_capture(self: &Arc<Self>, warn: LogWarn, error: LogWarn) {
        let mut task = self.capture.lock().await;
        if task.is_some() || self.lifecycle.is_cancelled() {
            return;
        }
        let session = Arc::downgrade(self);
        let cancel = self.lifecycle_token();
        let mut settings = self.osu_settings.subscribe();
        let publication = self
            .publication
            .get()
            .expect("publication configured before capture");
        let demand = publication.demand.receiver();
        let mut bus_demand = publication.demand.bus_receiver();
        let bus = Arc::clone(&publication.bus);
        let mut offset_ready = self.offset_ready.subscribe();
        #[cfg(test)]
        let offset_waiting = Arc::clone(&self.offset_waiting);
        #[cfg(test)]
        let controls = self.capture_controls.clone();
        let pid = self.pid;
        let generation = self.generation;
        let client = self.client;
        *task = Some(tokio::spawn(async move {
            if cancel.is_cancelled() {
                return;
            }
            let (reader_tx, reader) = watch::channel(reader_settings(
                *settings.borrow_and_update(),
                bus_demand.borrow_and_update().collect_errors,
            ));
            #[cfg(test)]
            controls.send_replace(Some((reader.clone(), demand.clone())));
            let publisher: Arc<dyn SnapshotPublisher<Arc<StateBundleSnapshot>>> =
                Arc::clone(&bus) as Arc<BusPublisher<_>>;
            let synchronization = Arc::new(Mutex::new(ReaderSynchronization::default()));
            let last_probe_error = Arc::new(Mutex::new(None::<String>));
            let initialize = async {
                if client == vosu_osu_model::enums::ClientType::Lazer {
                    while !*offset_ready.borrow_and_update() {
                        #[cfg(test)]
                        offset_waiting.notify_one();
                        tokio::select! {
                            _ = cancel.cancelled() => return None,
                            changed = offset_ready.changed() => { if changed.is_err() { return None; } }
                        }
                    }
                }
                initialize_state_worker(
                &cancel,
                Duration::from_secs(1),
                || {
                    let target = target(&session);
                    let reader = reader.clone();
                    let demand = demand.clone();
                    let publisher = Arc::clone(&publisher);
                    let synchronization = Arc::clone(&synchronization);
                    let warn = Arc::clone(&warn);
                    let last_probe_error = Arc::clone(&last_probe_error);
                    async move {
                        let result = match target {
                            Some(target) => {
                                create_state_worker(
                                    &target,
                                    reader,
                                    demand,
                                    publisher,
                                    synchronization,
                                )
                                .await
                            }
                            None => Ok(None),
                        };
                        report_probe(&result, &last_probe_error, pid, &warn);
                        result
                    }
                },
                |failure| {
                    error(format!(
                        "State initialization stopped for PID {} ({client:?}, generation {}): {failure}",
                        pid, generation
                    ))
                },
            ).await
            };
            tokio::pin!(initialize);
            let mut initialized = false;
            let mut worker = None;
            let mut settings_open = true;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    next = &mut initialize, if !initialized => {
                        initialized = true;
                        worker = next;
                    }
                    changed = settings.changed(), if settings_open => {
                        settings_open = changed.is_ok();
                        if settings_open {
                            reader_tx.send_replace(reader_settings(*settings.borrow_and_update(), bus_demand.borrow().collect_errors));
                        }
                    }
                    changed = bus_demand.changed() => {
                        if changed.is_err() { break; }
                        reader_tx.send_replace(reader_settings(*settings.borrow(), bus_demand.borrow_and_update().collect_errors));
                    }
                }
            }
            // A selected cancellation can race completion of reader preparation.
            // Join the initializer and reader before session teardown completes.
            if !initialized {
                worker = initialize.await;
            }
            if let Some(worker) = worker {
                // shutdown awaits the reducer and synchronously joins reader
                // threads. Keep both joins off the plugin runtime worker.
                let runtime = tokio::runtime::Handle::current();
                let _ =
                    tokio::task::spawn_blocking(move || runtime.block_on(worker.shutdown())).await;
            }
        }));
    }
}

pub(super) fn reader_settings(
    settings: crate::settings::OsuSettings,
    collect_errors: bool,
) -> vosu_osu_state::ReaderSettings {
    vosu_osu_state::ReaderSettings {
        calculate_pp: settings.calculate_pp,
        poll_rate: settings.poll_rate,
        precise_data_poll_rate: settings.precise_data_poll_rate,
        show_mp_commands: settings.show_mp_commands,
        read_mania_scroll_speed: settings.read_mania_scroll_speed,
        collect_hit_errors: collect_errors,
    }
}

fn target(session: &Weak<OsuSession>) -> Option<CaptureTarget> {
    let session = session.upgrade()?;
    Some(CaptureTarget {
        pid: session.pid,
        generation: session.generation,
        client: session.client,
        process: session.process.clone(),
        root: session.root.clone(),
        settings: session.settings.clone(),
        is_tournament_manager: session.is_tournament_manager(),
        lazer_offsets: session.lazer_offsets(),
    })
}

fn report_probe<T>(
    result: &Result<Option<T>, StateWorkerError>,
    previous: &Mutex<Option<String>>,
    pid: u32,
    warn: &LogWarn,
) {
    // Permission denial has its own terminal ERROR callback. Not-ready is not
    // recovery: retain the message until an actual worker is initialized.
    match result {
        Err(StateWorkerError::MemoryPermissionDenied { .. }) | Ok(None) => {}
        Ok(Some(_)) => {
            previous
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
        }
        Err(failure) => {
            let message = format!("PID {pid} memory probe failed: {failure}");
            let warning = {
                let mut previous = previous
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if previous.as_ref() == Some(&message) {
                    None
                } else {
                    let warning = format!("osu! process candidate rejected: {message}");
                    *previous = Some(message);
                    Some(warning)
                }
            };
            if let Some(warning) = warning {
                warn(warning);
            }
        }
    }
}

#[cfg(test)]
mod probe_log_tests {
    use super::*;

    #[test]
    fn permission_denial_does_not_duplicate_the_terminal_error_log() {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let output = messages.clone();
        let warn: LogWarn = Arc::new(move |message| output.lock().unwrap().push(message));
        let previous = Mutex::new(None);
        let denied: Result<Option<()>, _> = Err(StateWorkerError::MemoryPermissionDenied {
            address: 16,
            len: 4,
            message: "denied".into(),
        });
        report_probe(&denied, &previous, 32002, &warn);
        assert!(messages.lock().unwrap().is_empty());
        assert!(previous.lock().unwrap().is_none());
    }

    #[test]
    fn not_ready_preserves_probe_dedup_until_a_worker_really_starts() {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let output = messages.clone();
        let previous = Arc::new(Mutex::new(None));
        let checked = previous.clone();
        let warn: LogWarn = Arc::new(move |message| {
            assert!(
                checked.try_lock().is_ok(),
                "never call host logging under the dedup lock"
            );
            output.lock().unwrap().push(message);
        });
        let failure: Result<Option<()>, _> = Err(StateWorkerError::Capture("same failure".into()));
        for _ in 0..1000 {
            report_probe(&failure, &previous, 32003, &warn);
            report_probe(&Ok(None::<()>), &previous, 32003, &warn);
        }
        assert_eq!(messages.lock().unwrap().len(), 1);
        report_probe(&Ok(Some(())), &previous, 32003, &warn);
        assert!(previous.lock().unwrap().is_none());
        report_probe(&failure, &previous, 32003, &warn);
        assert_eq!(messages.lock().unwrap().len(), 2);
    }
}

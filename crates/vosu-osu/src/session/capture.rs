//! Session-owned capture task. No task retains its owning session.
use super::OsuSession;
use crate::{
    bridge_b1::{B1Event, B1Phase, B1Publisher, B1SessionKey, B1Sink},
    capture::{CaptureTarget, create_state_worker, initialize_state_worker},
};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::watch;
use vosu_osu_reader::{memory::ReaderSynchronization, memory::lazer_loader::LogWarn};
use vosu_osu_state::{SnapshotPublisher, StateBundleSnapshot};

#[cfg(test)]
pub(super) type CaptureControls = (
    watch::Receiver<vosu_osu_state::ReaderSettings>,
    watch::Receiver<vosu_osu_state::SubscriptionDemand>,
);

impl OsuSession {
    pub async fn start_capture(self: &Arc<Self>, sink: Arc<B1Sink>, warn: LogWarn, error: LogWarn) {
        let mut task = self.capture.lock().await;
        if task.is_some() || self.lifecycle.is_cancelled() {
            return;
        }
        let session = Arc::downgrade(self);
        let cancel = self.lifecycle_token();
        let mut settings = self.osu_settings.subscribe();
        let mut inputs = sink.inputs();
        let mut offset_ready = self.offset_ready.subscribe();
        #[cfg(test)]
        let offset_waiting = Arc::clone(&self.offset_waiting);
        #[cfg(test)]
        let controls = self.capture_controls.clone();
        let key = B1SessionKey {
            pid: self.pid,
            generation: self.generation,
        };
        let client = self.client;
        let instance_settings = self.settings.clone();
        *task = Some(tokio::spawn(async move {
            if cancel.is_cancelled() {
                return;
            }
            sink.send(B1Event::Attached {
                key,
                client,
                settings: instance_settings,
                is_tournament_manager: false,
            });
            // Stable also remains in this legacy dashboard phase until its
            // first publication; it never has lazer offsets to mark ready.
            sink.send(B1Event::Phase {
                key,
                phase: B1Phase::ResolvingOffsets,
            });
            let (reader_tx, reader) = watch::channel(
                (*inputs.borrow_and_update()).reader_settings(*settings.borrow_and_update()),
            );
            let (demand_tx, demand) = watch::channel(inputs.borrow().demand);
            #[cfg(test)]
            controls.send_replace(Some((reader.clone(), demand.clone())));
            let publisher: Arc<dyn SnapshotPublisher<Arc<StateBundleSnapshot>>> =
                Arc::new(B1Publisher::new(key, Arc::clone(&sink)));
            let synchronization = Arc::new(Mutex::new(ReaderSynchronization::default()));
            let mut starting = false;
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
                    if target.as_ref().is_some_and(|target| {
                        target.client == vosu_osu_model::enums::ClientType::Lazer
                            && target.lazer_offsets.is_some()
                    }) && !starting
                    {
                        starting = true;
                        sink.send(B1Event::Phase {
                            key,
                            phase: B1Phase::StartingReader,
                        });
                    }
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
                        // One remembered message per session, cleared after success:
                        // repeated failed probes cannot flood the daemon's log.
                        if let Err(failure) = &result {
                            let message = format!("PID {} memory probe failed: {failure}", key.pid);
                            let changed = {
                                let mut previous = last_probe_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                if previous.as_ref() == Some(&message) {
                                    false
                                } else {
                                    *previous = Some(message.clone());
                                    true
                                }
                            };
                            if changed {
                                warn(format!("osu! process candidate rejected: {message}"));
                            }
                        } else {
                            last_probe_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                        }
                        result
                    }
                },
                |failure| {
                    error(format!(
                        "State initialization stopped for PID {} ({client:?}, generation {}): {failure}",
                        key.pid, key.generation
                    ))
                },
            ).await
            };
            tokio::pin!(initialize);
            let mut initialized = false;
            let mut worker = None;
            let mut settings_open = true;
            let mut inputs_open = true;
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
                            reader_tx.send_replace((*inputs.borrow()).reader_settings(*settings.borrow_and_update()));
                        }
                    }
                    changed = inputs.changed(), if inputs_open => {
                        inputs_open = changed.is_ok();
                        if inputs_open {
                            let current = *inputs.borrow_and_update();
                            reader_tx.send_replace(current.reader_settings(*settings.borrow()));
                            demand_tx.send_replace(current.demand);
                        }
                    }
                }
            }
            // A selected cancellation can race completion of reader preparation.
            // Finish the cancellation-aware initializer before detaching so no
            // newly spawned reader or publication can outlive Detached.
            if !initialized {
                worker = initialize.await;
            }
            if let Some(worker) = worker {
                // shutdown awaits the reducer and synchronously joins reader
                // threads. Keep both joins off the two plugin runtime workers.
                let runtime = tokio::runtime::Handle::current();
                let _ =
                    tokio::task::spawn_blocking(move || runtime.block_on(worker.shutdown())).await;
            }
            sink.send(B1Event::Detached { key });
        }));
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
        is_tournament_manager: false,
        lazer_offsets: session.lazer_offsets(),
    })
}

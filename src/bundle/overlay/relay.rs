use std::{collections::HashMap, sync::{Arc, Mutex, PoisonError, Weak}};

use tokio_util::sync::CancellationToken;
use vosu_core::server::service::{COMMANDS_NAMESPACE, RelayControl};
use vosu_overlay_host::commands::{CommandAction, CommandClient, CommandProcessor, Relay, RelaySink};

/// A rejected legacy filter source keeps the connection's prior plan.
pub(super) fn parse_command_filters(source: &[u8]) -> Option<Vec<vosu_surface::filter::Filter>> {
    vosu_adapter_support::filtered::parse_filters(std::str::from_utf8(source).ok()?)
}
struct RelayClient {
    channel: u64,
    client: CommandClient,
    control: Arc<dyn RelayControl>,
}

/// Production relay transport. WebSocket-originated events return to the
/// originating endpoint; route-originated refreshes target `/websocket/commands`.
#[derive(Clone)]
pub struct WebSocketRelaySink {
    clients: Arc<Mutex<HashMap<u64, RelayClient>>>,
    events: tokio::sync::mpsc::Sender<Relay>,
    processor: Arc<Mutex<Weak<CommandProcessor>>>,
    lifecycle: Arc<RelayLifecycle>,
}

struct RelayLifecycle {
    cancellation: CancellationToken,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for RelayLifecycle {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            worker.abort();
        }
    }
}

pub(super) struct RelayRegistration {
    sink: WebSocketRelaySink,
    client_id: u64,
}

impl Drop for RelayRegistration {
    fn drop(&mut self) {
        self.sink.unregister(self.client_id);
    }
}

impl Default for WebSocketRelaySink {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSocketRelaySink {
    pub fn new() -> Self {
        let clients = Arc::new(Mutex::new(HashMap::new()));
        let processor = Arc::new(Mutex::new(Weak::new()));
        // Capacity bounds retained payloads; incoming WebSocket messages are
        // independently limited to 1 MiB by the transport.
        let (events, receiver) = tokio::sync::mpsc::channel(16);
        let cancellation = CancellationToken::new();
        let worker = tokio::spawn(run_relay_worker(
            Arc::clone(&clients),
            Arc::clone(&processor),
            receiver,
            cancellation.clone(),
        ));
        Self {
            clients,
            events,
            processor,
            lifecycle: Arc::new(RelayLifecycle {
                cancellation,
                worker: Mutex::new(Some(worker)),
            }),
        }
    }

    pub(super) fn bind_processor(&self, processor: &Arc<CommandProcessor>) {
        *self
            .processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(processor);
    }

    pub async fn shutdown(&self) {
        self.lifecycle.cancellation.cancel();
        let worker = self
            .lifecycle
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }

    #[cfg(test)]
    pub fn is_shutdown(&self) -> bool {
        self.lifecycle.cancellation.is_cancelled()
    }

    pub(super) fn register(
        &self,
        channel: u64,
        client: CommandClient,
        control: Arc<dyn RelayControl>,
    ) -> RelayRegistration {
        let client_id = client.id;
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                client_id,
                RelayClient {
                    channel,
                    client,
                    control,
                },
            );
        RelayRegistration {
            sink: self.clone(),
            client_id,
        }
    }

    fn unregister(&self, client_id: u64) {
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&client_id);
    }
}

impl RelaySink for WebSocketRelaySink {
    fn relay(&self, event: Relay) {
        // Relay producers are synchronous. Reject on saturation rather than
        // retaining payloads in an unbounded global backlog.
        let _ = self.events.try_send(event);
    }
}

async fn run_relay_worker(
    clients: Arc<Mutex<HashMap<u64, RelayClient>>>,
    processor: Arc<Mutex<Weak<CommandProcessor>>>,
    mut events: tokio::sync::mpsc::Receiver<Relay>,
    cancellation: CancellationToken,
) {
    loop {
        let event = tokio::select! {
            _ = cancellation.cancelled() => return,
            event = events.recv() => {
                let Some(event) = event else { return };
                event
            }
        };
        let target_channel = event
            .except
            .and_then(client_channel)
            .unwrap_or(COMMANDS_NAMESPACE);
        let recipients = {
            let clients = clients.lock().unwrap_or_else(PoisonError::into_inner);
            clients
                .values()
                .filter(|entry| entry.channel == target_channel)
                .filter_map(|entry| {
                    let frame = CommandProcessor::relay_frame(&entry.client, &event)?;
                    Some((entry.client.clone(), entry.control.clone(), frame))
                })
                .collect::<Vec<_>>()
        };
        let processor = processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade();
        let Some(processor) = processor else {
            continue;
        };
        for (client, control, frame) in recipients {
            match processor.process(&client, &frame) {
                CommandAction::Reply(frame) => {
                    if !control.try_send_text(frame) {
                        // A closed connection and a full control queue are both
                        // terminal for this recipient; registration teardown
                        // will remove it without blocking unrelated clients.
                        control.close();
                    }
                }
                CommandAction::ApplyFilters(source) => {
                    if let Some(filters) = parse_command_filters(&source) {
                        control.apply_filters(filters);
                    }
                }
                CommandAction::NoAck => {}
            }
        }
    }
}

fn client_channel(client_id: u64) -> Option<u64> {
    let channel = client_id % 8;
    (1..=COMMANDS_NAMESPACE).contains(&channel).then_some(channel)
}

//! Surface-backed access to the existing latest-only hub, including write acknowledgements.
use super::*;
use vosu_surface::routes::{FeedControl, FeedReceiver, ResponseFuture, SurfaceFeed};
struct Control {
    connection: tokio::sync::Mutex<Connection>,
    control: ConnectionControl,
}
impl FeedControl for Control {
    fn id(&self) -> u64 {
        self.control.id
    }
    fn receive(&self, frame: Inbound) -> ResponseFuture<'_, Option<Bytes>> {
        Box::pin(async move { self.connection.lock().await.receive(frame).await })
    }
    fn replace_filters(&self, encoded: Bytes) -> Result<(), String> {
        if encoded.len() > MAX_MESSAGE_BYTES {
            return Err("filters exceed message limit".into());
        }
        let source = std::str::from_utf8(&encoded).map_err(|e| e.to_string())?;
        let filters =
            vosu_adapter_support::filtered::parse_filters(source).ok_or("invalid filters")?;
        let demand = lock_unpoisoned(&self.control.client.demand);
        if let Some(demand) = demand.as_ref() {
            demand.replace_filters(&filters)?;
        }
        self.control.client.set_filters(filters);
        drop(demand);
        self.control.hub.mark_shared_plans();
        self.control.hub.wake();
        Ok(())
    }
    fn write_completed(&self) {
        if self
            .control
            .client
            .state
            .compare_exchange(
                CLIENT_PENDING,
                CLIENT_OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.control.hub.wake_writable();
        }
    }
    fn send_text(&self, bytes: Bytes) -> ResponseFuture<'_, bool> {
        Box::pin(async move {
            let sent = self
                .control
                .client
                .send_control(Outbound::Reply(bytes))
                .await;
            if !sent {
                self.control.cleanup();
            }
            sent
        })
    }
    fn try_send_text(&self, bytes: Bytes) -> bool {
        self.control.try_send_text(bytes)
    }
    fn close(&self) {
        self.control.cleanup();
    }
    fn is_closed(&self) -> bool {
        self.control.client.cleaned.load(Ordering::Acquire)
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        self.control.cleanup();
    }
}
impl FeedReceiver for OutboundReceiver {
    fn recv(&mut self) -> ResponseFuture<'_, Option<Outbound>> {
        Box::pin(self.recv())
    }
}
impl WebSocketHub {
    pub fn surface_feed(&self, commands: bool) -> SurfaceFeed {
        let (connection, receiver) = if commands {
            self.connect_commands()
        } else {
            self.connect()
        };
        let control = connection.control_handle();
        SurfaceFeed {
            state: Arc::new(Control {
                connection: tokio::sync::Mutex::new(connection),
                control,
            }),
            receiver: Box::new(receiver),
        }
    }
}

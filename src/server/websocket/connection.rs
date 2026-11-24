//! Connection lifecycle, filter demand and write acknowledgements.
use super::*;
impl Connection {
    pub fn id(&self) -> ClientId {
        self.id
    }

    pub fn control_handle(&self) -> ConnectionControl {
        ConnectionControl {
            id: self.id,
            client: Arc::clone(&self.client),
            hub: self.hub.clone(),
        }
    }

    pub fn set_filters(&self, filters: Vec<Filter>) {
        let demand = lock_unpoisoned(&self.client.demand);
        if let Some(demand) = demand.as_ref()
            && demand.replace_filters(&filters).is_err()
        {
            return;
        }
        self.client.set_filters(filters);
        drop(demand);
        self.hub.mark_shared_plans();
        self.hub.wake();
    }

    /// The transport writer must call this after its state send future completes.
    pub fn write_completed(&self) {
        if self
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
            self.hub.wake_writable();
        }
    }

    pub async fn send_text(&self, bytes: Bytes) -> bool {
        let sent = self.client.send_control(Outbound::Reply(bytes)).await;
        if !sent {
            self.cleanup();
        }
        sent
    }

    /// Enqueues a control reply without waiting; callers on shared workers
    /// must not let one full client queue stall unrelated clients.
    pub fn try_send_text(&self, bytes: Bytes) -> bool {
        self.client.state.load(Ordering::Acquire) != CLIENT_CLOSED
            && self.client.control.try_send(Outbound::Reply(bytes)).is_ok()
    }

    /// Consumes a transport frame. Complete text/binary messages are returned
    /// to the command processor; protocol/control frames are handled here.
    pub async fn receive(&mut self, frame: Inbound) -> Option<Bytes> {
        match frame {
            Inbound::Ping(data) => {
                if !self.client.send_control(Outbound::Pong(data)).await {
                    self.cleanup();
                }
                None
            }
            Inbound::Pong(_) => None,
            Inbound::Close | Inbound::Error => {
                self.cleanup();
                None
            }
            Inbound::Text { data, finished } => {
                self.start_fragment(FragmentKind::Text, data, finished)
            }
            Inbound::Binary { data, finished } => {
                self.start_fragment(FragmentKind::Binary, data, finished)
            }
            Inbound::Continuation { data, finished } => self.continue_fragment(data, finished),
        }
    }

    pub fn cleanup(&self) {
        self.finish(None);
    }

    fn finish(&self, terminal: Option<Outbound>) {
        if !self.client.cleaned.load(Ordering::Acquire) {
            match terminal {
                Some(terminal) => self.client.terminate(terminal),
                None => {
                    if !self.client.cleaned.swap(true, Ordering::AcqRel) {
                        self.client.state.store(CLIENT_CLOSED, Ordering::Release);
                        drop(lock_unpoisoned(&self.client.demand).take());
                        self.client.cancellation.cancel();
                    }
                }
            }
            let _write = lock_unpoisoned(&self.hub.0.client_writes);
            let mut clients = HashMap::clone(&self.hub.0.clients.load());
            clients.remove(&self.id);
            mark_shared_plans(&clients);
            self.hub.0.clients.store(Arc::new(clients));
            self.hub.wake();
        }
    }

    fn start_fragment(&mut self, kind: FragmentKind, data: Bytes, finished: bool) -> Option<Bytes> {
        if self.fragments.kind.is_some() {
            self.close_with(PROTOCOL_ERROR, "Protocol error");
            return None;
        }
        if data.len() > MAX_FRAME_BYTES {
            self.close_with(MESSAGE_TOO_BIG, "Message too big");
            return None;
        }
        if finished {
            return Some(data);
        }
        self.fragments.kind = Some(kind);
        self.fragments.bytes.extend_from_slice(&data);
        None
    }

    fn continue_fragment(&mut self, data: Bytes, finished: bool) -> Option<Bytes> {
        if self.fragments.kind.is_none() {
            self.close_with(PROTOCOL_ERROR, "Protocol error");
            return None;
        }
        if data.len() > MAX_FRAME_BYTES
            || self.fragments.bytes.len().saturating_add(data.len()) > MAX_MESSAGE_BYTES
        {
            self.close_with(MESSAGE_TOO_BIG, "Message too big");
            return None;
        }
        self.fragments.bytes.extend_from_slice(&data);
        if finished {
            self.fragments.kind = None;
            return Some(Bytes::from(std::mem::take(&mut self.fragments.bytes)));
        }
        None
    }

    fn close_with(&mut self, code: u16, reason: &'static str) {
        self.fragments = Fragments::default();
        self.finish(Some(Outbound::Close { code, reason }));
    }
}

impl ConnectionControl {
    pub fn set_filters(&self, filters: Vec<Filter>) {
        let demand = lock_unpoisoned(&self.client.demand);
        if let Some(demand) = demand.as_ref()
            && demand.replace_filters(&filters).is_err()
        {
            return;
        }
        self.client.set_filters(filters);
        drop(demand);
        self.hub.mark_shared_plans();
        self.hub.wake();
    }

    /// Enqueues a control reply without waiting; a full recipient queue is
    /// reported to the caller so that recipient can be cleaned up.
    pub fn try_send_text(&self, bytes: Bytes) -> bool {
        self.client.state.load(Ordering::Acquire) != CLIENT_CLOSED
            && self.client.control.try_send(Outbound::Reply(bytes)).is_ok()
    }

    pub fn cleanup(&self) {
        if !self.client.cleaned.swap(true, Ordering::AcqRel) {
            self.client.state.store(CLIENT_CLOSED, Ordering::Release);
            drop(lock_unpoisoned(&self.client.demand).take());
            self.client.cancellation.cancel();
        }
        let _write = lock_unpoisoned(&self.hub.0.client_writes);
        let mut clients = HashMap::clone(&self.hub.0.clients.load());
        clients.remove(&self.id);
        mark_shared_plans(&clients);
        self.hub.0.clients.store(Arc::new(clients));
        self.hub.wake();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.cleanup();
    }
}

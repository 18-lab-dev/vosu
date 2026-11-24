//! Latest-only state-bus publication, independent of the interim B1 bridge.
use crate::project::{self, Caches, Projection, Projector, SessionFacts};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use vosu_osu_state::{Delivery, SnapshotPublisher, StateBundleSnapshot, StateWorkerError};
use vosu_plugin_api::PublishBatchWriter;
use vosu_plugin_api::{HostServices, LogLevel, VtableHostServices, vosu_opcode, vosu_status};

/// Type-erased session host, retaining the allocation-free flat send path.
pub(crate) struct SessionHost(pub Arc<dyn HostServices + Send + Sync>);
impl HostServices for SessionHost {
    fn send(&self, message: &vosu_plugin_api::Message, correlation_id: u64) -> vosu_status {
        self.0.send(message, correlation_id)
    }
    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        self.0.send_flat(opcode, flags, correlation_id, payload)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublicationFailure {
    Encoder,
    Host(vosu_status),
}
struct BusPublisherInner {
    projector: Projector,
    writer: PublishBatchWriter,
    out: Vec<u8>,
    facts: SessionFacts,
    refused: Vec<PublicationFailure>,
    caches: Caches,
    retry: bool,
}
struct Shared {
    inner: Option<BusPublisherInner>,
    pending: Option<Arc<StateBundleSnapshot>>,
    refresh: bool,
}

/// Restores ownership after a caught projection or host-callback panic.
struct RestoreInner<'a> {
    shared: &'a Mutex<Shared>,
    inner: Option<BusPublisherInner>,
}
impl Drop for RestoreInner<'_> {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            inner.projector.invalidate();
            inner.caches = Caches::default();
            inner.retry = true;
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            shared.inner = Some(inner);
        }
    }
}
pub struct BusPublisher<H: HostServices + Send + Sync + 'static = VtableHostServices> {
    host: H,
    handle: u64,
    identified: Arc<AtomicBool>,
    foreground: AtomicU8,
    inner: Mutex<Shared>,
    projections: &'static [Projection],
}
impl<H: HostServices + Send + Sync + 'static> BusPublisher<H> {
    pub fn new(host: H, handle: u64, identified: Arc<AtomicBool>, facts: SessionFacts) -> Self {
        Self {
            host,
            handle,
            identified,
            foreground: AtomicU8::new(encode_foreground(facts.foreground)),
            inner: Mutex::new(Shared {
                inner: Some(BusPublisherInner {
                    projector: Projector::default(),
                    writer: PublishBatchWriter::new(),
                    out: Vec::new(),
                    facts,
                    refused: Vec::new(),
                    caches: Caches::default(),
                    retry: false,
                }),
                pending: None,
                refresh: false,
            }),
            projections: project::projections(),
        }
    }
    pub(crate) fn update_foreground(&self, foreground: Option<bool>) -> bool {
        self.foreground
            .swap(encode_foreground(foreground), Ordering::AcqRel)
            != encode_foreground(foreground)
    }
    pub fn set_foreground(&self, foreground: Option<bool>) {
        if self.update_foreground(foreground) {
            self.republish_session_fields();
        }
    }
    pub fn republish_session_fields(&self) {
        self.enqueue(None, true);
    }

    fn enqueue(&self, snapshot: Option<Arc<StateBundleSnapshot>>, refresh: bool) {
        if !self.identified.load(Ordering::Acquire) {
            return;
        }
        // Lock order: only this short mailbox mutex. No lock is held during
        // projection or host callbacks, which may synchronously re-enter us.
        let (inner, mut current, mut force) = {
            let mut shared = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(snapshot) = snapshot {
                shared.pending = Some(snapshot);
            }
            shared.refresh |= refresh;
            let Some(inner) = shared.inner.take() else {
                return;
            };
            let current = shared
                .pending
                .take()
                .or_else(|| inner.projector.last_snapshot.clone());
            let force = std::mem::take(&mut shared.refresh);
            (inner, current, force)
        };
        let mut owner = RestoreInner {
            shared: &self.inner,
            inner: Some(inner),
        };
        loop {
            if let Some(snapshot) = current.take() {
                let inner = owner
                    .inner
                    .as_mut()
                    .expect("active publisher owns its buffers");
                let reset = inner.projector.last_snapshot.as_ref().is_some_and(|last| {
                    last.generation != snapshot.generation
                        || last.reset_epoch != snapshot.reset_epoch
                });
                if reset {
                    inner.projector.invalidate();
                    inner.caches = Caches::default();
                }
                inner.facts.foreground = match self.foreground.load(Ordering::Acquire) {
                    1 => Some(false),
                    2 => Some(true),
                    _ => None,
                };
                let unchanged = inner.projector.last_snapshot.as_ref().is_some_and(|last| {
                    Arc::ptr_eq(&last.regular, &snapshot.regular)
                        && Arc::ptr_eq(&last.precise, &snapshot.precise)
                });
                if force || reset || inner.retry || !unchanged {
                    inner.projector.prepare(&snapshot);
                    // Retain the attempted snapshot so a refresh after an unwind
                    // can retry even the first publication.
                    inner.projector.last_snapshot = Some(snapshot.clone());
                    inner.writer.begin(self.handle);
                    inner.projector.project_into(
                        &snapshot,
                        &inner.facts,
                        &mut inner.writer,
                        self.projections,
                        &mut inner.caches,
                    );
                    if let Some(error) = inner.caches.take_error() {
                        self.host.log(
                            LogLevel::Warn,
                            &format!("state projection IO failed: {error}"),
                            vosu_plugin_api::CborValue::Null,
                        );
                    }
                    if inner.caches.take_tourney_unavailable() {
                        self.host.log(
                            LogLevel::Debug,
                            "tournament projection unavailable: client score is missing",
                            vosu_plugin_api::CborValue::Null,
                        );
                    }
                    let mut accepted = true;
                    if !inner.writer.is_empty() {
                        if let Err(error) = inner.writer.finish_into(&mut inner.out) {
                            accepted = false;
                            self.log_refusal(
                                inner,
                                PublicationFailure::Encoder,
                                &error.to_string(),
                            );
                        } else {
                            let status = self.host.send_flat(
                                vosu_opcode::VOSU_OP_PUBLISH_BATCH,
                                0,
                                0,
                                &inner.out,
                            );
                            if status != vosu_status::VOSU_OK {
                                accepted = false;
                                self.log_refusal(
                                    inner,
                                    PublicationFailure::Host(status),
                                    "host refused state batch",
                                );
                            }
                        }
                    }
                    inner.projector.last_snapshot = Some(snapshot);
                    inner.retry = !accepted;
                    if !accepted {
                        inner.projector.invalidate();
                    }
                }
            }
            let mut shared = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if shared.pending.is_none() && !shared.refresh {
                shared.inner = owner.inner.take();
                return;
            }
            current = shared.pending.take().or_else(|| {
                owner
                    .inner
                    .as_ref()
                    .and_then(|inner| inner.projector.last_snapshot.clone())
            });
            force = std::mem::take(&mut shared.refresh);
        }
    }
    fn log_refusal(
        &self,
        inner: &mut BusPublisherInner,
        failure: PublicationFailure,
        reason: &str,
    ) {
        if !inner.refused.contains(&failure) {
            inner.refused.push(failure);
            self.host.log(
                LogLevel::Warn,
                &format!("state publication failed ({failure:?}): {reason}"),
                vosu_plugin_api::CborValue::Null,
            );
        }
    }
}
fn encode_foreground(value: Option<bool>) -> u8 {
    match value {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }
}
impl<H: HostServices + Send + Sync + 'static> SnapshotPublisher<Arc<StateBundleSnapshot>>
    for BusPublisher<H>
{
    fn publish(
        &self,
        snapshot: Arc<StateBundleSnapshot>,
        _: Delivery,
    ) -> Result<(), StateWorkerError> {
        self.enqueue(Some(snapshot), false);
        Ok(())
    }
}
/// The legacy sink always receives its publication first. Bus refusals are
/// handled by BusPublisher and never turn a successful B1 write into failure.
pub(crate) struct FanoutPublisher {
    pub b1: Option<crate::bridge_b1::B1Publisher>,
    pub bus: Arc<crate::publish::BusPublisher<crate::publish::SessionHost>>,
}
impl SnapshotPublisher<Arc<StateBundleSnapshot>> for FanoutPublisher {
    fn publish(
        &self,
        snapshot: Arc<StateBundleSnapshot>,
        delivery: Delivery,
    ) -> Result<(), StateWorkerError> {
        let result = self
            .b1
            .as_ref()
            .map_or(Ok(()), |b1| b1.publish(Arc::clone(&snapshot), delivery));
        let _ = self.bus.publish(snapshot, delivery);
        result
    }
}

#[cfg(test)]
mod tests;

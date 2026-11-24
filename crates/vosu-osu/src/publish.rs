//! Latest-only state-bus publication, independent of the interim B1 bridge.
use crate::project::{self, Projection, Projector, SessionFacts};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
use vosu_osu_state::{Delivery, SnapshotPublisher, StateBundleSnapshot, StateWorkerError};
use vosu_plugin_api::PublishBatchWriter;
use vosu_plugin_api::{HostServices, LogLevel, VtableHostServices, vosu_opcode, vosu_status};

struct BusPublisherInner {
    projector: Projector,
    writer: PublishBatchWriter,
    out: Vec<u8>,
    facts: SessionFacts,
    refused: Vec<vosu_status>,
    retry: bool,
}
struct Shared {
    inner: Option<BusPublisherInner>,
    pending: Option<Arc<StateBundleSnapshot>>,
    refresh: bool,
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
                    retry: false,
                }),
                pending: None,
                refresh: false,
            }),
            projections: project::projections(),
        }
    }
    pub fn set_foreground(&self, foreground: Option<bool>) {
        if self
            .foreground
            .swap(encode_foreground(foreground), Ordering::AcqRel)
            != encode_foreground(foreground)
        {
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
        let (mut inner, mut current, mut force) = {
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
        loop {
            if let Some(snapshot) = current.take() {
                inner.facts.foreground = match self.foreground.load(Ordering::Acquire) {
                    1 => Some(false),
                    2 => Some(true),
                    _ => None,
                };
                let unchanged = inner.projector.last_snapshot.as_ref().is_some_and(|last| {
                    Arc::ptr_eq(&last.regular, &snapshot.regular)
                        && Arc::ptr_eq(&last.precise, &snapshot.precise)
                });
                if force || inner.retry || !unchanged {
                    inner.writer.begin(self.handle);
                    inner.projector.project_into(
                        &snapshot,
                        &inner.facts,
                        &mut inner.writer,
                        self.projections,
                    );
                    let mut accepted = true;
                    if !inner.writer.is_empty() {
                        if let Err(error) = inner.writer.finish_into(&mut inner.out) {
                            accepted = false;
                            self.log_refusal(
                                &mut inner,
                                vosu_status::VOSU_ERR_INVALID_ARGUMENT,
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
                                self.log_refusal(&mut inner, status, "host refused state batch");
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
                shared.inner = Some(inner);
                return;
            }
            current = shared
                .pending
                .take()
                .or_else(|| inner.projector.last_snapshot.clone());
            force = std::mem::take(&mut shared.refresh);
        }
    }
    fn log_refusal(&self, inner: &mut BusPublisherInner, status: vosu_status, reason: &str) {
        if !inner.refused.contains(&status) {
            inner.refused.push(status);
            self.host.log(
                LogLevel::Warn,
                &format!("state publication refused ({status:?}): {reason}"),
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

#[cfg(test)]
mod tests;

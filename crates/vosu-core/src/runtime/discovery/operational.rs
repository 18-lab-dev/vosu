//! Daemon-specific discovery progress over the generic process source.
use crate::{
    logging::Logger,
    plugin::{Prefilter, ProcessEventBatch, ProcessRecord, ProcessSource},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(in crate::runtime) struct LoggedSource {
    source: Arc<dyn ProcessSource>,
    logger: Logger,
    initial_reported: AtomicBool,
}
impl LoggedSource {
    pub(in crate::runtime) fn new(source: Arc<dyn ProcessSource>, logger: Logger) -> Self {
        Self {
            source,
            logger,
            initial_reported: AtomicBool::new(false),
        }
    }
}
impl ProcessSource for LoggedSource {
    fn enumerate(&self) -> std::io::Result<Vec<ProcessRecord>> {
        let candidates = self.source.enumerate()?;
        if !self.initial_reported.swap(true, Ordering::AcqRel) {
            self.logger.info(format!(
                "Initial process discovery completed: {} osu! process candidate(s)",
                candidates.len()
            ));
        }
        Ok(candidates)
    }
    fn inspect(&self, pid: u32) -> std::io::Result<Option<ProcessRecord>> {
        self.source.inspect(pid)
    }
    fn drain_events(&self) -> ProcessEventBatch {
        self.source.drain_events()
    }
    fn set_prefilter(&self, filter: &Prefilter) -> std::io::Result<()> {
        self.source.set_prefilter(filter)
    }
    fn notifier(&self) -> Option<&tokio::sync::Notify> {
        self.source.notifier()
    }
    fn backend_name(&self) -> &'static str {
        self.source.backend_name()
    }
    fn events_healthy(&self) -> bool {
        self.source.events_healthy()
    }
    fn event_count(&self) -> u64 {
        self.source.event_count()
    }
    fn reconciliation_request_count(&self) -> u64 {
        self.source.reconciliation_request_count()
    }
    fn is_alive(&self, pid: u32) -> bool {
        self.source.is_alive(pid)
    }
}

//! Fixed-window budgets over the process's static tracing callsites.
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tracing::{Level, Metadata, callsite::Identifier};
use vosu_core::logging::Logger;

pub(super) const WINDOW: Duration = Duration::from_secs(1);
pub(super) const LINES_PER_WINDOW: u32 = 20;

struct Budget {
    metadata: &'static Metadata<'static>,
    started: Instant,
    sent: u32,
    suppressed: u64,
}

pub(super) struct RateLimiter {
    pub(super) logger: Logger,
    window: Duration,
    budgets: Mutex<HashMap<Identifier, Budget>>,
}
impl RateLimiter {
    pub(super) fn new(logger: Logger, window: Duration) -> Self {
        Self {
            logger,
            window,
            budgets: Mutex::default(),
        }
    }
    pub(super) fn permit(&self, metadata: &'static Metadata<'static>, now: Instant) -> bool {
        let (allowed, summary) = {
            let mut budgets = self
                .budgets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Keys and metadata belong to static callsites, not message strings.
            // Only a callsite's first event allocates; suppressed events do not
            // record/format their fields or allocate anything.
            let budget = budgets
                .entry(metadata.callsite())
                .or_insert_with(|| Budget {
                    metadata,
                    started: now,
                    sent: 0,
                    suppressed: 0,
                });
            let summary = if now.duration_since(budget.started) >= self.window {
                budget.started = now;
                budget.sent = 0;
                std::mem::take(&mut budget.suppressed)
            } else {
                0
            };
            let allowed = budget.sent < LINES_PER_WINDOW;
            if allowed {
                budget.sent += 1;
            } else {
                budget.suppressed = budget.suppressed.saturating_add(1);
            }
            (allowed, summary)
        };
        self.report(metadata, summary);
        allowed
    }
    pub(super) fn flush(
        &self,
        now: Instant,
        summaries: &mut Vec<(&'static Metadata<'static>, u64)>,
    ) {
        {
            let mut budgets = self
                .budgets
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for budget in budgets.values_mut() {
                if now.duration_since(budget.started) >= self.window {
                    let count = std::mem::take(&mut budget.suppressed);
                    if count != 0 {
                        summaries.push((budget.metadata, count));
                    }
                    budget.started = now;
                    budget.sent = 0;
                }
            }
        }
        // Never format messages or call the Logger while holding the budget lock.
        for (metadata, count) in summaries.drain(..) {
            self.report(metadata, count);
        }
    }
    fn report(&self, metadata: &'static Metadata<'static>, count: u64) {
        if count == 0 {
            return;
        }
        let message = format!(
            "suppressed {count} similar messages from {} ({}:{})",
            metadata.target(),
            metadata.file().unwrap_or("unknown"),
            metadata.line().unwrap_or_default()
        );
        match *metadata.level() {
            Level::ERROR => self.logger.error(message),
            Level::WARN => self.logger.warn(message),
            _ => self.logger.info(message),
        }
    }
}

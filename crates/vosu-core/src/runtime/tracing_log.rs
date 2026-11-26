//! Forward library diagnostics into the daemon's bounded, non-blocking logger.
use std::cell::Cell;
use std::fmt::Write;
use std::sync::Arc;
use std::time::Instant;

mod rate_limit;
use rate_limit::{RateLimiter, WINDOW};
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, Registry, layer::Context, prelude::*};
use crate::logging::Logger;

thread_local! { static FORWARDING: Cell<bool> = const { Cell::new(false) }; }

struct LoggerLayer(Arc<RateLimiter>);

#[derive(Default)]
struct Fields {
    message: String,
    values: String,
}
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.values, " {}={value:?}", field.name());
        }
    }
}

impl<S: Subscriber> Layer<S> for LoggerLayer {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _: Context<'_, S>) -> bool {
        matches!(*metadata.level(), Level::WARN | Level::ERROR)
            || (*metadata.level() == Level::INFO && metadata.target().starts_with("vosu_"))
    }
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        FORWARDING.with(|active| {
            if active.replace(true) {
                return;
            }
            struct Reset<'a>(&'a Cell<bool>);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    self.0.set(false);
                }
            }
            let _reset = Reset(active);
            if !self.0.permit(event.metadata(), Instant::now()) {
                return;
            }
            let mut fields = Fields::default();
            event.record(&mut fields);
            fields.message.push_str(&fields.values);
            // Logger uses try_send to its bounded writer queue; this never
            // waits for console or disk I/O. The guard also prevents recursion
            // from a field's Debug implementation or future logger internals.
            match *event.metadata().level() {
                Level::ERROR => self.0.logger.error(fields.message),
                Level::WARN => self.0.logger.warn(fields.message),
                Level::INFO => self.0.logger.info(fields.message),
                _ => {}
            }
        });
    }
}

pub(super) fn install(logger: Logger) {
    let limiter = Arc::new(RateLimiter::new(logger, WINDOW));
    let weak = Arc::downgrade(&limiter);
    // The process owns a single subscriber. Test harnesses may already own one.
    if tracing::subscriber::set_global_default(Registry::default().with(LoggerLayer(limiter)))
        .is_ok()
    {
        tokio::spawn(async move {
            let mut summaries = Vec::new();
            loop {
                tokio::time::sleep(WINDOW).await;
                let Some(limiter) = weak.upgrade() else { break };
                // Flush even when a noisy callsite stops emitting completely.
                limiter.flush(Instant::now(), &mut summaries);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    use crate::logging::{LoggerOptions, StdoutConsole};

    #[tokio::test]
    async fn core_warnings_and_fields_reach_daemon_log_with_level_filtering() {
        let root = tempfile::tempdir().unwrap();
        let logger = Logger::new(
            root.path(),
            LoggerOptions::default(),
            Arc::new(StdoutConsole),
        )
        .unwrap();
        let subscriber = Registry::default().with(LoggerLayer(Arc::new(RateLimiter::new(
            logger.clone(),
            WINDOW,
        ))));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "vosu_core::plugin::discovery", pid = 4242, error = "denied", "core discovery warning");
            tracing::error!(target: "foreign_library", "foreign error");
            tracing::info!(target: "vosu_core::plugin", "core info");
            tracing::info!(target: "foreign_library", "foreign info must be filtered");
            tracing::debug!(target: "vosu_core::plugin", "debug must be filtered");
        });
        tokio::time::timeout(Duration::from_secs(2), logger.shutdown())
            .await
            .unwrap()
            .unwrap();
        let log = std::fs::read_to_string(root.path().join("latest.log")).unwrap();
        assert!(log.contains("core discovery warning pid=4242 error=\"denied\""));
        assert!(log.contains("foreign error"));
        assert!(log.contains("core info"));
        assert!(!log.contains("must be filtered"));
    }

    #[tokio::test]
    async fn identical_error_flood_is_bounded_and_summarized_once_per_window() {
        let root = tempfile::tempdir().unwrap();
        let logger = Logger::new(
            root.path(),
            LoggerOptions::default(),
            Arc::new(StdoutConsole),
        )
        .unwrap();
        // A long window makes this test independent of wall-clock scheduling.
        // Expiry is advanced explicitly; no sleep or repeating-timer rendezvous.
        let window = Duration::from_secs(3600);
        let limiter = Arc::new(RateLimiter::new(logger.clone(), window));
        let subscriber = Registry::default().with(LoggerLayer(limiter.clone()));
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..10_000 {
                tracing::error!(target: "vosu_osu_state::flood", "identical calculation failure");
            }
            tracing::warn!(target: "independent_callsite", "unrelated warning");
        });
        let mut summaries = Vec::new();
        let expired = Instant::now() + window;
        limiter.flush(expired, &mut summaries);
        limiter.flush(expired, &mut summaries);
        tokio::time::timeout(Duration::from_secs(2), logger.shutdown())
            .await
            .unwrap()
            .unwrap();
        let log = std::fs::read_to_string(root.path().join("latest.log")).unwrap();
        assert_eq!(
            log.matches("identical calculation failure").count(),
            rate_limit::LINES_PER_WINDOW as usize
        );
        assert_eq!(log.matches("suppressed 9980 similar messages").count(), 1);
        assert_eq!(log.matches("unrelated warning").count(), 1);
    }
}

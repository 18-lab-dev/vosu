//! Forward library diagnostics into the daemon's bounded, non-blocking logger.
use std::cell::Cell;
use std::fmt::Write;
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, Registry, layer::Context, prelude::*};
use vosu_core::logging::Logger;

thread_local! { static FORWARDING: Cell<bool> = const { Cell::new(false) }; }

struct LoggerLayer(Logger);

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
            let mut fields = Fields::default();
            event.record(&mut fields);
            fields.message.push_str(&fields.values);
            // Logger uses try_send to its bounded writer queue; this never
            // waits for console or disk I/O. The guard also prevents recursion
            // from a field's Debug implementation or future logger internals.
            match *event.metadata().level() {
                Level::ERROR => self.0.error(fields.message),
                Level::WARN => self.0.warn(fields.message),
                Level::INFO => self.0.info(fields.message),
                _ => {}
            }
        });
    }
}

pub(super) fn install(logger: Logger) {
    // The process owns a single subscriber. Test harnesses may already own one.
    let _ = tracing::subscriber::set_global_default(Registry::default().with(LoggerLayer(logger)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    use vosu_core::logging::{LoggerOptions, StdoutConsole};

    #[tokio::test]
    async fn core_warnings_and_fields_reach_daemon_log_with_level_filtering() {
        let root = tempfile::tempdir().unwrap();
        let logger = Logger::new(
            root.path(),
            LoggerOptions::default(),
            Arc::new(StdoutConsole),
        )
        .unwrap();
        let subscriber = Registry::default().with(LoggerLayer(logger.clone()));
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
}

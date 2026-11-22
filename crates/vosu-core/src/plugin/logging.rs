//! Host-side `VOSU_OP_LOG_RECORD` routing (`docs/v2/plugin-abi.md` §6.10):
//! attributes every record to its plugin instance, writes it through the
//! daemon's existing logging sink (`crate::logging::Logger`), and enforces
//! the per-instance 200/second rate limit, with a single dropped-count
//! summary logged once per second while dropping continues.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vosu_plugin_api::{CborValue, LogLevel as PluginLogLevel, LogRecord};

use crate::logging::{LogLevel as DaemonLogLevel, Logger};

use super::host_services::PluginInstanceId;

/// §6.10's per-instance rate limit: records past this many in the current
/// one-second window are dropped and counted instead of written.
pub const LOG_RATE_LIMIT_PER_SECOND: u32 = 200;

/// The window both the rate limit and the dropped-count summary share:
/// §6.10 states the limit as "200/second" and the summary as "logged once
/// per second while dropping continues", so one rolling window serves
/// both rather than tracking them on independent clocks.
const WINDOW: Duration = Duration::from_secs(1);

struct InstanceState {
    window_start: Instant,
    window_count: u32,
    dropped_since_summary: u64,
}

impl InstanceState {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            window_count: 0,
            dropped_since_summary: 0,
        }
    }
}

/// `crate::logging::LogLevel` has no `Trace` variant (it predates this ABI
/// and matches 1.x's own file-rotation levels); `trace` and `debug` both
/// map to `Debug`, the closest existing level, rather than inventing a
/// seventh log file this host never reads from elsewhere. Recorded as a
/// spec-adjacent gap in this phase's handoff notes, not a silent
/// approximation.
fn map_level(level: PluginLogLevel) -> DaemonLogLevel {
    match level {
        PluginLogLevel::Trace | PluginLogLevel::Debug => DaemonLogLevel::Debug,
        PluginLogLevel::Info => DaemonLogLevel::Info,
        PluginLogLevel::Warn => DaemonLogLevel::Warn,
        PluginLogLevel::Error => DaemonLogLevel::Error,
    }
}

/// Routes every loaded instance's `LogRecord`s into one shared
/// `crate::logging::Logger`, keeping one rate-limit window per instance so
/// a runaway plugin cannot starve another's budget.
pub struct PluginLogRouter {
    logger: Logger,
    state: Mutex<HashMap<PluginInstanceId, InstanceState>>,
}

impl PluginLogRouter {
    pub fn new(logger: Logger) -> Self {
        Self {
            logger,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Handles one `VOSU_OP_LOG_RECORD` (plugin -> host, no reply): writes
    /// it through the daemon logger when `instance` is still within its
    /// budget for the current window, otherwise drops it and counts it
    /// toward the next summary line. Returns `true` when the record was
    /// written.
    pub fn record(&self, instance: PluginInstanceId, record: LogRecord) -> bool {
        let now = Instant::now();
        let dropped_summary = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let state = guard
                .entry(instance)
                .or_insert_with(|| InstanceState::new(now));
            if now.duration_since(state.window_start) >= WINDOW {
                state.window_start = now;
                state.window_count = 0;
                let dropped = std::mem::take(&mut state.dropped_since_summary);
                if dropped > 0 { Some(dropped) } else { None }
            } else {
                None
            }
        };
        if let Some(dropped) = dropped_summary {
            self.logger.warn(format!(
                "plugin instance {instance:?} exceeded the log rate limit ({LOG_RATE_LIMIT_PER_SECOND}/s): {dropped} record(s) dropped in the last second"
            ));
        }
        let accepted = {
            let mut guard = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let state = guard
                .entry(instance)
                .or_insert_with(|| InstanceState::new(now));
            if state.window_count >= LOG_RATE_LIMIT_PER_SECOND {
                state.dropped_since_summary += 1;
                false
            } else {
                state.window_count += 1;
                true
            }
        };
        if accepted {
            self.write(instance, record);
        }
        accepted
    }

    fn write(&self, instance: PluginInstanceId, record: LogRecord) {
        let level = map_level(record.level);
        let fields_suffix = match &record.fields {
            CborValue::Map(entries) if !entries.is_empty() => format!(" {entries:?}"),
            _ => String::new(),
        };
        self.logger.log(
            level,
            format!("plugin {instance:?}: {}{fields_suffix}", record.message),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;

    use tempfile::tempdir;

    use crate::logging::{ConsoleSink, LoggerOptions};

    use super::*;

    #[derive(Default)]
    struct RecordingConsole {
        lines: Mutex<Vec<String>>,
    }

    impl ConsoleSink for RecordingConsole {
        fn is_tty(&self) -> bool {
            false
        }
        fn write_log(&self, text: &str) -> io::Result<()> {
            self.lines
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(text.to_owned());
            Ok(())
        }
        fn clear_progress(&self, _lines: usize) -> io::Result<()> {
            Ok(())
        }
        fn write_progress(&self, _text: &str) -> io::Result<()> {
            Ok(())
        }
    }

    fn record(message: &str) -> LogRecord {
        LogRecord {
            level: PluginLogLevel::Info,
            message: message.to_owned(),
            fields: CborValue::Map(Vec::new()),
        }
    }

    async fn router() -> (Arc<RecordingConsole>, PluginLogRouter) {
        let dir = tempdir().unwrap();
        let console = Arc::new(RecordingConsole::default());
        let logger = Logger::new(
            dir.path(),
            LoggerOptions {
                version: "0.1.0".to_owned(),
                debug_log: true,
                queue_capacity: 4096,
            },
            console.clone(),
        )
        .unwrap();
        // Keeps `dir` alive for the logger's whole lifetime in each test by
        // leaking it: the logger owns no reference to it directly, only
        // the open file it already created.
        std::mem::forget(dir);
        (console, PluginLogRouter::new(logger))
    }

    #[tokio::test]
    async fn accepts_up_to_the_per_second_limit_and_drops_past_it() {
        let (_console, router) = router().await;
        let instance = PluginInstanceId::new(1);
        let mut accepted = 0;
        for i in 0..(LOG_RATE_LIMIT_PER_SECOND + 50) {
            if router.record(instance, record(&format!("line {i}"))) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, LOG_RATE_LIMIT_PER_SECOND);
    }

    #[tokio::test]
    async fn one_instance_exceeding_the_limit_does_not_affect_another() {
        let (_console, router) = router().await;
        let noisy = PluginInstanceId::new(1);
        let quiet = PluginInstanceId::new(2);
        for i in 0..(LOG_RATE_LIMIT_PER_SECOND + 10) {
            router.record(noisy, record(&format!("noisy {i}")));
        }
        assert!(router.record(quiet, record("quiet")));
    }

    #[tokio::test]
    async fn dropped_records_are_summarized_once_the_window_elapses() {
        let (console, router) = router().await;
        let instance = PluginInstanceId::new(7);
        for i in 0..(LOG_RATE_LIMIT_PER_SECOND + 25) {
            router.record(instance, record(&format!("line {i}")));
        }
        // Force the next call to observe an elapsed window without a real
        // sleep by waiting out the real window instead: `Instant` gives no
        // way to fast-forward it deterministically.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        router.record(instance, record("after the window"));
        router.logger.flush().await.unwrap();
        let lines = console.lines.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("25 record(s) dropped")),
            "expected a dropped-record summary line, got: {lines:?}"
        );
    }
}

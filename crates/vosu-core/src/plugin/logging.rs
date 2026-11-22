//! Host-side `VOSU_OP_LOG_RECORD` routing (`docs/v2/plugin-abi.md` §6.10):
//! attributes every record to its plugin id (and session, where the
//! caller supplies one), writes it through the daemon's existing logging
//! sink (`crate::logging::Logger`) with structured fields rendered
//! readably and plugin-provided text escaped, and enforces the
//! per-instance 200/second rate limit, with a single dropped-count
//! summary flushed once per second on a background tick, never waiting
//! on the next record to arrive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use vosu_plugin_api::{CborValue, LogLevel as PluginLogLevel, LogRecord};

use crate::logging::{LogLevel as DaemonLogLevel, Logger};

use super::host_services::PluginInstanceId;

/// §6.10's per-instance rate limit: records past this many in the current
/// one-second window are dropped and counted instead of written.
pub const LOG_RATE_LIMIT_PER_SECOND: u32 = 200;

/// The rate-limit window and the dropped-count summary cadence: §6.10
/// states the limit as "200/second" and the summary as "logged once per
/// second while dropping continues".
const WINDOW: Duration = Duration::from_secs(1);

struct InstanceState {
    window_start: Instant,
    window_count: u32,
    dropped_since_summary: u64,
    last_summary: Instant,
}

impl InstanceState {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            window_count: 0,
            dropped_since_summary: 0,
            last_summary: now,
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

/// Plugin-provided text is not fully trusted (`--plugin <dir>` development
/// plugins, and any plugin's own bugs): escapes newlines and carriage
/// returns (which could otherwise forge whole extra log lines), every
/// other ASCII control character including raw ANSI escapes (`\x1b`...),
/// and the daemon logger's own `%...%` highlight markup delimiter so
/// plugin text can never be interpreted as markup or styled output.
fn escape_plugin_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            // A visually similar, byte-distinct character: the logger's
            // `transform_highlights` only ever matches the ASCII '%'
            // (0x25), so this can never pair up as markup.
            '%' => out.push('\u{FF05}'),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Renders one CBOR scalar/collection readably for a log line, rather
/// than ciborium's `Value` `Debug` dump. Plugin-provided text leaves are
/// escaped the same way the top-level message is.
fn render_cbor(value: &CborValue) -> String {
    match value {
        CborValue::Integer(i) => i128::from(*i).to_string(),
        CborValue::Float(f) => f.to_string(),
        CborValue::Bool(b) => b.to_string(),
        CborValue::Null => "null".to_owned(),
        CborValue::Text(s) => escape_plugin_text(s),
        CborValue::Bytes(b) => format!("<{} bytes>", b.len()),
        CborValue::Array(items) => {
            let rendered: Vec<String> = items.iter().map(render_cbor).collect();
            format!("[{}]", rendered.join(", "))
        }
        CborValue::Map(entries) => {
            let rendered: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}={}", render_cbor(k), render_cbor(v)))
                .collect();
            format!("{{{}}}", rendered.join(", "))
        }
        other => format!("{other:?}"),
    }
}

/// Renders `VOSU_OP_LOG_RECORD`'s `fields` as a trailing `key=value ...`
/// suffix (empty string when there are no fields), never as a raw
/// `Debug` dump of the CBOR value.
fn render_fields(fields: &CborValue) -> String {
    match fields {
        CborValue::Map(entries) if !entries.is_empty() => {
            let rendered: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}={}", render_cbor(k), render_cbor(v)))
                .collect();
            format!(" {}", rendered.join(" "))
        }
        _ => String::new(),
    }
}

/// Routes every loaded instance's `LogRecord`s into one shared
/// `crate::logging::Logger`, keeping one rate-limit window per instance so
/// a runaway plugin cannot starve another's budget. Owns a background
/// task (see `new`) that flushes any instance's pending dropped-count
/// summary once a second even if that instance sends nothing more.
pub struct PluginLogRouter {
    inner: Arc<RouterInner>,
}

struct RouterInner {
    logger: Logger,
    state: Mutex<HashMap<PluginInstanceId, InstanceState>>,
}

impl PluginLogRouter {
    /// `runtime` is captured once (typically `Handle::current()` at
    /// startup) to spawn the background summary-flush task below; it
    /// holds only a `Weak` reference to this router's shared state, so
    /// the task exits on its own once every `PluginLogRouter` clone of
    /// that state is dropped.
    pub fn new(logger: Logger, runtime: Handle) -> Self {
        let inner = Arc::new(RouterInner {
            logger,
            state: Mutex::new(HashMap::new()),
        });
        runtime.spawn(flush_task(Arc::downgrade(&inner)));
        Self { inner }
    }

    /// Handles one `VOSU_OP_LOG_RECORD` (plugin -> host, no reply):
    /// writes it through the daemon logger, attributed to `plugin_id`
    /// (and `session`, when the caller has one for this instance — this
    /// module has no instance or session registry of its own), when
    /// `instance` is still within its budget for the current window,
    /// otherwise drops it and counts it toward the next summary line.
    /// Returns `true` when the record was written.
    pub fn record(
        &self,
        instance: PluginInstanceId,
        plugin_id: &str,
        session: Option<&str>,
        record: LogRecord,
    ) -> bool {
        let now = Instant::now();
        let accepted = {
            let mut guard = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let state = guard
                .entry(instance)
                .or_insert_with(|| InstanceState::new(now));
            if now.duration_since(state.window_start) >= WINDOW {
                state.window_start = now;
                state.window_count = 0;
            }
            if state.window_count >= LOG_RATE_LIMIT_PER_SECOND {
                state.dropped_since_summary += 1;
                false
            } else {
                state.window_count += 1;
                true
            }
        };
        if accepted {
            self.write(plugin_id, session, record);
        }
        accepted
    }

    fn write(&self, plugin_id: &str, session: Option<&str>, record: LogRecord) {
        let level = map_level(record.level);
        let session_suffix = session
            .map(|session| format!(" session={}", escape_plugin_text(session)))
            .unwrap_or_default();
        let message = escape_plugin_text(&record.message);
        let fields_suffix = render_fields(&record.fields);
        self.inner.logger.log(
            level,
            format!("plugin {plugin_id}{session_suffix}: {message}{fields_suffix}"),
        );
    }
}

/// Flushes any instance's dropped-count summary once its window has had a
/// full second to accumulate drops, on this ticker's own one-second
/// cadence — never waiting for that instance to send another record.
/// Exits once `inner` can no longer be upgraded (the owning
/// `PluginLogRouter` was dropped).
async fn flush_task(inner: Weak<RouterInner>) {
    let mut ticker = tokio::time::interval(WINDOW);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let due: Vec<(PluginInstanceId, u64)> = {
            let mut guard = inner
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let now = Instant::now();
            guard
                .iter_mut()
                .filter_map(|(instance, state)| {
                    if state.dropped_since_summary > 0
                        && now.duration_since(state.last_summary) >= WINDOW
                    {
                        let dropped = std::mem::take(&mut state.dropped_since_summary);
                        state.last_summary = now;
                        Some((*instance, dropped))
                    } else {
                        None
                    }
                })
                .collect()
        };
        for (instance, dropped) in due {
            inner.logger.warn(format!(
                "plugin instance {instance:?} exceeded the log rate limit ({LOG_RATE_LIMIT_PER_SECOND}/s): {dropped} record(s) dropped in the last second"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;

    use tempfile::{TempDir, tempdir};

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

    /// Returns the backing `TempDir` too, so the caller keeps it alive for
    /// exactly this test's scope and it is removed on drop at the end of
    /// the test function, instead of leaking it into `/tmp` forever.
    async fn router() -> (TempDir, Arc<RecordingConsole>, Logger, PluginLogRouter) {
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
        let router = PluginLogRouter::new(logger.clone(), Handle::current());
        (dir, console, logger, router)
    }

    #[tokio::test]
    async fn accepts_up_to_the_per_second_limit_and_drops_past_it() {
        let (_dir, _console, _logger, router) = router().await;
        let instance = PluginInstanceId::new(1);
        let mut accepted = 0;
        for i in 0..(LOG_RATE_LIMIT_PER_SECOND + 50) {
            if router.record(instance, "vosu.test", None, record(&format!("line {i}"))) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, LOG_RATE_LIMIT_PER_SECOND);
    }

    #[tokio::test]
    async fn one_instance_exceeding_the_limit_does_not_affect_another() {
        let (_dir, _console, _logger, router) = router().await;
        let noisy = PluginInstanceId::new(1);
        let quiet = PluginInstanceId::new(2);
        for i in 0..(LOG_RATE_LIMIT_PER_SECOND + 10) {
            router.record(noisy, "vosu.test", None, record(&format!("noisy {i}")));
        }
        assert!(router.record(quiet, "vosu.test", None, record("quiet")));
    }

    #[tokio::test]
    async fn dropped_records_are_summarized_once_per_second_without_another_record() {
        let (_dir, console, logger, router) = router().await;
        let instance = PluginInstanceId::new(7);
        for i in 0..(LOG_RATE_LIMIT_PER_SECOND + 25) {
            router.record(instance, "vosu.test", None, record(&format!("line {i}")));
        }
        // No further `record` call: the background flush task, not the
        // next record, must surface the summary once its own one-second
        // tick elapses.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        logger.flush().await.unwrap();
        let lines = console.lines.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("25 record(s) dropped")),
            "expected a dropped-record summary line, got: {lines:?}"
        );
    }

    #[tokio::test]
    async fn log_lines_are_attributed_by_plugin_id_and_session() {
        let (_dir, console, logger, router) = router().await;
        let instance = PluginInstanceId::new(3);
        assert!(router.record(instance, "vosu.osu", Some("session-abc"), record("hello")));
        logger.flush().await.unwrap();
        let lines = console.lines.lock().unwrap();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("plugin vosu.osu session=session-abc: hello")),
            "expected plugin id and session attribution, got: {lines:?}"
        );
    }

    #[tokio::test]
    async fn fields_render_readably_and_plugin_text_is_escaped() {
        let (_dir, console, logger, router) = router().await;
        let instance = PluginInstanceId::new(4);
        let message = "line one\nline two\r%fake-markup%".to_owned();
        let fields = CborValue::Map(vec![(
            CborValue::Text("count".to_owned()),
            CborValue::Integer(3.into()),
        )]);
        router.record(
            instance,
            "vosu.test",
            None,
            LogRecord {
                level: PluginLogLevel::Info,
                message,
                fields,
            },
        );
        logger.flush().await.unwrap();
        let lines = console.lines.lock().unwrap();
        let line = lines
            .iter()
            .find(|line| line.contains("vosu.test"))
            .unwrap_or_else(|| panic!("expected a line for vosu.test, got: {lines:?}"));
        assert!(!line.contains('\n') || line.ends_with('\n'), "{line}");
        assert!(line.contains("line one\\nline two\\r"), "{line}");
        assert!(!line.contains("%fake-markup%"), "{line}");
        assert!(line.contains("count=3"), "{line}");
    }
}

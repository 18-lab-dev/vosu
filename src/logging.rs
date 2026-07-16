//! Non-blocking daemon logging and retained log management.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

mod retention;

pub use retention::{CleanupReport, cleanup_logs, rotate_latest};

const RESET: &str = "\x1b[0m";
const GREY: &str = "\x1b[38;2;170;170;170m";
const GREEN_TEXT: &str = "\x1b[38;2;60;200;100m";
const HIGHLIGHT: &str = "\x1b[1m\x1b[37m";
const SEPARATOR: &str = "┃";
const PROGRESS_FILLED: &str = "━";
const PROGRESS_EMPTY: &str = "─";
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

/// Source-compatible log severity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    DebugError,
    Debug,
    Time,
}

impl LogLevel {
    fn file_name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::DebugError => "derror",
            Self::Debug => "debug",
            Self::Time => "time",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Self::Info => "\x1b[38;2;80;250;123m",
            Self::Warn => "\x1b[38;2;255;184;108m",
            Self::Error => "\x1b[38;2;255;85;85m",
            Self::DebugError => "\x1b[38;2;189;147;249m",
            Self::Debug => "\x1b[38;2;120;120;120m",
            Self::Time => "\x1b[38;2;80;80;80m",
        }
    }

    fn debug_only(self) -> bool {
        matches!(self, Self::Debug | Self::Time | Self::DebugError)
    }
}

/// A replaceable console boundary. Implementations must not retain `text`.
pub trait ConsoleSink: Send + Sync + 'static {
    fn is_tty(&self) -> bool;
    fn write_log(&self, text: &str) -> io::Result<()>;
    fn clear_progress(&self, line_count: usize) -> io::Result<()>;
    fn write_progress(&self, text: &str) -> io::Result<()>;
}

/// The process stdout console.
#[derive(Debug, Default)]
pub struct StdoutConsole;

impl ConsoleSink for StdoutConsole {
    fn is_tty(&self) -> bool {
        std::io::IsTerminal::is_terminal(&std::io::stdout())
    }

    fn write_log(&self, text: &str) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()
    }

    fn clear_progress(&self, line_count: usize) -> io::Result<()> {
        if line_count == 0 {
            return Ok(());
        }
        let mut stdout = io::stdout().lock();
        write!(stdout, "\x1b[{line_count}A\r\x1b[J")?;
        stdout.flush()
    }

    fn write_progress(&self, text: &str) -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        stdout.write_all(text.as_bytes())?;
        stdout.flush()
    }
}

#[derive(Clone)]
pub struct Logger {
    inner: Arc<Inner>,
}

struct Inner {
    version: Arc<str>,
    started: Instant,
    debug: AtomicBool,
    tx: mpsc::Sender<WriterCommand>,
    dropped: AtomicU64,
    console: Arc<dyn ConsoleSink>,
    progress: Mutex<ProgressData>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

struct ProgressData {
    next_token: u64,
    bars: BTreeMap<u64, ProgressState>,
    last_line_count: usize,
    disabled: bool,
}

struct ProgressState {
    title: String,
    progress: f64,
    message: String,
    last_render: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProgressToken(u64);

#[derive(Clone, Debug)]
pub struct LoggerOptions {
    pub version: String,
    pub debug_log: bool,
    pub queue_capacity: usize,
}

impl Default for LoggerOptions {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            debug_log: false,
            queue_capacity: 4096,
        }
    }
}

enum WriterCommand {
    Line(String),
    Flush(oneshot::Sender<io::Result<()>>),
    Shutdown(oneshot::Sender<io::Result<()>>),
}

impl Logger {
    /// Rotates and cleans the log directory, then starts its sole file owner.
    ///
    /// Normal logging uses `try_send`: when the bounded queue is full (or the
    /// writer has closed), the newest file line is dropped and `dropped_lines`
    /// is incremented. Console output remains independent. This policy makes
    /// every producer, including memory polling, strictly non-blocking.
    pub fn new(
        logs_dir: impl AsRef<Path>,
        options: LoggerOptions,
        console: Arc<dyn ConsoleSink>,
    ) -> io::Result<Self> {
        let logs_dir = logs_dir.as_ref();
        fs::create_dir_all(logs_dir)?;
        rotate_latest(logs_dir)?;
        cleanup_logs(logs_dir)?;

        let latest = logs_dir.join("latest.log");
        OpenOptions::new().create(true).append(true).open(&latest)?;

        let (tx, rx) = mpsc::channel(options.queue_capacity.max(1));
        let writer = tokio::spawn(writer_task(latest, rx));
        Ok(Self {
            inner: Arc::new(Inner {
                version: Arc::from(options.version),
                started: Instant::now(),
                debug: AtomicBool::new(options.debug_log),
                tx,
                dropped: AtomicU64::new(0),
                console,
                progress: Mutex::new(ProgressData {
                    next_token: 1,
                    bars: BTreeMap::new(),
                    last_line_count: 0,
                    disabled: false,
                }),
                writer: Mutex::new(Some(writer)),
            }),
        })
    }

    /// Applies the reloadable `DEBUG_LOG` value to subsequent console writes.
    pub fn update_debug(&self, enabled: bool) {
        self.inner.debug.store(enabled, Ordering::Release);
    }

    /// Applies the debug field from a freshly published configuration snapshot.
    pub fn update_config(&self, config: &crate::config::Config) {
        self.update_debug(config.debug_log);
    }

    pub fn debug_enabled(&self) -> bool {
        self.inner.debug.load(Ordering::Acquire)
    }

    pub fn dropped_lines(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    pub fn running_time(&self) -> String {
        format_running_time(self.inner.started.elapsed())
    }

    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        let message = message.into();
        let time = self.running_time();
        let file_line = format_file_line(&time, level, &message);
        if self
            .inner
            .tx
            .try_send(WriterCommand::Line(file_line))
            .is_err()
        {
            self.inner.dropped.fetch_add(1, Ordering::Relaxed);
        }

        if level.debug_only() && !self.debug_enabled() {
            return;
        }

        self.clear_progress();
        let console_line = format_console_line(&self.inner.version, &time, level, &message);
        let _ = self.inner.console.write_log(&console_line);
        self.render_progress_at(Instant::now(), true);
    }

    pub fn info(&self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    pub fn warn(&self, message: impl Into<String>) {
        self.log(LogLevel::Warn, message);
    }

    pub fn error(&self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }

    pub fn debug(&self, message: impl Into<String>) {
        self.log(LogLevel::Debug, message);
    }

    pub fn time(&self, message: impl Into<String>) {
        self.log(LogLevel::Time, message);
    }

    pub fn debug_error(&self, message: impl Into<String>) {
        self.log(LogLevel::DebugError, message);
    }

    /// Logs a fatal line without the lossy path, then durably flushes it.
    pub async fn fatal(&self, message: impl Into<String>) -> io::Result<()> {
        let message = message.into();
        let time = self.running_time();
        let line = format_file_line(&time, LogLevel::Error, &message);
        self.inner
            .tx
            .send(WriterCommand::Line(line))
            .await
            .map_err(|_| broken_writer())?;
        self.clear_progress();
        let _ = self.inner.console.write_log(&format_console_line(
            &self.inner.version,
            &time,
            LogLevel::Error,
            &message,
        ));
        self.flush().await
    }

    /// Acknowledges only after all preceding lines have reached the OS file.
    pub async fn flush(&self) -> io::Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.inner
            .tx
            .send(WriterCommand::Flush(ack_tx))
            .await
            .map_err(|_| broken_writer())?;
        ack_rx.await.map_err(|_| broken_writer())?
    }

    /// Flushes, closes the file owner, and waits for its task to finish.
    pub async fn shutdown(&self) -> io::Result<()> {
        let handle = take_writer(&self.inner.writer);
        let Some(handle) = handle else {
            return Ok(());
        };

        let (ack_tx, ack_rx) = oneshot::channel();
        self.inner
            .tx
            .send(WriterCommand::Shutdown(ack_tx))
            .await
            .map_err(|_| broken_writer())?;
        let result = ack_rx.await.map_err(|_| broken_writer())?;
        handle
            .await
            .map_err(|error| io::Error::other(format!("log writer task failed: {error}")))?;
        result
    }

    pub fn progress_start(&self, title: impl Into<String>) -> ProgressToken {
        let token = {
            let mut data = lock(&self.inner.progress);
            let token = data.next_token;
            data.next_token = data.next_token.wrapping_add(1).max(1);
            if !data.disabled {
                data.bars.insert(
                    token,
                    ProgressState {
                        title: title.into(),
                        progress: 0.0,
                        message: String::new(),
                        last_render: None,
                    },
                );
            }
            ProgressToken(token)
        };
        self.render_progress_at(Instant::now(), true);
        token
    }

    pub fn progress_update(&self, token: ProgressToken, progress: f64, message: impl Into<String>) {
        let now = Instant::now();
        let should_render = {
            let mut data = lock(&self.inner.progress);
            let Some(bar) = data.bars.get_mut(&token.0) else {
                return;
            };
            bar.progress = progress;
            bar.message = message.into();
            let due = progress >= 1.0
                || bar
                    .last_render
                    .is_none_or(|last| now.duration_since(last) >= PROGRESS_INTERVAL);
            if due {
                bar.last_render = Some(now);
            }
            due
        };
        if should_render {
            self.render_progress_at(now, true);
        }
    }

    pub async fn progress_end(
        &self,
        token: ProgressToken,
        message: impl Into<String>,
        delay: Duration,
    ) {
        let title = {
            let mut data = lock(&self.inner.progress);
            let Some(bar) = data.bars.get_mut(&token.0) else {
                return;
            };
            bar.progress = 1.0;
            bar.title.clone()
        };
        self.render_progress_at(Instant::now(), true);
        if self.inner.console.is_tty() && !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        {
            let mut data = lock(&self.inner.progress);
            data.bars.remove(&token.0);
        }
        self.info(format!("{title}: {}", message.into()));
    }

    fn clear_progress(&self) {
        if !self.inner.console.is_tty() {
            return;
        }
        let line_count = {
            let mut data = lock(&self.inner.progress);
            if data.disabled || data.last_line_count == 0 {
                return;
            }
            let count = data.last_line_count;
            data.last_line_count = 0;
            count
        };
        if self.inner.console.clear_progress(line_count).is_err() {
            self.disable_progress();
        }
    }

    fn render_progress_at(&self, now: Instant, force: bool) {
        if !self.inner.console.is_tty() {
            return;
        }
        self.clear_progress();
        let output = {
            let mut data = lock(&self.inner.progress);
            if data.disabled || data.bars.is_empty() {
                return;
            }
            if !force {
                return;
            }
            let time = format_running_time(now.duration_since(self.inner.started));
            let mut output = String::from("\n");
            for bar in data.bars.values() {
                let progress = bar.progress.clamp(0.0, 1.0);
                let filled = (30.0 * progress).round() as usize;
                let empty = 30 - filled;
                let percentage = (progress * 100.0).round() as u32;
                output.push_str(LogLevel::Info.color());
                output.push_str(SEPARATOR);
                output.push(' ');
                output.push_str(GREY);
                output.push_str(&time);
                output.push_str(RESET);
                output.push_str("  ");
                output.push_str(&bar.title);
                output.push_str(" [");
                output.push_str(GREEN_TEXT);
                output.push_str(&PROGRESS_FILLED.repeat(filled));
                output.push_str(GREY);
                output.push_str(&PROGRESS_EMPTY.repeat(empty));
                output.push_str(RESET);
                output.push_str(&format!("] {percentage:>3}% {}\n", bar.message));
            }
            output.pop();
            data.last_line_count = data.bars.len();
            output
        };
        if self.inner.console.write_progress(&output).is_err() {
            self.disable_progress();
        }
    }

    fn disable_progress(&self) {
        let mut data = lock(&self.inner.progress);
        data.disabled = true;
        data.bars.clear();
        data.last_line_count = 0;
    }
}

async fn writer_task(path: PathBuf, mut rx: mpsc::Receiver<WriterCommand>) {
    let opened = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await;
    let mut file = match opened {
        Ok(file) => file,
        Err(error) => {
            while let Some(command) = rx.recv().await {
                match command {
                    WriterCommand::Line(_) => {}
                    WriterCommand::Flush(ack) | WriterCommand::Shutdown(ack) => {
                        let _ = ack.send(Err(io::Error::new(error.kind(), error.to_string())));
                    }
                }
            }
            return;
        }
    };
    let mut first_error: Option<(io::ErrorKind, String)> = None;

    while let Some(command) = rx.recv().await {
        match command {
            WriterCommand::Line(line) => {
                if let Err(error) = file.write_all(line.as_bytes()).await
                    && first_error.is_none()
                {
                    first_error = Some((error.kind(), error.to_string()));
                }
            }
            WriterCommand::Flush(ack) => {
                let result = flush_result(&mut file, &mut first_error).await;
                let _ = ack.send(result);
            }
            WriterCommand::Shutdown(ack) => {
                let result = flush_result(&mut file, &mut first_error).await;
                let _ = ack.send(result);
                break;
            }
        }
    }
}

async fn flush_result(
    file: &mut tokio::fs::File,
    first_error: &mut Option<(io::ErrorKind, String)>,
) -> io::Result<()> {
    let flush_error = file.flush().await.err();
    if let Some((kind, message)) = first_error.take() {
        return Err(io::Error::new(kind, message));
    }
    match flush_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn broken_writer() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "log writer is closed")
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn take_writer(mutex: &Mutex<Option<JoinHandle<()>>>) -> Option<JoinHandle<()>> {
    lock(mutex).take()
}

pub fn format_running_time(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    let hours = total_seconds / 3600;
    let minutes = total_seconds % 3600 / 60;
    let seconds = total_seconds % 60;
    format!(
        "{hours:02}:{minutes:02}:{seconds:02}.{:03}",
        elapsed.subsec_millis()
    )
}

fn format_file_line(time: &str, level: LogLevel, message: &str) -> String {
    let label = format!("[{}]", level.file_name());
    format!("{time} {label:<8} {}\n", strip_highlights(message))
}

fn format_console_line(version: &str, time: &str, level: LogLevel, message: &str) -> String {
    format!(
        "{GREY}{version}{RESET}  {}{SEPARATOR}{RESET}  {GREY}{time}{RESET}  {}\n",
        level.color(),
        ansi_highlights(message)
    )
}

fn transform_highlights(text: &str, ansi: bool) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some(open) = remaining.find('%') {
        output.push_str(&remaining[..open]);
        let after_open = &remaining[open + 1..];
        let Some(close) = after_open.find('%') else {
            output.push_str(&remaining[open..]);
            return output;
        };
        if after_open[..close].contains('\n') {
            output.push('%');
            remaining = after_open;
            continue;
        }
        if ansi {
            output.push_str(HIGHLIGHT);
        }
        output.push_str(&after_open[..close]);
        if ansi {
            output.push_str(RESET);
        }
        remaining = &after_open[close + 1..];
    }
    output.push_str(remaining);
    output
}

fn strip_highlights(text: &str) -> String {
    transform_highlights(text, false)
}

fn ansi_highlights(text: &str) -> String {
    transform_highlights(text, true)
}

#[cfg(test)]
mod tests;

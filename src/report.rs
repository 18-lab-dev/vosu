//! Escaped machine and application report generation.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::Stream;
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::{Config, ConfigDocument};
use thiserror::Error;

mod native;

const TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/server/report.html"
));
const CONTENT_TYPE: &str = "text/html; charset=utf-8";
const LOG_LIMIT: usize = 10;
const LOG_MAX_BYTES: usize = 1_048_576;
const LOG_MAX_TOTAL_BYTES: usize = 8_388_608;
const LOG_MAX_LINES: usize = 10_000;
const LOG_MAX_TOTAL_LINES: usize = 100_000;

#[derive(Debug, Error)]
pub enum ReportError {
    #[error("failed to read report logs: {0}")]
    Logs(#[source] io::Error),
    #[error("report log worker failed: {0}")]
    LogWorker(#[source] tokio::task::JoinError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceType {
    Desktop,
    Laptop,
    Unknown,
}

impl DeviceType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "Desktop",
            Self::Laptop => "Laptop",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphicsDeviceType {
    Cpu,
    IntegratedGpu,
    DiscreteGpu,
    VirtualGpu,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsAdapter {
    pub name: String,
    pub device_type: GraphicsDeviceType,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportSpecs {
    pub system_type: DeviceType,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub gpus: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportInstance {
    pub pid: u32,
    pub client_type: String,
    pub bitness: String,
    pub version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportCounter {
    pub author: String,
    pub name: String,
    pub version: String,
    pub folder_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogLine {
    pub timestamp: String,
    pub level: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportLog {
    pub filename: String,
    pub modified: DateTime<Utc>,
    pub lines: Vec<LogLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineData {
    pub system_type: DeviceType,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub adapters: Vec<GraphicsAdapter>,
    /// Used only on Windows when the headless adapter enumeration is empty.
    pub fallback_gpu_names: Vec<String>,
}

pub trait ReportProbe: Send + Sync {
    fn probe(&self) -> Pin<Box<dyn Future<Output = MachineData> + Send + '_>>;
}

#[derive(Debug, Default)]
pub struct NativeReportProbe;
impl ReportProbe for NativeReportProbe {
    fn probe(&self) -> Pin<Box<dyn Future<Output = MachineData> + Send + '_>> {
        Box::pin(async {
            tokio::task::spawn_blocking(|| {
                let base = native_machine_base();
                #[cfg(not(windows))]
                let base = MachineData {
                    adapters: enumerate_graphics_adapters(),
                    ..base
                };
                base
            })
            .await
            .unwrap_or_else(|_| MachineData::unknown())
        })
    }
}

impl MachineData {
    fn unknown() -> Self {
        Self {
            system_type: DeviceType::Unknown,
            os: "Unknown".into(),
            arch: std::env::consts::ARCH.into(),
            cpu: "Unknown".into(),
            adapters: Vec::new(),
            fallback_gpu_names: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReportInput {
    /// Configuration in source binding order.
    pub config: Vec<(String, String)>,
    /// Instances should normally already be PID ordered; generation enforces it.
    pub instances: Vec<ReportInstance>,
    /// Installed catalog/counter data. Generation sorts by folder, then name.
    pub counters: Vec<ReportCounter>,
    pub logs_dir: Option<PathBuf>,
}

impl ReportInput {
    #[must_use]
    pub fn with_config(config: &Config) -> Self {
        Self {
            config: config_fields(config),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub date: DateTime<Utc>,
    pub spec: ReportSpecs,
    pub config: Vec<(String, String)>,
    pub instances: Vec<ReportInstance>,
    pub counters: Vec<ReportCounter>,
    pub logs: Vec<ReportLog>,
}

#[derive(Debug)]
pub struct ReportDocument {
    pub attachment_name: String,
    pub content_type: &'static str,
    report: Report,
}

impl IntoResponse for ReportDocument {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(self.content_type),
        );
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!(
                "attachment; filename=\"{}\"",
                self.attachment_name
            ))
            .expect("epoch attachment filename is a valid header value"),
        );
        let body = Body::from_stream(ReportBodyStream::new(self.report));
        (headers, body).into_response()
    }
}

/// Server handler hook: gathers a complete report and returns a response-ready
/// streamed document plus the exact attachment filename.
pub async fn generate_report_document(
    input: ReportInput,
    probe: &dyn ReportProbe,
) -> Result<ReportDocument, ReportError> {
    generate_report_document_at(input, probe, SystemTime::now()).await
}

pub async fn generate_report_document_at(
    input: ReportInput,
    probe: &dyn ReportProbe,
    now: SystemTime,
) -> Result<ReportDocument, ReportError> {
    let epoch_ms = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    let report = collect_report_at(input, probe, now).await?;

    Ok(ReportDocument {
        attachment_name: format!("vosu-report-{epoch_ms}.html"),
        content_type: CONTENT_TYPE,
        report,
    })
}

/// Collects the typed report independently of HTML rendering, allowing server
/// callers and tests to inspect the exact report fields.
pub async fn collect_report_at(
    mut input: ReportInput,
    probe: &dyn ReportProbe,
    now: SystemTime,
) -> Result<Report, ReportError> {
    let machine = probe.probe().await;
    let gpus = filtered_gpu_names(&machine.adapters, &machine.fallback_gpu_names);
    input.instances.sort_by_key(|instance| instance.pid);
    input.counters.sort_by(|left, right| {
        left.folder_name
            .cmp(&right.folder_name)
            .then_with(|| left.name.cmp(&right.name))
    });
    let logs = match input.logs_dir {
        Some(directory) => tokio::task::spawn_blocking(move || read_newest_logs(&directory))
            .await
            .map_err(ReportError::LogWorker)?,
        None => Ok(Vec::new()),
    }
    .map_err(ReportError::Logs)?;
    Ok(Report {
        date: DateTime::<Utc>::from(now),
        spec: ReportSpecs {
            system_type: machine.system_type,
            os: nonempty_or_unknown(machine.os),
            arch: nonempty_or_unknown(machine.arch),
            cpu: nonempty_or_unknown(machine.cpu),
            gpus,
        },
        config: input.config,
        instances: input.instances,
        counters: input.counters,
        logs,
    })
}

#[must_use]
pub fn config_fields(config: &Config) -> Vec<(String, String)> {
    ConfigDocument::new(config.clone())
        .serialize()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[must_use]
#[derive(Debug)]
struct ReportBodyStream {
    report: Report,
    phase: StreamPhase,
    log_index: usize,
    row_index: usize,
}

#[derive(Clone, Copy, Debug)]
enum StreamPhase {
    Head,
    LogHeader,
    LogRows,
    LogFooter,
    Tail,
    Done,
}

impl ReportBodyStream {
    fn new(report: Report) -> Self {
        Self {
            report,
            phase: StreamPhase::Head,
            log_index: 0,
            row_index: 0,
        }
    }
}

impl Stream for ReportBodyStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.phase {
                StreamPhase::Head => {
                    this.phase = StreamPhase::LogHeader;
                    return Poll::Ready(Some(Ok(Bytes::from(render_report_head(&this.report)))));
                }
                StreamPhase::LogHeader => {
                    let Some(log) = this.report.logs.get(this.log_index) else {
                        this.phase = StreamPhase::Tail;
                        continue;
                    };
                    this.row_index = 0;
                    this.phase = StreamPhase::LogRows;
                    return Poll::Ready(Some(Ok(Bytes::from(render_log_header(log)))));
                }
                StreamPhase::LogRows => {
                    let log = &this.report.logs[this.log_index];
                    if let Some(line) = log.lines.get(this.row_index) {
                        this.row_index += 1;
                        return Poll::Ready(Some(Ok(Bytes::from(render_log_row(
                            this.row_index,
                            line,
                        )))));
                    }
                    this.phase = StreamPhase::LogFooter;
                }
                StreamPhase::LogFooter => {
                    this.log_index += 1;
                    this.phase = StreamPhase::LogHeader;
                    return Poll::Ready(Some(Ok(Bytes::from_static(
                        b"</table></div></div></div>",
                    ))));
                }
                StreamPhase::Tail => {
                    this.phase = StreamPhase::Done;
                    let (_, tail) = TEMPLATE
                        .split_once("{{LOGS}}")
                        .expect("retained report template contains LOGS insertion point");
                    return Poll::Ready(Some(Ok(Bytes::from_static(tail.as_bytes()))));
                }
                StreamPhase::Done => return Poll::Ready(None),
            }
        }
    }
}

fn render_report_head(report: &Report) -> String {
    let config = report
        .config
        .iter()
        .map(|(key, value)| {
            format!(
                "<tr><td class=\"highlight\">{}</td><td>{}</td></tr>",
                escape_html(key),
                escape_html(value)
            )
        })
        .collect::<String>();
    let instances = report.instances.iter().enumerate().map(|(index, instance)| {
        format!(
            "<tr><td class=\"highlight\">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            index + 1,
            instance.pid,
            escape_html(&instance.client_type),
            escape_html(&instance.bitness),
            escape_html(&instance.version),
        )
    }).collect::<String>();
    let counters = report.counters.iter().enumerate().map(|(index, counter)| {
        format!(
            "<tr><td class=\"highlight\">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            index + 1,
            escape_html(&counter.name),
            escape_html(&counter.version),
            escape_html(&counter.author),
            escape_html(&counter.folder_name),
        )
    }).collect::<String>();
    let gpu_text = report
        .spec
        .gpus
        .iter()
        .map(|gpu| escape_html(gpu))
        .collect::<Vec<_>>()
        .join(", ");
    let (template_head, _) = TEMPLATE
        .split_once("{{LOGS}}")
        .expect("retained report template contains LOGS insertion point");

    template_head
        .replace("{{TIME}}", &escape_html(&iso_millis(report.date)))
        .replace("{{SYSTEM_TYPE}}", report.spec.system_type.as_str())
        .replace(
            "{{SYSTEM_OS}}",
            &escape_html(&format!("{}-{}", report.spec.os, report.spec.arch)),
        )
        .replace("{{SYSTEM_CPU}}", &escape_html(&report.spec.cpu))
        .replace("{{SYSTEM_GPUS}}", &gpu_text)
        .replace("{{VOSU_CONFIG}}", &config)
        .replace("{{INSTANCES}}", &instances)
        .replace("{{COUNTERS}}", &counters)
}

fn render_log_header(log: &ReportLog) -> String {
    format!(
        "<div class=\"group logs\"><h3>{} ({})</h3><div class=\"group-content\"><div class=\"scrollable\"><table><thead><tr><th>#</th><th class=\"timestamp\">Timestamp</th><th class=\"type\">Type</th><th class=\"message\">Message</th></tr></thead>",
        escape_html(&log.filename),
        escape_html(&iso_millis(log.modified)),
    )
}

fn render_log_row(index: usize, line: &LogLine) -> String {
    format!(
        "<tr><td class=\"highlight\">{index}</td><td>{}</td><td class=\"status-{}\">{}</td><td>{}</td></tr>",
        escape_html(&line.timestamp),
        escape_html(&line.level),
        escape_html(&line.level),
        escape_html(&line.text),
    )
}

fn iso_millis(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[must_use]
pub fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn read_newest_logs(directory: &Path) -> io::Result<Vec<ReportLog>> {
    let mut files = Vec::with_capacity(LOG_LIMIT + 1);
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("log") {
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        files.push((
            metadata.modified()?,
            entry.file_name().to_string_lossy().into_owned(),
            path,
        ));
        files.sort_unstable_by(|left, right| {
            right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1))
        });
        files.truncate(LOG_LIMIT);
    }
    let mut total_bytes = 0;
    let mut total_lines = 0;
    let mut omitted = false;
    let mut logs = Vec::with_capacity(files.len());
    for (modified, filename, path) in files {
        let remaining_bytes = LOG_MAX_TOTAL_BYTES.saturating_sub(total_bytes);
        let remaining_lines = LOG_MAX_TOTAL_LINES.saturating_sub(total_lines);
        if remaining_bytes == 0 || remaining_lines == 0 {
            omitted = true;
            break;
        }
        let (contents, byte_truncated) = read_bounded_log(&path, remaining_bytes)?;
        total_bytes = total_bytes.saturating_add(contents.len());
        let line_limit = LOG_MAX_LINES.min(remaining_lines);
        let mut lines = contents
            .lines()
            .take(line_limit)
            .map(parse_log_line)
            .collect::<Vec<_>>();
        total_lines = total_lines.saturating_add(lines.len());
        let line_truncated = contents.lines().nth(line_limit).is_some();
        if byte_truncated || line_truncated {
            lines.push(LogLine {
                timestamp: String::new(),
                level: String::new(),
                text: format!(
                    "[log truncated: maximum {} bytes and {} lines per file]",
                    LOG_MAX_BYTES, LOG_MAX_LINES
                ),
            });
        }
        logs.push(ReportLog {
            filename,
            modified: DateTime::<Utc>::from(modified),
            lines,
        });
    }
    if omitted && let Some(log) = logs.last_mut() {
        log.lines.push(LogLine {
            timestamp: String::new(),
            level: String::new(),
            text: format!("[logs truncated: maximum {LOG_MAX_TOTAL_BYTES} bytes and {LOG_MAX_TOTAL_LINES} lines aggregate]"),
        });
    }
    Ok(logs)
}

fn read_bounded_log(path: &Path, remaining: usize) -> io::Result<(String, bool)> {
    let limit = LOG_MAX_BYTES.min(remaining);
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    if truncated && let Err(error) = std::str::from_utf8(&bytes) {
        if error.error_len().is_none() {
            bytes.truncate(error.valid_up_to());
        } else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
    let contents = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok((contents, truncated))
}

fn parse_log_line(line: &str) -> LogLine {
    fn unstructured(line: &str) -> LogLine {
        LogLine {
            timestamp: String::new(),
            level: String::new(),
            text: line.to_owned(),
        }
    }

    let bytes = line.as_bytes();
    let timestamp_shape = bytes.len() >= 14
        && [0, 1, 3, 4, 6, 7, 9, 10, 11]
            .into_iter()
            .all(|index| bytes[index].is_ascii_digit())
        && bytes[2] == b':'
        && bytes[5] == b':'
        && bytes[8] == b'.';
    if !timestamp_shape {
        return unstructured(line);
    }
    let mut position = 12;
    let whitespace_start = position;
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    if position == whitespace_start || bytes.get(position) != Some(&b'[') {
        return unstructured(line);
    }
    position += 1;
    let Some(level_length) = bytes[position..].iter().position(|byte| *byte == b']') else {
        return unstructured(line);
    };
    if level_length == 0 {
        return unstructured(line);
    }
    let level_end = position + level_length;
    let raw_level = &line[position..level_end];
    position = level_end + 1;
    let whitespace_start = position;
    while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
        position += 1;
    }
    if position == whitespace_start {
        return unstructured(line);
    }
    let level = if raw_level == "derror" {
        "debugError"
    } else {
        raw_level
    };
    LogLine {
        timestamp: line[..12].to_owned(),
        level: level.to_owned(),
        text: line[position..].to_owned(),
    }
}

fn filtered_gpu_names(adapters: &[GraphicsAdapter], fallback: &[String]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for adapter in adapters {
        if adapter.device_type != GraphicsDeviceType::Cpu && !adapter.name.trim().is_empty() {
            names.insert(adapter.name.trim().to_owned());
        }
    }
    #[cfg(windows)]
    if names.is_empty() {
        names.extend(fallback.iter().filter_map(|name| {
            let name = name.trim();
            (!name.is_empty()).then(|| name.to_owned())
        }));
    }
    #[cfg(not(windows))]
    let _ = fallback;
    names.into_iter().collect()
}

#[cfg(all(test, target_os = "linux"))]
use native::classify_linux_device;
#[cfg(not(windows))]
use native::enumerate_graphics_adapters;
use native::{native_machine_base, nonempty_or_unknown};

#[cfg(test)]
mod tests;

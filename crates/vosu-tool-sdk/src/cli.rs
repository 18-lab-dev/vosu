//! Shared terminal output and cancellable job handling for tool CLIs.
use crate::{Client, Error};
pub use reqwest::Method;
use serde_json::Value;
use std::{
    future::Future,
    io::{IsTerminal, Write},
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(transparent)]
    Request(#[from] Error),
    #[error("{0}")]
    Failed(String),
    #[error("job cancelled")]
    Cancelled,
}
impl CliError {
    pub fn exit_code(&self) -> i32 {
        if matches!(self, Self::Cancelled) {
            130
        } else {
            1
        }
    }
}
impl Client {
    /// Wait for a protocol job. Ctrl+C cancels the job before returning exit 130.
    pub async fn wait_job(
        &self,
        tool: &str,
        id: &str,
        on_progress: impl FnMut(&Value),
    ) -> Result<Value, CliError> {
        self.wait_job_until(tool, id, on_progress, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
    }
    /// Injectable cancellation future for embedders and signal-free tests.
    pub async fn wait_job_until(
        &self,
        tool: &str,
        id: &str,
        mut on_progress: impl FnMut(&Value),
        cancel: impl Future<Output = ()>,
    ) -> Result<Value, CliError> {
        let path = format!("v1/jobs/{id}");
        tokio::pin!(cancel);
        loop {
            let poll = async {
                let job: Value = self.tool_json(tool, &path).await?;
                Ok::<_, CliError>(job)
            };
            let job = tokio::select! {
                biased;
                () = &mut cancel => {
                    // Cancellation is best effort, but never lets a dead host hang exit.
                    let _ = self.tool(Method::DELETE, tool, &path).timeout(Duration::from_secs(3)).send().await;
                    return Err(CliError::Cancelled);
                }
                job = poll => job?,
            };
            on_progress(&job);
            match job["state"].as_str() {
                Some("done") => return Ok(job),
                Some("failed") => {
                    return Err(CliError::Failed(
                        job["message"].as_str().unwrap_or("job failed").into(),
                    ));
                }
                Some("cancelled") => return Err(CliError::Cancelled),
                Some("queued" | "running") => {}
                _ => return Err(CliError::Failed("invalid job state".into())),
            }
            tokio::select! {
                () = &mut cancel => {
                    let _ = self.tool(Method::DELETE, tool, &path).timeout(Duration::from_secs(3)).send().await;
                    return Err(CliError::Cancelled);
                }
                () = tokio::time::sleep(Duration::from_millis(200)) => {}
            }
        }
    }
}
/// Progress uses carriage-return updates only on a terminal, without ANSI.
pub struct Progress {
    label: String,
    last: String,
    width: usize,
    tty: bool,
}
impl Progress {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            last: String::new(),
            width: 0,
            tty: std::io::stderr().is_terminal(),
        }
    }
    pub fn update(&mut self, job: &Value) {
        let line = format!(
            "{} … {:.0}% — {}",
            self.label,
            job["progress"].as_f64().unwrap_or(0.0) * 100.0,
            text(&job["message"])
        );
        if line == self.last {
            return;
        }
        if self.tty {
            let _ = write!(
                std::io::stderr().lock(),
                "\r{line:width$}",
                width = self.width.max(line.chars().count())
            );
            let _ = std::io::stderr().flush();
        } else {
            eprintln!("{line}");
        }
        self.width = line.chars().count();
        self.last = line;
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        if self.tty && !self.last.is_empty() {
            eprintln!();
        }
    }
}
pub fn print_result(
    value: &Value,
    json: bool,
    human: impl FnOnce() -> String,
) -> std::io::Result<()> {
    writeln!(
        std::io::stdout().lock(),
        "{}",
        if json { value.to_string() } else { human() }
    )
}
pub const SKILLS: [&str; 8] = [
    "overall",
    "stream",
    "jumpstream",
    "handstream",
    "stamina",
    "jackspeed",
    "chordjack",
    "technical",
];
pub fn text(value: &Value) -> String {
    value
        .as_str()
        .unwrap_or("—")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
pub fn number(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "—".into(), |v| format!("{v:.2}"))
}
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let widths: Vec<_> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
                .max(h.chars().count())
        })
        .collect();
    let row = |cells: &[String]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{s}{}", " ".repeat(widths[i] - s.chars().count())))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut output = vec![
        row(&headers.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
        widths
            .iter()
            .map(|n| "─".repeat(*n))
            .collect::<Vec<_>>()
            .join("  "),
    ];
    output.extend(rows.iter().map(|r| row(r)));
    if rows.is_empty() {
        output.push("(none)".into());
    }
    output.join("\n")
}
pub fn ratings(msd: &Value, ssr: Option<&Value>) -> String {
    let rows = SKILLS
        .iter()
        .map(|skill| {
            let mut row = vec![skill.to_string(), number(&msd[skill])];
            if let Some(ssr) = ssr {
                row.push(number(&ssr[skill]));
            }
            row
        })
        .collect::<Vec<_>>();
    table(
        if ssr.is_some() {
            &["Skillset", "MSD", "SSR"]
        } else {
            &["Skillset", "Rating"]
        },
        &rows,
    )
}
pub fn dan(value: &Value, reason: &str) -> String {
    if value.is_object() {
        format!(
            "dan: {} ({}, experimental)",
            text(&value["label"]),
            number(&value["value"])
        )
    } else {
        format!("dan: — ({reason})")
    }
}
pub fn range(value: &str, min: f64, max: f64) -> Result<f64, String> {
    let n: f64 = value.parse().map_err(|_| "expected a number".to_string())?;
    if n.is_finite() && n >= min && n <= max {
        Ok(n)
    } else {
        Err(format!("value must be {min}–{max}"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_and_exit_codes() {
        let v = serde_json::json!({"overall":12.500002});
        assert!(ratings(&v, None).contains("12.50"));
        assert!(!ratings(&v, None).contains("12.500002"));
        assert!(table(&["Map"], &[]).contains("(none)"));
        assert_eq!(CliError::Cancelled.exit_code(), 130);
        assert_eq!(CliError::Failed("bad".into()).exit_code(), 1);
        for v in ["NaN", "inf", "-1", "11"] {
            assert!(range(v, 0., 10.).is_err());
        }
    }
}

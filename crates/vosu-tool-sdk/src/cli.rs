//! Shared terminal output and cancellable job handling for tool CLIs.
use crate::{Client, Error};
pub use reqwest::Method;
use serde_json::Value;
use std::{future::Future, io::Write, time::Duration};

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
pub fn progress(job: &Value) {
    eprintln!(
        "{:.0}% {}",
        job["progress"].as_f64().unwrap_or(0.0) * 100.0,
        job["message"].as_str().unwrap_or("")
    );
}
/// JSON is the service response unchanged; human output labels nested fields.
pub fn format_output(value: &Value, json: bool) -> String {
    if json {
        return value.to_string();
    }
    fn lines(value: &Value, prefix: &str, out: &mut String) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    lines(
                        value,
                        &if prefix.is_empty() {
                            key.clone()
                        } else {
                            format!("{prefix}.{key}")
                        },
                        out,
                    );
                }
            }
            Value::Array(values) => {
                if values.is_empty() {
                    out.push_str(&format!("{prefix}: (none)\n"));
                }
                for (i, value) in values.iter().enumerate() {
                    lines(value, &format!("{prefix}[{i}]"), out);
                }
            }
            _ => out.push_str(&format!(
                "{prefix}: {}\n",
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            )),
        }
    }
    let mut out = String::new();
    lines(value, "", &mut out);
    out.trim_end().into()
}
pub fn print_output(value: &Value, json: bool) -> std::io::Result<()> {
    writeln!(std::io::stdout().lock(), "{}", format_output(value, json))
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
        let v = serde_json::json!({"msd":{"overall":12.5},"name":"test"});
        assert_eq!(
            serde_json::from_str::<Value>(&format_output(&v, true)).unwrap(),
            v
        );
        assert!(format_output(&v, false).contains("msd.overall: 12.5"));
        assert_eq!(CliError::Cancelled.exit_code(), 130);
        assert_eq!(CliError::Failed("bad".into()).exit_code(), 1);
        for v in ["NaN", "inf", "-1", "11"] {
            assert!(range(v, 0., 10.).is_err());
        }
    }
}

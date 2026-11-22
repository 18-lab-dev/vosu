use chrono::{DateTime, Local};

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAX_LOG_BYTES: u64 = 100 * 1024 * 1024;
const SAFE_LOG_BYTES: u64 = 90 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CleanupReport {
    pub deleted_files: usize,
    pub freed_bytes: u64,
}

/// Renames `latest.log` using its local mtime and a `-N` collision suffix.
pub fn rotate_latest(logs_dir: &Path) -> io::Result<Option<PathBuf>> {
    let latest = logs_dir.join("latest.log");
    let metadata = match fs::metadata(&latest) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let modified: DateTime<Local> = metadata.modified()?.into();
    let base = modified.format("%Y-%m-%d %H-%M-%S").to_string();
    let mut suffix = 0_u64;
    let destination = loop {
        let name = if suffix == 0 {
            format!("{base}.log")
        } else {
            format!("{base}-{suffix}.log")
        };
        let candidate = logs_dir.join(name);
        if !candidate.exists() {
            break candidate;
        }
        suffix += 1;
    };
    fs::rename(latest, &destination)?;
    Ok(Some(destination))
}

/// At 100 MiB total, deletes oldest non-latest files until at most 90 MiB.
pub fn cleanup_logs(logs_dir: &Path) -> io::Result<CleanupReport> {
    let entries = match fs::read_dir(logs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CleanupReport::default());
        }
        Err(error) => return Err(error),
    };
    let mut total = 0_u64;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let size = if metadata.is_file() {
            metadata.len()
        } else {
            0
        };
        total = total.saturating_add(size);
        if entry.file_name() != "latest.log" {
            candidates.push((metadata.modified()?, entry.path(), size));
        }
    }
    if total < MAX_LOG_BYTES {
        return Ok(CleanupReport::default());
    }
    candidates.sort_by_key(|(modified, path, _)| (*modified, path.clone()));
    let mut report = CleanupReport::default();
    for (_, path, size) in candidates {
        if total <= SAFE_LOG_BYTES {
            break;
        }
        fs::remove_file(path)?;
        total = total.saturating_sub(size);
        report.deleted_files += 1;
        report.freed_bytes = report.freed_bytes.saturating_add(size);
    }
    Ok(report)
}

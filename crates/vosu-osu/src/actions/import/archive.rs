use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;
use vosu_net::streaming::{
    NetworkError, open_preflighted_zip, preflight_zip_entries, validate_zip_entry,
};

use super::{ImportError, check_cancel};

#[derive(Debug, Clone, Copy)]
pub struct ArchiveLimits {
    pub compressed_bytes: u64,
    pub expanded_bytes: u64,
    pub entries: usize,
    pub chart_bytes: u64,
}
impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            compressed_bytes: 256 * 1024 * 1024,
            expanded_bytes: 512 * 1024 * 1024,
            entries: 10_000,
            chart_bytes: 16 * 1024 * 1024,
        }
    }
}

/// An owned, private snapshot: replacing or modifying the source after this
/// returns cannot change the bytes committed/submitted. No caller gets a mutable
/// file or path until validation has completed.
pub struct ValidatedArchive {
    pub(super) snapshot: NamedTempFile,
}
impl ValidatedArchive {
    /// Blocking. Opens the source exactly once and snapshots at most the budget
    /// plus one byte. Reuses counter ZIP entry/count preflight, then decodes each
    /// entry to EOF to verify CRC and actual expanded bytes without extracting
    /// untrusted content. Only bounded chart data is buffered for parsing.
    pub fn open(
        source: &Path,
        limits: ArchiveLimits,
        cancel: &CancellationToken,
    ) -> Result<Self, ImportError> {
        check_cancel(cancel)?;
        if !source.is_absolute()
            || !source
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("osz"))
            || !std::fs::symlink_metadata(source)
                .map_err(|_| ImportError::InvalidSource)?
                .is_file()
        {
            return Err(ImportError::InvalidSource);
        }
        let mut input = File::open(source).map_err(|_| ImportError::InvalidSource)?;
        let metadata = input.metadata().map_err(|_| ImportError::Io)?;
        if !metadata.is_file() {
            return Err(ImportError::InvalidSource);
        }
        if metadata.len() > limits.compressed_bytes {
            return Err(ImportError::LimitExceeded);
        }
        let mut snapshot = tempfile::Builder::new()
            .prefix("vosu-import-")
            .suffix(".osz")
            .tempfile()
            .map_err(|_| ImportError::Io)?;
        copy_bounded(
            &mut input,
            snapshot.as_file_mut(),
            limits.compressed_bytes,
            cancel,
        )?;
        snapshot
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|_| ImportError::Io)?;
        validate(
            snapshot.reopen().map_err(|_| ImportError::Io)?,
            limits,
            cancel,
        )?;
        check_cancel(cancel)?;
        snapshot
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|_| ImportError::Io)?;
        Ok(Self { snapshot })
    }
}

fn network_error(error: NetworkError) -> ImportError {
    match error {
        NetworkError::Cancelled => ImportError::Cancelled,
        _ => ImportError::InvalidArchive,
    }
}

fn validate(
    mut file: File,
    limits: ArchiveLimits,
    cancel: &CancellationToken,
) -> Result<(), ImportError> {
    let count = preflight_zip_entries(&mut file, limits.entries, cancel).map_err(network_error)?;
    let mut zip = open_preflighted_zip(file, count).map_err(network_error)?;
    let mut expanded = 0u64;
    let mut paths = HashSet::new();
    let mut chart = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut valid_chart = false;
    for index in 0..zip.len() {
        check_cancel(cancel)?;
        let mut entry = zip
            .by_index(index)
            .map_err(|_| ImportError::InvalidArchive)?;
        let path = validate_zip_entry(entry.name(), entry.unix_mode()).map_err(network_error)?;
        let is_chart = !entry.is_dir()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("osu"));
        if !paths.insert(path) {
            return Err(ImportError::InvalidArchive);
        }
        let remaining = limits
            .expanded_bytes
            .checked_sub(expanded)
            .ok_or(ImportError::LimitExceeded)?;
        let maximum = if is_chart {
            remaining.min(limits.chart_bytes)
        } else {
            remaining
        };
        if entry.size() > maximum {
            return Err(ImportError::LimitExceeded);
        }
        chart.clear();
        let mut copied = 0u64;
        loop {
            check_cancel(cancel)?;
            let length = buffer.len().min(
                maximum
                    .saturating_sub(copied)
                    .saturating_add(1)
                    .min(usize::MAX as u64) as usize,
            );
            let read = entry
                .read(&mut buffer[..length])
                .map_err(|_| ImportError::InvalidArchive)?;
            if read == 0 {
                break;
            }
            copied += read as u64;
            if copied > maximum {
                return Err(ImportError::LimitExceeded);
            }
            if is_chart {
                chart.extend_from_slice(&buffer[..read]);
            }
        }
        if copied != entry.size() {
            return Err(ImportError::InvalidArchive);
        }
        expanded += copied;
        if is_chart
            && chart.starts_with(b"osu file format v")
            && let Ok(parsed) = rosu_map::Beatmap::from_bytes(&chart)
        {
            valid_chart |=
                !parsed.hit_objects.is_empty() && !parsed.control_points.timing_points.is_empty();
        }
    }
    if valid_chart {
        Ok(())
    } else {
        Err(ImportError::InvalidArchive)
    }
}

pub(super) fn copy_bounded(
    input: &mut File,
    output: &mut File,
    maximum: u64,
    cancel: &CancellationToken,
) -> Result<(), ImportError> {
    let mut remaining = maximum;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let length = buffer
            .len()
            .min(remaining.saturating_add(1).min(usize::MAX as u64) as usize);
        let read = input
            .read(&mut buffer[..length])
            .map_err(|_| ImportError::Io)?;
        if read == 0 {
            break;
        }
        remaining = remaining
            .checked_sub(read as u64)
            .ok_or(ImportError::LimitExceeded)?;
        output
            .write_all(&buffer[..read])
            .map_err(|_| ImportError::Io)?;
    }
    Ok(())
}

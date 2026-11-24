//! Bounded STATE_UPDATE splitting and latest-per-key pending coalescing (ABI §6.12).
use std::io::{self, Write};

use serde::Serialize;
use vosu_plugin_api::{ErrorInfo, StateEntry, StateUpdate, VOSU_FRAME_MAX_BYTES};

use super::MAX_SELECTORS;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    #[error("frame cap is too small even for a value_too_large error")]
    CapTooSmall,
    #[error("pending update exceeds the selector bound")]
    TooManyEntries,
}

/// Splits only between entries, counting the exact CBOR envelope and array header.
/// Fails rather than silently dropping keys if even an error entry cannot fit.
/// `cap` is additionally clamped to the ABI's universal 1 MiB maximum.
pub fn split_update(
    session: u64,
    entries: Vec<StateEntry>,
    cap: usize,
) -> Result<Vec<StateUpdate>, UpdateError> {
    if entries.len() > MAX_SELECTORS {
        return Err(UpdateError::TooManyEntries);
    }
    let cap = cap.min(VOSU_FRAME_MAX_BYTES as usize);
    let base = encoded_size(
        &StateUpdate {
            session,
            values: Vec::new(),
            more: false,
        },
        cap,
    )
    .ok_or(UpdateError::CapTooSmall)?
        - 1; // subtract the empty array's header
    let mut parts = Vec::new();
    let mut values = Vec::new();
    let mut bytes = 0;
    for mut entry in entries {
        let size = match encoded_size(&entry, cap).filter(|size| base + 1 + size <= cap) {
            Some(size) => size,
            None => {
                mark_oversize(&mut entry);
                encoded_size(&entry, cap)
                    .filter(|size| base + 1 + size <= cap)
                    .ok_or(UpdateError::CapTooSmall)?
            }
        };
        if base + array_header(values.len() + 1) + bytes + size > cap {
            parts.push(StateUpdate {
                session,
                values: std::mem::take(&mut values),
                more: true,
            });
            bytes = 0;
        }
        bytes += size;
        values.push(entry);
    }
    parts.push(StateUpdate {
        session,
        values,
        more: false,
    });
    Ok(parts)
}

fn array_header(len: usize) -> usize {
    match len {
        0..=23 => 1,
        24..=255 => 2,
        256..=65535 => 3,
        65536..=0xffff_ffff => 5,
        _ => 9,
    }
}

fn mark_oversize(entry: &mut StateEntry) {
    entry.value = None;
    entry.error = Some(ErrorInfo {
        code: "value_too_large".into(),
        message: "value exceeds frame cap".into(),
    });
}

/// Counts without allocating a second copy of a potentially oversized value.
fn encoded_size(value: &impl Serialize, cap: usize) -> Option<usize> {
    let mut writer = Counter { bytes: 0, cap };
    ciborium::ser::into_writer(value, &mut writer).ok()?;
    Some(writer.bytes)
}

struct Counter {
    bytes: usize,
    cap: usize,
}
impl Write for Counter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.cap.saturating_sub(self.bytes) {
            return Err(io::Error::other("frame cap exceeded"));
        }
        self.bytes += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One session's not-yet-started complete update. Clear it on subscription
/// replacement, so keys from the old plan cannot survive into the new baseline.
/// Every stored entry is at most 1 MiB and the set holds at most 256 keys.
/// Final envelope accounting waits for delivery's actual session id.
#[derive(Default)]
pub struct PendingUpdate {
    entries: Vec<StateEntry>,
}

impl PendingUpdate {
    /// Transactional on error: an invalid merge never loses older data.
    pub fn merge(&mut self, mut entries: Vec<StateEntry>) -> Result<(), UpdateError> {
        if entries.len() > MAX_SELECTORS {
            return Err(UpdateError::TooManyEntries);
        }
        let mut keys: Vec<_> = self
            .entries
            .iter()
            .map(|e| (e.ns.as_str(), e.pointer.as_str()))
            .collect();
        for e in &entries {
            let key = (e.ns.as_str(), e.pointer.as_str());
            if !keys.contains(&key) {
                keys.push(key);
            }
            if keys.len() > MAX_SELECTORS {
                return Err(UpdateError::TooManyEntries);
            }
        }
        drop(keys);
        for entry in &mut entries {
            if encoded_size(entry, VOSU_FRAME_MAX_BYTES as usize).is_none() {
                mark_oversize(entry);
                encoded_size(entry, VOSU_FRAME_MAX_BYTES as usize)
                    .ok_or(UpdateError::CapTooSmall)?;
            }
        }
        for entry in entries {
            if let Some(old) = self
                .entries
                .iter_mut()
                .find(|old| old.ns == entry.ns && old.pointer == entry.pointer)
            {
                *old = entry;
            } else {
                self.entries.push(entry);
            }
        }
        Ok(())
    }

    pub fn take(&mut self) -> Vec<StateEntry> {
        std::mem::take(&mut self.entries)
    }
}

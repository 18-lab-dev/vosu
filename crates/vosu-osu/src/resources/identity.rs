use super::roots::Root;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::Arc};
use vosu_sys::fs::SafeRelativePath;

pub(super) const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub(super) const MAX_SNAPSHOT_BYTES: usize = 128 * 1024 * 1024;

pub(super) fn digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hash = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in hash {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 15) as usize] as char);
    }
    text
}

/// Read the opened object once on the cold worker. The immutable bytes, not a
/// path/mtime assertion, are the representation of the published identity.
pub(super) fn snapshot(root: &Root, path: &str) -> Option<Arc<[u8]>> {
    let child = root
        .safe
        .open_child(&SafeRelativePath::parse(path).ok()?)
        .ok()?;
    if !child.metadata.is_file() || child.metadata.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(child.metadata.len() as usize);
    child
        .file
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then(|| Arc::from(bytes))
}

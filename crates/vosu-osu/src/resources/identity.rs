use super::roots::Root;
use sha2::{Digest, Sha256};
use vosu_sys::fs::{FileIdentity, SafeRelativePath};

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

/// Sample metadata only on the cold worker. No file bytes or open file handles
/// survive preparation; each lease opens and revalidates its own disk handle.
pub(super) fn identity(root: &Root, path: &str) -> Option<FileIdentity> {
    let child = root
        .safe
        .open_child(&SafeRelativePath::parse(path).ok()?)
        .ok()?;
    FileIdentity::of(&child.file).ok()
}

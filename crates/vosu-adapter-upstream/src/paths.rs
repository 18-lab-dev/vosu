//! Path strings are upstream echoes only, never filesystem authority.
use std::path::{Component, PathBuf};
use vosu_adapter_support::json::{push_str, write_str_content};

#[inline]
fn is_simple_segment(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.bytes().any(|b| matches!(b, b'/' | b'\\' | b':'))
}

pub(super) fn direct_join(folder: Option<&str>, file: Option<&str>) -> String {
    let folder = folder.unwrap_or("");
    let file = file.unwrap_or("");
    if folder.is_empty() && file.is_empty() {
        return ".".to_owned();
    }
    if folder.is_empty() && is_simple_segment(file) {
        return file.to_owned();
    }
    if file.is_empty() && is_simple_segment(folder) {
        return folder.to_owned();
    }
    if is_simple_segment(folder) && is_simple_segment(file) {
        let mut out =
            String::with_capacity(folder.len() + std::path::MAIN_SEPARATOR_STR.len() + file.len());
        out.push_str(folder);
        out.push_str(std::path::MAIN_SEPARATOR_STR);
        out.push_str(file);
        return out;
    }
    direct_join_normalized(folder, file)
}

/// Writes the JSON-quoted result of `direct_join(folder, file)` directly to
/// `out`, allocating only when path normalization is required.
#[inline]
pub(super) fn push_direct_join(out: &mut Vec<u8>, folder: Option<&str>, file: Option<&str>) {
    let folder = folder.unwrap_or("");
    let file = file.unwrap_or("");
    if folder.is_empty() && file.is_empty() {
        out.extend_from_slice(b"\".\"");
        return;
    }
    if folder.is_empty() && is_simple_segment(file) {
        push_str(out, file);
        return;
    }
    if file.is_empty() && is_simple_segment(folder) {
        push_str(out, folder);
        return;
    }
    if is_simple_segment(folder) && is_simple_segment(file) {
        out.reserve(folder.len() + file.len() + 3);
        out.push(b'"');
        write_str_content(out, folder);
        write_str_content(out, std::path::MAIN_SEPARATOR_STR);
        write_str_content(out, file);
        out.push(b'"');
        return;
    }
    let joined = direct_join_normalized(folder, file);
    push_str(out, &joined);
}

#[cold]
#[inline(never)]
fn direct_join_normalized(folder: &str, file: &str) -> String {
    let mut joined = PathBuf::from(folder);
    if !file.is_empty() {
        if !folder.is_empty() {
            let mut combined = std::ffi::OsString::from(folder);
            combined.push(std::path::MAIN_SEPARATOR_STR);
            combined.push(file);
            joined = PathBuf::from(combined);
        } else {
            joined = PathBuf::from(file);
        }
    }
    let mut normalized = PathBuf::new();

    for component in joined.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));
                if can_pop {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        normalized.to_string_lossy().into_owned()
    }
}

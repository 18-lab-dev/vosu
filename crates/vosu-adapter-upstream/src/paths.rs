//! Path strings are upstream echoes only, never filesystem authority.
use std::path::{Component, PathBuf};

pub(super) fn direct_join(folder: Option<&str>, file: Option<&str>) -> String {
    let folder = folder.unwrap_or("");
    let file = file.unwrap_or("");
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

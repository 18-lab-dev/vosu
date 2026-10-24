use super::*;
use crate::model::memory::CurrentSkin;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 20_000;
const MAX_SCAN: usize = 40_000;

#[derive(Debug, Serialize)]
pub struct SkinEntry {
    pub path: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: Option<SystemTime>,
}
impl Stamp {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            size: meta.len(),
            modified: meta.modified().ok(),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct SkinFileIndex {
    pub entries: Vec<SkinEntry>,
    pub truncated: bool,
    #[serde(skip)]
    folded: BTreeMap<String, usize>,
    #[serde(skip)]
    stamps: Vec<(String, Stamp)>,
    #[serde(skip)]
    directories: BTreeMap<String, Vec<usize>>,
}

impl SkinFileIndex {
    pub fn modified(&self) -> Option<SystemTime> {
        self.stamps
            .iter()
            .filter_map(|(_, stamp)| stamp.modified)
            .max()
    }
    fn unchanged(&self, root: &Path, all: bool) -> bool {
        (!all || !self.truncated)
            && !self.stamps.is_empty()
            && self
                .stamps
                .iter()
                .take(if all { usize::MAX } else { 1 })
                .all(|(path, stamp)| {
                    safe_child(root, path)
                        .and_then(|child| secure_open(&child))
                        .is_ok_and(|opened| Stamp::of(&opened.metadata) == *stamp)
                })
    }
    fn parent_unchanged(&self, root: &Path, requested: &str) -> bool {
        let mut parent = requested.rsplit_once('/').map_or("", |(parent, _)| parent);
        loop {
            if parent.is_empty() {
                return true;
            } // get() already checked the root.
            if let Some(indices) = self.directories.get(&parent.to_lowercase())
                && !indices.iter().all(|&index| {
                    let (path, stamp) = &self.stamps[index];
                    safe_child(root, path)
                        .and_then(|child| secure_open(&child))
                        .is_ok_and(|opened| Stamp::of(&opened.metadata) == *stamp)
                })
            {
                return false;
            }
            parent = parent.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }

    fn finish(&mut self) {
        self.entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        for (index, entry) in self.entries.iter().enumerate() {
            self.folded
                .entry(entry.path.to_lowercase())
                .or_insert(index);
        }
    }
}

struct Cached {
    root: Option<PathBuf>,
    skin: Option<Arc<CurrentSkin>>,
    name: Option<String>,
    lazer: bool,
    index: Arc<SkinFileIndex>,
}

#[derive(Default)]
pub struct SkinIndexCache {
    current: Option<Cached>,
}

impl SkinIndexCache {
    pub fn get(
        &mut self,
        root: Option<&Path>,
        skin: Option<&Arc<CurrentSkin>>,
        name: Option<&str>,
        lazer: bool,
        validate: bool,
    ) -> Arc<SkinFileIndex> {
        if let Some(cached) = &self.current {
            let same_skin = match (skin, &cached.skin) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if cached.root.as_deref() == root
                && cached.name.as_deref() == name
                && cached.lazer == lazer
                && same_skin
                && ((lazer && skin.is_some_and(|s| s.builtin.is_some()))
                    || root.is_none_or(|root| cached.index.unchanged(root, validate)))
            {
                return Arc::clone(&cached.index);
            }
        }
        let index = Arc::new(if lazer {
            lazer_index(root, skin.map(Arc::as_ref))
        } else {
            folder_index(root)
        });
        self.current = Some(Cached {
            root: root.map(Path::to_owned),
            skin: skin.cloned(),
            name: name.map(str::to_owned),
            lazer,
            index: Arc::clone(&index),
        });
        index
    }

    pub fn resolve_folder(
        &mut self,
        root: Option<&Path>,
        name: Option<&str>,
        encoded: &str,
        target: &str,
        missing: MissingKind,
    ) -> Result<ResourceOutcome, SafePathError> {
        let decoded = match decode_request_path(encoded) {
            Ok(name) => name.replace('\\', "/"),
            Err(_) => return Ok(ResourceOutcome::Missing(MissingKind::NotFound)),
        };
        if !valid_relative(&decoded) {
            return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
        }
        let exact = resolve_resource(root, &decoded, target, missing)?;
        if !matches!(exact, ResourceOutcome::Missing(_)) {
            return Ok(exact);
        }
        let Some(root) = root else {
            return Ok(exact);
        };
        let mut index = self.get(Some(root), None, name, false, false);
        if !index.parent_unchanged(root, &decoded) {
            self.current = None;
            index = self.get(Some(root), None, name, false, false);
        }
        if let Some(&entry) = index.folded.get(&decoded.to_lowercase()) {
            let outcome =
                resolve_resource(Some(root), &index.entries[entry].path, target, missing)?;
            if !matches!(outcome, ResourceOutcome::Missing(_)) {
                return Ok(outcome);
            }
            self.current = None;
        }
        if !index.truncated {
            return Ok(exact);
        }
        // Handles newly added files and truncated indexes without trusting an
        // unconfined path. The final open always uses the normal secure resolver.
        if let Some(path) = case_path(root, &decoded)? {
            self.current = None;
            return resolve_resource(Some(root), &path, target, missing);
        }
        Ok(exact)
    }
}

fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains(':'))
}

fn case_path(root: &Path, requested: &str) -> Result<Option<String>, SafePathError> {
    let mut relative = String::new();
    for part in requested.split('/') {
        let child = safe_child(root, &relative)?;
        let directory = match secure_open(&child) {
            Ok(opened) if opened.metadata.is_dir() => opened,
            _ => return Ok(None),
        };
        let mut best: Option<String> = None;
        let folded = part.to_lowercase();
        directory::visit_entries(&directory.file, |name, _| {
            if name.to_lowercase() == folded
                && best
                    .as_ref()
                    .is_none_or(|old| name == part || (old != part && name < *old))
            {
                best = Some(name);
            }
            true
        })?;
        let Some(part) = best else {
            return Ok(None);
        };
        if !relative.is_empty() {
            relative.push('/');
        }
        relative.push_str(&part);
    }
    Ok(Some(relative))
}

fn folder_index(root: Option<&Path>) -> SkinFileIndex {
    let mut index = SkinFileIndex::default();
    let Some(root) = root else {
        return index;
    };
    let mut stack = vec![(String::new(), 0usize)];
    let mut visited = BTreeSet::new();
    let mut scanned = 0;
    while let Some((relative, depth)) = stack.pop() {
        if depth > 128 {
            index.truncated = true;
            continue;
        }
        let Ok(child) = safe_child(root, &relative) else {
            continue;
        };
        let Ok(opened) = secure_open(&child) else {
            continue;
        };
        if !opened.metadata.is_dir() {
            continue;
        }
        let Ok(canonical) = fs::canonicalize(root.join(&relative)) else {
            index.truncated = true;
            continue;
        };
        if !visited.insert(canonical) {
            index.truncated = true;
            continue;
        }
        index
            .directories
            .entry(relative.to_lowercase())
            .or_default()
            .push(index.stamps.len());
        index
            .stamps
            .push((relative.clone(), Stamp::of(&opened.metadata)));
        let Ok(entries) =
            directory::directory_entries_bounded(&opened.file, MAX_SCAN - scanned + 1)
        else {
            index.truncated = true;
            continue;
        };
        for (name, _) in entries {
            scanned += 1;
            if scanned > MAX_SCAN || index.entries.len() >= MAX_ENTRIES {
                index.truncated = true;
                break;
            }
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if !valid_relative(&path) {
                continue;
            }
            let Ok(child) = safe_child(root, &path) else {
                continue;
            };
            let opened = match secure_open(&child) {
                Ok(opened) => opened,
                Err(SafePathError::Io(_)) => {
                    index.truncated = true;
                    continue;
                }
                Err(_) => continue,
            };
            if opened.metadata.is_dir() {
                stack.push((path, depth + 1));
            } else if opened.metadata.is_file() {
                let modified = opened
                    .metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .and_then(|d| u64::try_from(d.as_millis()).ok());
                if modified.is_none() {
                    index.truncated = true;
                    continue;
                }
                index
                    .stamps
                    .push((path.clone(), Stamp::of(&opened.metadata)));
                index.entries.push(SkinEntry {
                    path,
                    size: opened.metadata.len(),
                    modified,
                    hash: None,
                });
            }
        }
        if scanned > MAX_SCAN || index.entries.len() >= MAX_ENTRIES {
            if !stack.is_empty() {
                index.truncated = true;
            }
            break;
        }
    }
    index.finish();
    index
}

fn lazer_index(root: Option<&Path>, skin: Option<&CurrentSkin>) -> SkinFileIndex {
    let mut index = SkinFileIndex::default();
    let (Some(root), Some(skin)) = (root, skin) else {
        return index;
    };
    if skin.builtin.is_some() {
        return index;
    }
    if let Ok(opened) = safe_child(root, "").and_then(|child| secure_open(&child)) {
        index
            .stamps
            .push((String::new(), Stamp::of(&opened.metadata)));
    }
    for (path, relative) in &skin.files {
        if index.entries.len() >= MAX_ENTRIES {
            index.truncated = true;
            break;
        }
        let path = path.replace('\\', "/");
        if !valid_relative(&path) {
            continue;
        }
        let Ok(child) = safe_child(root, relative) else {
            continue;
        };
        let Ok(opened) = secure_open(&child) else {
            index.truncated = true;
            continue;
        };
        if !opened.metadata.is_file() {
            continue;
        }
        let Some(hash) = Path::new(relative)
            .file_name()
            .and_then(|p| p.to_str())
            .filter(|hash| {
                matches!(hash.len(), 32 | 64) && hash.bytes().all(|b| b.is_ascii_hexdigit())
            })
        else {
            continue;
        };
        index
            .stamps
            .push((relative.clone(), Stamp::of(&opened.metadata)));
        index.entries.push(SkinEntry {
            path,
            size: opened.metadata.len(),
            modified: None,
            hash: Some(hash.to_owned()),
        });
    }
    index.finish();
    index
}

#[cfg(test)]
mod tests;

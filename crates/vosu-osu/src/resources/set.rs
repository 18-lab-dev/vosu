use super::{identity::digest, roots::Root};
use std::collections::BTreeMap;
use vosu_plugin_api::{ResourceResolved, TreeEntry};
use vosu_sys::fs::SafeRelativePath;

pub const MAX_ENTRIES: usize = 20_000;
const MAX_SCAN: usize = 40_000;
const MAX_PATH: usize = 1024;

/// Stable relative folder or lazer logical-name -> content-store-path mapping.
/// An empty mapping also represents a builtin skin with no exported files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeSource {
    Folder(String),
    Store(BTreeMap<String, String>),
}

pub(super) struct Member {
    pub entry: TreeEntry,
    pub physical: String,
}
pub(super) struct Tree {
    pub root: &'static str,
    pub members: BTreeMap<String, Member>,
    pub truncated: bool,
    pub revision: String,
}

pub(super) fn logical(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.is_empty()
        || path.len() > MAX_PATH
        || path.contains(['\0', ':'])
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return None;
    }
    SafeRelativePath::parse(&path).ok()?;
    Some(path)
}

impl Tree {
    pub fn prepare(root: &Root, source: &TreeSource) -> Self {
        let mut tree = Self {
            root: root.name,
            members: BTreeMap::new(),
            truncated: false,
            revision: String::new(),
        };
        match source {
            TreeSource::Store(files) => {
                for (name, physical) in files.iter().take(MAX_SCAN) {
                    tree.add(root, name, physical, store_hash(physical));
                }
                tree.truncated |= files.len() > MAX_SCAN;
            }
            TreeSource::Folder(folder) => {
                // Enumeration yields names only. Every directory and every file
                // is independently opened beneath the retained root before use.
                let folder = folder.replace('\\', "/");
                if !folder.is_empty() && logical(&folder).is_none() {
                    return tree;
                }
                let mut pending = vec![String::new()];
                let mut scanned = 0;
                while let Some(relative) = pending.pop() {
                    let physical = join(&folder, &relative);
                    let Some(directory) = SafeRelativePath::parse(&physical)
                        .ok()
                        .and_then(|p| root.safe.open_child(&p).ok())
                        .filter(|c| c.metadata.is_dir())
                    else {
                        continue;
                    };
                    // Retain `directory` while enumerating; child opens below
                    // remain authoritative even if the configured name changes.
                    let Ok(entries) = std::fs::read_dir(root.path.join(&physical)) else {
                        continue;
                    };
                    for entry in entries {
                        scanned += 1;
                        if scanned > MAX_SCAN {
                            tree.truncated = true;
                            break;
                        }
                        let Ok(entry) = entry else {
                            tree.truncated = true;
                            continue;
                        };
                        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                            continue;
                        };
                        let Some(name) = logical(&join(&relative, &name)) else {
                            continue;
                        };
                        let physical = join(&folder, &name);
                        let Some(child) = SafeRelativePath::parse(&physical)
                            .ok()
                            .and_then(|p| root.safe.open_child(&p).ok())
                        else {
                            continue;
                        };
                        if child.metadata.is_dir() {
                            pending.push(name);
                        } else if child.metadata.is_file() {
                            tree.add(root, &name, &physical, None);
                        }
                    }
                    drop(directory);
                    if scanned > MAX_SCAN {
                        break;
                    }
                }
            }
        }
        let mut revision = Vec::new();
        for member in tree.members.values() {
            revision.extend_from_slice(member.entry.path.as_bytes());
            revision.push(0);
            revision.extend_from_slice(&member.entry.size.to_le_bytes());
            revision.extend_from_slice(member.entry.hash.as_deref().unwrap_or_default().as_bytes());
            revision.push(0);
        }
        revision.push(u8::from(tree.truncated));
        tree.revision = digest(&revision);
        tree
    }

    fn add(&mut self, root: &Root, name: &str, physical: &str, hash: Option<String>) {
        if self.members.len() >= MAX_ENTRIES {
            self.truncated = true;
            return;
        }
        let (Some(name), Some(physical)) = (logical(name), logical(physical)) else {
            return;
        };
        let Some(child) = SafeRelativePath::parse(&physical)
            .ok()
            .and_then(|p| root.safe.open_child(&p).ok())
        else {
            self.truncated = true;
            return;
        };
        if !child.metadata.is_file() {
            return;
        }
        self.members.entry(name.clone()).or_insert(Member {
            entry: TreeEntry {
                path: name,
                size: child.metadata.len(),
                hash,
            },
            physical,
        });
    }

    pub fn list(&self) -> ResourceResolved {
        ResourceResolved::Tree {
            entries: self.members.values().map(|m| m.entry.clone()).collect(),
            truncated: self.truncated,
            revision: self.revision.clone(),
        }
    }

    pub fn lookup(&self, requested: &str, folded: bool) -> Option<ResourceResolved> {
        let name = logical(requested)?;
        let member = self.members.get(&name).or_else(|| {
            folded
                .then(|| {
                    let folded = name.to_lowercase();
                    self.members
                        .iter()
                        .find(|(n, _)| n.to_lowercase() == folded)
                        .map(|(_, m)| m)
                })
                .flatten()
        })?;
        Some(ResourceResolved::TreePath {
            root: self.root.into(),
            path: member.physical.clone(),
            entry_path: member.entry.path.clone(),
        })
    }
}

fn join(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.into()
    } else if child.is_empty() {
        parent.into()
    } else {
        format!("{parent}/{child}")
    }
}
fn store_hash(path: &str) -> Option<String> {
    let hash = path.rsplit(['/', '\\']).next()?;
    (matches!(hash.len(), 32 | 64) && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hash.into())
}

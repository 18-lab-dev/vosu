//! Transitional 1.x lazer skin policy, owned by the osu! provider until M4a.
//! Stable folder serving deliberately remains in the legacy server.
use std::path::Path;
use vosu_osu_model::memory::CurrentSkin;
use vosu_sys::fs::{SafeRelativePath, SafeRoot};

pub struct Entry {
    pub path: String,
    pub size: u64,
    pub hash: String,
}
#[derive(Default)]
pub struct Index {
    pub entries: Vec<Entry>,
    pub stamps: Vec<(String, std::fs::Metadata)>,
    pub truncated: bool,
}
fn valid(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.contains(':'))
}
/// Input is decoded exactly once by the HTTP adapter. Preserve the legacy
/// lower-case mapping lookup, including its missing/builtin behavior.
pub fn lookup<'a>(skin: Option<&'a CurrentSkin>, name: &str) -> Option<&'a str> {
    if !valid(name) {
        return None;
    }
    skin?.files.get(&name.to_lowercase()).map(String::as_str)
}
pub fn index(root: Option<&Path>, skin: Option<&CurrentSkin>) -> Index {
    let mut index = Index::default();
    let (Some(root), Some(skin)) = (root, skin) else {
        return index;
    };
    if skin.builtin.is_some() {
        return index;
    }
    let root = SafeRoot::open(root).ok();
    if let Some(root) = root.as_ref()
        && let Ok(opened) = SafeRelativePath::parse("").and_then(|child| root.open_child(&child))
    {
        index.stamps.push((String::new(), opened.metadata));
    }
    for (path, relative) in &skin.files {
        if index.entries.len() >= 20_000 {
            index.truncated = true;
            break;
        }
        let path = path.replace('\\', "/");
        if !valid(&path) {
            continue;
        }
        let Ok(child) = SafeRelativePath::parse(relative) else {
            continue;
        };
        let Some(opened) = root.as_ref().and_then(|root| root.open_child(&child).ok()) else {
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
        index.entries.push(Entry {
            path,
            size: opened.metadata.len(),
            hash: hash.to_owned(),
        });
        index.stamps.push((relative.clone(), opened.metadata));
    }
    index
}

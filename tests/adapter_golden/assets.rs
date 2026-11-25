//! Replay assets: the only file and resource authority a replay has.
//!
//! Each root slot becomes a real temporary directory that the replay game
//! declares for the selected session through `VOSU_OP_ROOT_DECLARE`.
//! Each resource is a file inside one of those roots, published as a
//! `vosu.chart` resource reference and resolved to that root-relative path.
//! Paths recorded in state are data only: nothing here reads them, and no
//! recorded path ever becomes a root or a resource.
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::SystemTime};
use tempfile::TempDir;

/// One file written into a root slot.
#[derive(Clone, Debug)]
pub struct AssetFile {
    /// Root-relative, `/`-separated. No empty, `.`, `..` or rooted component.
    pub path: String,
    pub bytes: Vec<u8>,
    /// The declared modification time, when a case depends on it.
    pub modified: Option<SystemTime>,
}

/// One declared root slot of the osu! manifest (`songs`, `skins`, `files`).
#[derive(Clone, Debug)]
pub struct RootAsset {
    pub slot: String,
    pub files: Vec<AssetFile>,
}

/// A non-tree `vosu.chart` resource reference (`/resources/<kind>`), backed
/// by one root file.
#[derive(Clone, Debug)]
pub struct ResourceAsset {
    pub kind: String,
    /// The reference name. Its extension selects the published media type.
    pub name: String,
    pub slot: String,
    pub path: String,
}

#[derive(Clone, Debug, Default)]
pub struct ReplayAssets {
    pub roots: Vec<RootAsset>,
    pub resources: Vec<ResourceAsset>,
}

impl ReplayAssets {
    /// One chart-text resource: `name` inside a single-file root `slot`.
    pub fn chart(slot: &str, name: &str, bytes: &[u8]) -> Self {
        Self {
            roots: vec![RootAsset {
                slot: slot.into(),
                files: vec![AssetFile {
                    path: name.into(),
                    bytes: bytes.to_vec(),
                    modified: None,
                }],
            }],
            resources: vec![ResourceAsset {
                kind: "file".into(),
                name: name.into(),
                slot: slot.into(),
                path: name.into(),
            }],
        }
    }

    pub fn resource(&self, kind: &str) -> Option<&ResourceAsset> {
        self.resources.iter().find(|resource| resource.kind == kind)
    }

    /// Checks the declarations without writing anything.
    pub fn validate(&self) -> Result<(), String> {
        for (index, root) in self.roots.iter().enumerate() {
            if !matches!(root.slot.as_str(), "songs" | "skins" | "files") {
                return Err(format!(
                    "asset root {:?} is not an osu! root slot",
                    root.slot
                ));
            }
            if self.roots[..index]
                .iter()
                .any(|other| other.slot == root.slot)
            {
                return Err(format!("asset root {:?} is declared twice", root.slot));
            }
            for (at, file) in root.files.iter().enumerate() {
                components(&file.path)?;
                if root.files[..at].iter().any(|other| other.path == file.path) {
                    return Err(format!("asset file {:?} is declared twice", file.path));
                }
            }
        }
        for (index, resource) in self.resources.iter().enumerate() {
            if !matches!(resource.kind.as_str(), "file" | "audio" | "background") {
                return Err(format!(
                    "asset resource kind {:?} is not a replayable non-tree kind",
                    resource.kind
                ));
            }
            if self.resources[..index]
                .iter()
                .any(|other| other.kind == resource.kind)
            {
                return Err(format!(
                    "asset resource {:?} is declared twice",
                    resource.kind
                ));
            }
            if self.file(&resource.slot, &resource.path).is_none() {
                return Err(format!(
                    "asset resource {:?} names {}:{}, which is not an asset file",
                    resource.kind, resource.slot, resource.path
                ));
            }
        }
        Ok(())
    }

    fn file(&self, slot: &str, path: &str) -> Option<&AssetFile> {
        self.roots
            .iter()
            .find(|root| root.slot == slot)?
            .files
            .iter()
            .find(|file| file.path == path)
    }

    /// The opaque content identity a resource reference carries.
    pub fn content_id(&self, resource: &ResourceAsset) -> String {
        let bytes = &self
            .file(&resource.slot, &resource.path)
            .expect("validated resource file")
            .bytes;
        let mut out = String::from("sha256:");
        for byte in Sha256::digest(bytes) {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// Writes every root into a fresh temporary directory.
    pub fn materialize(&self) -> Result<Materialized, String> {
        self.validate()?;
        let dir = TempDir::new().map_err(|e| format!("asset root directory: {e}"))?;
        let mut roots = Vec::with_capacity(self.roots.len());
        for root in &self.roots {
            let base = dir.path().join(&root.slot);
            std::fs::create_dir_all(&base).map_err(|e| format!("asset root: {e}"))?;
            for file in &root.files {
                let mut target = base.clone();
                for component in components(&file.path)? {
                    target.push(component);
                }
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("asset parent: {e}"))?;
                }
                std::fs::write(&target, &file.bytes).map_err(|e| format!("asset file: {e}"))?;
                if let Some(modified) = file.modified {
                    std::fs::File::options()
                        .write(true)
                        .open(&target)
                        .and_then(|handle| handle.set_modified(modified))
                        .map_err(|e| format!("asset mtime: {e}"))?;
                }
            }
            roots.push((root.slot.clone(), base));
        }
        Ok(Materialized { roots, _dir: dir })
    }
}

/// Real temporary roots, removed when dropped.
pub struct Materialized {
    pub roots: Vec<(String, PathBuf)>,
    _dir: TempDir,
}

fn components(path: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.iter().any(|part| {
        part.is_empty()
            || matches!(*part, "." | "..")
            || part.contains(['\\', ':'])
            || part.ends_with([' ', '.'])
    }) {
        return Err(format!("asset path {path:?} is not a plain relative path"));
    }
    Ok(parts)
}

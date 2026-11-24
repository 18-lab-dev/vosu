use std::path::PathBuf;
use vosu_plugin_api::RootDeclare;
use vosu_sys::fs::SafeRoot;

/// Explicit selection roots. Missing or empty roots never mean the working directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootPaths {
    pub songs: Option<PathBuf>,
    pub skin: Option<PathBuf>,
}

pub(super) struct Root {
    pub name: &'static str,
    pub path: PathBuf,
    pub safe: SafeRoot,
}

impl Root {
    pub fn open(name: &'static str, path: Option<PathBuf>) -> Option<Self> {
        let path = path?;
        path.to_str()?;
        let safe = SafeRoot::open(&path).ok()?;
        Some(Self { name, path, safe })
    }

    pub fn declaration(&self, handle: u64) -> Option<RootDeclare> {
        Some(RootDeclare {
            handle: Some(handle),
            name: self.name.into(),
            path: self.path.to_str()?.into(),
        })
    }
}

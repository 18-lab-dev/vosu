//! Per-overlay nicknames and URL aliases chosen on the dashboard.
//!
//! The overlay's folder stays its identity: settings, profiles, command
//! sockets and `COUNTER_PATH` all keep using it. A nickname only changes how
//! the dashboard names the overlay. An alias is a short top-level path on the
//! overlay listener (`/acc`) that redirects to the folder (`/accuracy by
//! sxlvia/`), so browser sources may use either URL while relative assets keep
//! resolving against the real folder.
//!
//! Names persist atomically in `<config dir>/overlay-names.json`, keyed by
//! folder. Entries whose folder disappears are dropped on the next discovery.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// First path segments the overlay listener (or the dashboard) already
/// routes; an alias may not shadow them. Compared case-insensitively.
const RESERVED: &[&str] = &[
    "api",
    "assets",
    "available",
    "backgroundimage",
    "favicon.ico",
    "files",
    "json",
    "settings",
    "songs",
    "tokens",
    "tools",
    "websocket",
    "ws",
];
const MAX_ALIAS: usize = 48;
const MAX_NICKNAME: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayName {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

pub struct OverlayNames {
    path: PathBuf,
    entries: RwLock<BTreeMap<String, OverlayName>>,
}

impl OverlayNames {
    /// Loads saved names; a missing file is empty. An unreadable or malformed
    /// file is kept on disk untouched and treated as empty until it is fixed or
    /// replaced by the next successful save.
    pub fn load(path: PathBuf) -> Self {
        let entries = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            entries: RwLock::new(entries),
        }
    }

    pub fn snapshot(&self) -> BTreeMap<String, OverlayName> {
        self.entries.read().clone()
    }

    /// Folder an alias redirects to.
    pub fn folder_for_alias(&self, alias: &str) -> Option<String> {
        self.entries
            .read()
            .iter()
            .find(|(_, name)| {
                name.alias
                    .as_deref()
                    .is_some_and(|value| value.eq_ignore_ascii_case(alias))
            })
            .map(|(folder, _)| folder.clone())
    }

    /// Replaces the nickname and alias of `folder`. Blank values clear them.
    /// `static_root` is checked for the folder and for top-level names an
    /// alias would collide with.
    pub fn set(
        &self,
        static_root: &Path,
        folder: &str,
        nickname: Option<&str>,
        alias: Option<&str>,
    ) -> Result<OverlayName, String> {
        if folder.is_empty() || !static_root.join(folder).is_dir() {
            return Err("overlay folder not found".into());
        }
        let nickname = nickname.map(str::trim).filter(|value| !value.is_empty());
        if let Some(value) = nickname
            && (value.chars().count() > MAX_NICKNAME || value.chars().any(char::is_control))
        {
            return Err(format!(
                "nickname must be at most {MAX_NICKNAME} characters without control characters"
            ));
        }
        let alias = alias
            .map(|value| value.trim().trim_matches('/'))
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let mut entries = self.entries.write();
        if let Some(value) = alias.as_deref() {
            validate_alias(value)?;
            if RESERVED.contains(&value) {
                return Err(format!("/{value} is reserved by vosu"));
            }
            if top_level_entries(static_root)
                .iter()
                .any(|name| name.eq_ignore_ascii_case(value))
            {
                return Err(format!("/{value} is already an overlay folder"));
            }
            if entries.iter().any(|(other, name)| {
                other != folder
                    && name
                        .alias
                        .as_deref()
                        .is_some_and(|existing| existing.eq_ignore_ascii_case(value))
            }) {
                return Err(format!("/{value} is already used by another overlay"));
            }
        }
        let name = OverlayName {
            nickname: nickname.map(str::to_owned),
            alias,
        };
        let mut next = entries.clone();
        if name == OverlayName::default() {
            next.remove(folder);
        } else {
            next.insert(folder.to_owned(), name.clone());
        }
        persist(&self.path, &next).map_err(|error| format!("saving overlay names: {error}"))?;
        *entries = next;
        Ok(name)
    }

    /// Drops names whose folder no longer exists (deleted or renamed outside
    /// vosu). Persists only when something changed.
    pub fn retain_folders<'a>(&self, folders: impl IntoIterator<Item = &'a str>) -> io::Result<()> {
        let present = folders
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let mut entries = self.entries.write();
        if entries
            .keys()
            .all(|folder| present.contains(folder.as_str()))
        {
            return Ok(());
        }
        let next = entries
            .iter()
            .filter(|(folder, _)| present.contains(folder.as_str()))
            .map(|(folder, name)| (folder.clone(), name.clone()))
            .collect();
        persist(&self.path, &next)?;
        *entries = next;
        Ok(())
    }

    pub fn remove(&self, folder: &str) -> io::Result<()> {
        let mut entries = self.entries.write();
        if !entries.contains_key(folder) {
            return Ok(());
        }
        let mut next = entries.clone();
        next.remove(folder);
        persist(&self.path, &next)?;
        *entries = next;
        Ok(())
    }
}

/// Lowercase ASCII letters, digits, `-` and `_`, starting with a letter or
/// digit: safe as a single unencoded URL path segment on every platform.
fn validate_alias(alias: &str) -> Result<(), String> {
    let mut chars = alias.chars();
    let valid = alias.len() <= MAX_ALIAS
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "URL alias must be 1-{MAX_ALIAS} lowercase letters, digits, '-' or '_', starting with a letter or digit"
        ))
    }
}

fn top_level_entries(static_root: &Path) -> Vec<String> {
    fs::read_dir(static_root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn persist(path: &Path, entries: &BTreeMap<String, OverlayName>) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("overlay names path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, entries)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let statics = root.path().join("static");
        for folder in ["accuracy by sxlvia", "pp", "nested/inner"] {
            fs::create_dir_all(statics.join(folder)).unwrap();
        }
        let file = root.path().join("config/overlay-names.json");
        (root, statics, file)
    }

    #[test]
    fn aliases_validate_collisions_and_survive_reload() {
        let (_root, statics, file) = fixture();
        let names = OverlayNames::load(file.clone());
        let set = names
            .set(
                &statics,
                "accuracy by sxlvia",
                Some("  Accuracy  "),
                Some("/ACC/"),
            )
            .unwrap();
        assert_eq!(set.nickname.as_deref(), Some("Accuracy"));
        assert_eq!(set.alias.as_deref(), Some("acc"));
        assert_eq!(
            names.folder_for_alias("Acc").as_deref(),
            Some("accuracy by sxlvia")
        );
        for (alias, expected) in [
            ("tools", "reserved"),
            ("api", "reserved"),
            ("websocket", "reserved"),
            ("Files", "reserved"),
            ("pp", "overlay folder"),
            ("nested", "overlay folder"),
            ("bad alias", "URL alias"),
            ("../x", "URL alias"),
            ("-x", "URL alias"),
            ("é", "URL alias"),
        ] {
            let error = names.set(&statics, "pp", None, Some(alias)).unwrap_err();
            assert!(error.contains(expected), "{alias}: {error}");
        }
        assert!(
            names
                .set(&statics, "pp", None, Some("acc"))
                .unwrap_err()
                .contains("another overlay")
        );
        // Re-saving the same alias for its own folder is not a collision.
        names
            .set(&statics, "accuracy by sxlvia", Some("Acc"), Some("acc"))
            .unwrap();
        assert!(names.set(&statics, "missing", Some("x"), None).is_err());
        assert!(
            names
                .set(&statics, "pp", Some(&"x".repeat(65)), None)
                .is_err()
        );

        let reloaded = OverlayNames::load(file.clone());
        assert_eq!(reloaded.snapshot(), names.snapshot());
        // Clearing both values removes the entry.
        reloaded
            .set(&statics, "accuracy by sxlvia", Some(" "), Some(""))
            .unwrap();
        assert!(reloaded.snapshot().is_empty());
        assert_eq!(fs::read_to_string(file).unwrap().trim(), "{}");
    }

    #[test]
    fn removed_or_renamed_folders_drop_their_names() {
        let (_root, statics, file) = fixture();
        let names = OverlayNames::load(file.clone());
        names.set(&statics, "pp", None, Some("p")).unwrap();
        names
            .set(&statics, "accuracy by sxlvia", Some("Acc"), None)
            .unwrap();
        names.retain_folders(["pp"]).unwrap();
        assert_eq!(names.snapshot().len(), 1);
        names.remove("pp").unwrap();
        assert!(OverlayNames::load(file).snapshot().is_empty());
    }
}

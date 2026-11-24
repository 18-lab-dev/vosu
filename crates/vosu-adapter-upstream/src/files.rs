//! Legacy game-file policy over opaque, session-bound declared roots.
use serde::Deserialize;
use std::sync::Arc;
use vosu_plugin_api::Selector;
use vosu_surface::{
    files::{DirectoryEntry, EntryKind, FileError, OpenedFile, RelativeChild, RootLease},
    requester::BundledRequester,
    state::{IdentifiedSession, SessionTarget},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Background,
    Audio,
    Beatmap,
    Songs,
    BeatmapPath,
    Skin,
}
pub enum Outcome {
    File {
        file: OpenedFile,
        name: String,
        lazer: bool,
    },
    Directory {
        entries: Vec<DirectoryEntry>,
        truncated: bool,
    },
    Redirect(String),
    Missing,
    NotReady,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Current {
    beatmap_folder: Option<String>,
    beatmap_file: Option<String>,
    background: Option<String>,
    audio: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Paths {
    current: Option<Current>,
}

/// No pathname-based fallback exists. I0 injects the registered adapter requester.
#[derive(Clone, Default)]
pub struct Provider {
    requester: Option<Arc<dyn BundledRequester>>,
}
pub struct Selection {
    pub(crate) requester: Arc<dyn BundledRequester>,
    pub(crate) session: IdentifiedSession,
    current: Current,
    pub(crate) chart: Option<String>,
}
impl Provider {
    pub fn new(requester: Arc<dyn BundledRequester>) -> Self {
        Self {
            requester: Some(requester),
        }
    }
    pub fn capture(&self) -> Result<Selection, FileError> {
        let requester = self.requester.as_ref().ok_or(FileError::NotReady)?;
        let capture = requester
            .capture(
                SessionTarget::Focused,
                &[
                    Selector {
                        ns: "vosu.osu".into(),
                        pointer: "/internal/paths".into(),
                    },
                    Selector {
                        ns: "vosu.chart".into(),
                        pointer: "/resources/file".into(),
                    },
                ],
            )
            .map_err(|_| FileError::NotReady)?;
        let session = crate::view::Identity::from_capture(&capture)
            .map_err(|_| FileError::NotReady)?
            .session;
        let paths: Paths =
            crate::view::decode(&capture, "/internal/paths").map_err(|_| FileError::NotReady)?;
        let chart = crate::view::value(&capture, "vosu.chart", "/resources/file")
            .ok()
            .and_then(|value| value.as_map())
            .and_then(|fields| {
                fields.iter().find_map(|(key, value)| {
                    (key.as_text() == Some("url"))
                        .then(|| value.as_text())
                        .flatten()
                })
            })
            .and_then(|url| url.strip_prefix("/api/v1/resources/"))
            .map(str::to_owned);
        Ok(Selection {
            requester: Arc::clone(requester),
            session,
            current: paths.current.unwrap_or_default(),
            chart,
        })
    }
    pub async fn resolve(&self, route: Route, encoded: &str, target: &str) -> Outcome {
        // Retired index and lazer skin must not fall through to a browsable root.
        if route == Route::Skin && encoded.is_empty() {
            return Outcome::Missing;
        }
        let selected = match self.capture() {
            Ok(selected) => selected,
            Err(_) => return Outcome::NotReady,
        };
        selected.resolve(route, encoded, target).await
    }
}
impl Selection {
    pub async fn resolve(&self, route: Route, encoded: &str, target: &str) -> Outcome {
        let lazer = self.session.variant == "lazer";
        if route == Route::Skin && (lazer || encoded.is_empty()) {
            return Outcome::Missing;
        }
        let slot = if route == Route::Skin {
            "skins"
        } else if lazer {
            "files"
        } else {
            "songs"
        };
        let root = match self.requester.declared_root(&self.session, slot) {
            Ok(root) => root,
            Err(FileError::NotReady | FileError::NotFound) => return Outcome::NotReady,
            Err(_) => return Outcome::Missing,
        };
        let exact = matches!(route, Route::Background | Route::Audio | Route::Beatmap);
        let relative = if exact {
            let leaf = match route {
                Route::Background => &self.current.background,
                Route::Audio => &self.current.audio,
                _ => &self.current.beatmap_file,
            };
            let Some(leaf) = leaf.as_deref().filter(|s| !s.is_empty()) else {
                return Outcome::Missing;
            };
            match self
                .current
                .beatmap_folder
                .as_deref()
                .filter(|s| !s.is_empty())
            {
                Some(folder) => format!("{folder}/{leaf}"),
                None => leaf.to_owned(),
            }
        } else {
            match decode_once(encoded) {
                Ok(path) => path,
                Err(_) => return Outcome::Missing,
            }
        };
        let relative = if relative.contains('\\') {
            relative.replace('\\', "/")
        } else {
            relative
        };
        // The declared skins slot already retains the selected stable skin.
        if route == Route::Skin
            && relative
                .trim_end_matches('/')
                .split('/')
                .any(|p| p.is_empty() || matches!(p, "." | "..") || p.contains(':'))
        {
            return Outcome::Missing;
        }
        let child = match RelativeChild::parse(&relative) {
            Ok(child) => child,
            Err(_) => return Outcome::Missing,
        };
        let mut outcome = resolve_child(&root, &child, target, exact, lazer).await;
        if route == Route::Skin
            && matches!(outcome, Outcome::Missing)
            && let Ok(Some(path)) = case_path(&root, child.as_str()).await
        {
            let child = match RelativeChild::parse(&path) {
                Ok(child) => child,
                Err(_) => return Outcome::Missing,
            };
            outcome = resolve_child(&root, &child, target, false, lazer).await;
        }
        outcome
    }
}
async fn resolve_child(
    root: &RootLease,
    child: &RelativeChild,
    target: &str,
    exact: bool,
    lazer: bool,
) -> Outcome {
    match root.open(child).await {
        Ok(file) => {
            return Outcome::File {
                file,
                name: child.as_str().to_owned(),
                lazer,
            };
        }
        Err(FileError::NotReady) => return Outcome::NotReady,
        Err(FileError::TargetGone | FileError::PermissionDenied) => return Outcome::Missing,
        Err(_) => {}
    }
    if exact {
        return Outcome::Missing;
    }
    let Ok(directory) = root.directory(child).await else {
        return Outcome::Missing;
    };
    if !target
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .ends_with('/')
    {
        let (base, suffix) = target
            .find(['?', '#'])
            .map_or((target, ""), |i| target.split_at(i));
        return Outcome::Redirect(format!("{base}/{suffix}"));
    }
    directory_entries(directory).await
}

const MAX_DIRECTORY_ENTRIES: usize = 131_072;
const MAX_DIRECTORY_NAME_BYTES: usize = 16 * 1024 * 1024;
async fn directory_entries(
    mut directory: Box<dyn vosu_surface::files::DirectoryReader>,
) -> Outcome {
    // Bound the representation without confusing truncation with absence in lookup.
    let mut entries = Vec::new();
    let mut bytes = 0usize;
    loop {
        let page = match directory.next_page().await {
            Ok(page) => page,
            Err(_) => return Outcome::Missing,
        };
        for entry in page.entries {
            bytes += entry.name.len();
            if entries.len() == MAX_DIRECTORY_ENTRIES || bytes > MAX_DIRECTORY_NAME_BYTES {
                entries.sort_unstable_by(|a: &DirectoryEntry, b| a.name.cmp(&b.name));
                return Outcome::Directory {
                    entries,
                    truncated: true,
                };
            }
            entries.push(entry);
        }
        if !page.truncated {
            break;
        }
    }
    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Outcome::Directory {
        entries,
        truncated: false,
    }
}
/// Lexical full-path fallback, including case-colliding ancestor directories.
/// Each scan pages to completion but retains only one candidate per component.
async fn case_path(root: &RootLease, requested: &str) -> Result<Option<String>, FileError> {
    let folded: Vec<_> = requested
        .trim_end_matches('/')
        .split('/')
        .map(str::to_lowercase)
        .collect();
    let mut chosen: Vec<Option<String>> = vec![None; folded.len()];
    let mut depth = 0;
    let mut parent = String::new();
    loop {
        parent.clear();
        for name in &chosen[..depth] {
            if !parent.is_empty() {
                parent.push('/');
            }
            parent.push_str(name.as_deref().expect("selected ancestor"));
        }
        let after = chosen[depth].take();
        let mut best: Option<String> = None;
        match root.directory(&RelativeChild::parse(&parent)?).await {
            Ok(mut directory) => loop {
                let page = directory.next_page().await?;
                for entry in page.entries {
                    if entry.kind != EntryKind::Other
                        && (depth + 1 == folded.len() || entry.kind == EntryKind::Directory)
                        && entry.name.to_lowercase() == folded[depth]
                        && after.as_ref().is_none_or(|previous| entry.name > *previous)
                        && best.as_ref().is_none_or(|previous| entry.name < *previous)
                    {
                        best = Some(entry.name);
                    }
                }
                if !page.truncated {
                    break;
                }
            },
            Err(FileError::NotFound | FileError::InvalidPath | FileError::Io) => {}
            Err(error) => return Err(error),
        }
        if let Some(best) = best {
            chosen[depth] = Some(best);
            if depth + 1 == folded.len() {
                if !parent.is_empty() {
                    parent.push('/');
                }
                parent.push_str(chosen[depth].as_deref().expect("selected path"));
                return Ok(Some(parent));
            }
            depth += 1;
            chosen[depth] = None;
        } else if depth == 0 {
            return Ok(None);
        } else {
            depth -= 1;
        }
    }
}
fn decode_once(encoded: &str) -> Result<String, FileError> {
    let mut bytes = Vec::with_capacity(encoded.len());
    let mut source = encoded.bytes();
    while let Some(byte) = source.next() {
        if byte == b'%' {
            let high = source
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .ok_or(FileError::InvalidPath)?;
            let low = source
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .ok_or(FileError::InvalidPath)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).map_err(|_| FileError::InvalidPath)
}
#[cfg(test)]
mod tests;

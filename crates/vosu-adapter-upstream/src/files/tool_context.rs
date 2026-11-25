//! Pre-M7 tool-host context facts. Absolute spellings are display-only; the
//! daemon resolves file references through its retained declared-root metadata.
use super::*;
use crate::view;
use std::path::{Component, Path, PathBuf};

pub struct ToolContext {
    pub session: IdentifiedSession,
    pub root: &'static str,
    pub game: Option<PathBuf>,
    pub skin_folder: Option<PathBuf>,
    pub skin_name: String,
    pub skin_builtin: Option<String>,
    pub skin_base: Option<String>,
    pub skin_files: bool,
    pub checksum: Option<String>,
    pub mode: &'static str,
    pub file: Option<String>,
    pub audio: Option<String>,
    pub background: Option<String>,
}
#[derive(Deserialize)]
struct Mode {
    number: i64,
}
#[derive(Deserialize)]
struct Skin {
    builtin: Option<String>,
    base: Option<String>,
    files: bool,
}
#[derive(Deserialize)]
struct Stable {
    game: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextPaths {
    stable: Option<Stable>,
    skin_folder: Option<String>,
    current: Option<Current>,
}
impl Provider {
    /// Capture all context inputs at one selected session/revision cut. Reads
    /// create no demand; paths here never grant filesystem authority.
    pub fn tool_context(&self) -> Result<ToolContext, FileError> {
        let requester = self.requester.as_ref().ok_or(FileError::NotReady)?;
        let mut selectors = view::selectors(&[
            "/internal/paths",
            "/session/state",
            "/beatmap/checksum",
            "/beatmap/selectionMode",
            "/play/mode",
            "/result/mode",
            "/skin",
        ]);
        selectors.push(Selector {
            ns: "vosu.skin".into(),
            pointer: "/name".into(),
        });
        let capture = requester
            .capture(SessionTarget::Focused, &selectors)
            .map_err(|_| FileError::NotReady)?;
        let session = view::Identity::from_capture(&capture)
            .map_err(|_| FileError::NotReady)?
            .session;
        let paths: ContextPaths =
            view::decode(&capture, "/internal/paths").map_err(|_| FileError::NotReady)?;
        let skin: Skin = view::decode(&capture, "/skin").map_err(|_| FileError::NotReady)?;
        let state: Mode =
            view::decode(&capture, "/session/state").map_err(|_| FileError::NotReady)?;
        let mode = match state.number {
            2 | 7 => {
                view::decode::<Mode>(
                    &capture,
                    if state.number == 2 {
                        "/play/mode"
                    } else {
                        "/result/mode"
                    },
                )
                .map_err(|_| FileError::NotReady)?
                .number
            }
            _ => view::decode::<Option<i64>>(&capture, "/beatmap/selectionMode")
                .map_err(|_| FileError::NotReady)?
                .unwrap_or(0),
        };
        let lazer = session.variant == "lazer";
        let game = (!lazer)
            .then(|| paths.stable.and_then(|s| s.game).map(PathBuf::from))
            .flatten()
            .filter(|p| p.is_absolute());
        let skin_name = paths.skin_folder.unwrap_or_default();
        let skin_folder = game.as_ref().and_then(|game| {
            let mut parts = Path::new(&skin_name).components();
            let Component::Normal(name) = parts.next()? else {
                return None;
            };
            parts
                .next()
                .is_none()
                .then(|| game.join("Skins").join(name))
        });
        let current = paths.current.unwrap_or_default();
        let relative = |name: Option<String>| {
            name.filter(|v| !v.is_empty()).map(|name| {
                current
                    .beatmap_folder
                    .as_ref()
                    .filter(|v| !v.is_empty())
                    .map_or(name.clone(), |folder| format!("{folder}/{name}"))
            })
        };
        Ok(ToolContext {
            root: if lazer { "files" } else { "songs" },
            session,
            game,
            skin_folder,
            skin_name: view::decode_ns::<Option<String>>(&capture, "vosu.skin", "/name")
                .map_err(|_| FileError::NotReady)?
                .unwrap_or(skin_name),
            skin_builtin: skin.builtin,
            skin_base: skin.base,
            skin_files: skin.files,
            checksum: view::decode(&capture, "/beatmap/checksum")
                .map_err(|_| FileError::NotReady)?,
            mode: match mode {
                1 => "taiko",
                2 => "fruits",
                3 => "mania",
                _ => "osu",
            },
            file: relative(current.beatmap_file),
            audio: relative(current.audio),
            background: relative(current.background),
        })
    }
}

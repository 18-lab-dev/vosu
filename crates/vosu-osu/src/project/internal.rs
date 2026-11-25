//! Filesystem names are published only inside the schema's plugin-only subtree.
use super::settings::emit;
use super::{OsuPointer, ProjectionSink, SessionFacts};
use vosu_osu_model::enums::ClientType;
use vosu_osu_state::StateBundleSnapshot;
fn nonempty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}
pub(super) fn project(
    snap: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    let g = &snap.regular.global;
    let m = &snap.regular.menu;
    emit(sink, OsuPointer::OsuInternalPaths, |c| {
        c.map(4);
        c.text("stable");
        if facts.client == ClientType::Stable {
            c.map(4);
            c.text("game");
            c.optional_text(nonempty(&g.game_folder));
            c.text("songs");
            c.optional_text(nonempty(&g.songs_folder));
            for (key, suffix) in [("skins", "Skins"), ("replays", "Replays")] {
                c.text(key);
                if g.game_folder.is_empty() {
                    c.null()
                } else {
                    // Encode a joined string directly into the reusable output, without allocating a PathBuf.
                    let separator = std::path::MAIN_SEPARATOR_STR;
                    let root = g.game_folder.trim_end_matches(std::path::MAIN_SEPARATOR);
                    let len = root.len() + separator.len() + suffix.len();
                    c.text_parts(root, separator, suffix, len);
                }
            }
        } else {
            c.null()
        }
        c.text("lazer");
        if facts.client == ClientType::Lazer {
            c.map(2);
            c.text("data");
            c.optional_text(nonempty(&g.game_folder));
            c.text("files");
            c.optional_text(nonempty(&g.songs_folder));
        } else {
            c.null()
        }
        c.text("current");
        if m.folder.is_none()
            && m.filename.is_none()
            && m.background_filename.is_none()
            && m.audio_filename.is_none()
        {
            c.null()
        } else {
            c.map(4);
            for (key, value) in [
                ("beatmapFolder", m.folder.as_deref()),
                ("beatmapFile", m.filename.as_deref()),
                ("background", m.background_filename.as_deref()),
                ("audio", m.audio_filename.as_deref()),
            ] {
                c.text(key);
                c.optional_text(value)
            }
        }
        c.text("skinFolder");
        c.optional_text(if facts.client == ClientType::Stable {
            nonempty(&g.skin_folder)
        } else {
            None
        });
    });
}

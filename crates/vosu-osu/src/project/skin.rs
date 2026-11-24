//! Stable filesystem metadata is sampled once per skin-name transition.
use super::{OsuPointer as P, ProjectionSink, SessionFacts};
use vosu_osu_model::enums::ClientType;
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishValueRef as V;
#[derive(Default)]
pub(super) struct SkinCache {
    name: Option<String>,
    files: bool,
    pending_error: Option<std::io::Error>,
}
impl SkinCache {
    pub(super) fn take_error(&mut self) -> Option<std::io::Error> {
        self.pending_error.take()
    }
    pub(super) fn project(
        &mut self,
        facts: &SessionFacts,
        snap: &StateBundleSnapshot,
        sink: &mut dyn ProjectionSink,
    ) {
        let g = &snap.regular.global;
        let name = snap
            .regular
            .settings
            .values
            .get("skin")
            .and_then(|v| v.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let skin = g.current_skin.as_deref();
        let stable = facts.client == ClientType::Stable;
        if stable && self.name.as_deref() != Some(name) {
            name.clone_into(self.name.get_or_insert_with(String::new));
            self.files = false;
            if !g.skin_folder.is_empty() {
                match std::fs::metadata(&g.skin_folder) {
                    Ok(m) => self.files = m.is_dir(),
                    Err(e) => self.pending_error = Some(e),
                }
            }
        }
        sink.put(
            P::OsuSkinBuiltin,
            skin.and_then(|s| s.builtin.as_deref())
                .map_or(V::Null, V::String),
        );
        sink.put(
            P::OsuSkinBase,
            if stable {
                V::String("legacy")
            } else {
                skin.and_then(|s| s.base.as_deref())
                    .map_or(V::Null, V::String)
            },
        );
        sink.put(
            P::OsuSkinVariant,
            V::String(if stable { "stable" } else { "lazer" }),
        );
        sink.put(
            P::OsuSkinFiles,
            V::Bool(if stable {
                self.files
            } else {
                skin.is_some_and(|s| !s.files.is_empty())
            }),
        );
    }
}

//! Stable filesystem metadata is sampled only when its root/name inputs change.
use super::{OsuPointer as P, ProjectionSink, SessionFacts};
use vosu_osu_model::enums::ClientType;
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishValueRef as V;
#[derive(Default)]
pub(super) struct SkinCache {
    inputs: Option<(String, String)>,
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
        let inputs = (&g.game_folder, &g.skin_folder);
        let skin = g.current_skin.as_deref();
        let stable = facts.client == ClientType::Stable;
        if stable
            && self
                .inputs
                .as_ref()
                .is_none_or(|(root, name)| root != inputs.0 || name != inputs.1)
        {
            let cached = self
                .inputs
                .get_or_insert_with(|| (String::new(), String::new()));
            inputs.0.clone_into(&mut cached.0);
            inputs.1.clone_into(&mut cached.1);
            self.files = false;
            self.pending_error = None;
            // Match 1.x stable_skin_root and its absolute-root check
            // (src/runtime/service.rs): never interpret a name as a host path.
            let root = std::path::Path::new(inputs.0);
            let mut components = std::path::Path::new(inputs.1).components();
            if root.is_absolute()
                && let Some(std::path::Component::Normal(name)) = components.next()
                && components.next().is_none()
            {
                match std::fs::metadata(root.join("Skins").join(name)) {
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

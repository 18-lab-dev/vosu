//! Serializes the pre-M7 tool-host context contract. The adapter owns game
//! selection; core alone discloses confined retained-object display metadata.
#[cfg(test)]
#[path = "tests/context.rs"]
mod tests;
use super::*;

pub(super) fn tool_context(
    provider: &vosu_adapter_upstream::files::Provider,
    host: &PluginHost,
) -> serde_json::Value {
    let Ok(context) = provider.tool_context() else {
        return serde_json::json!({"client":null,"songs_dir":null,"osu_dir":null,"skin":null,"beatmap":null});
    };
    let selected = &context.session;
    let resolve = |path: &Option<String>| {
        path.as_deref()
            .and_then(|path| host.display_path(selected, context.root, Some(path)))
    };
    let file = resolve(&context.file);
    let beatmap = file.map(|file| {
        serde_json::json!({
            "checksum": context.checksum,
            "mode": context.mode,
            "folder": file.parent(),
            "file": file,
            "audio": resolve(&context.audio),
            "background": resolve(&context.background),
        })
    });
    let stable = selected.variant != "lazer";
    serde_json::json!({
        "client": if stable { "stable" } else { "lazer" },
        "songs_dir": host.display_path(selected, context.root, None),
        "osu_dir": context.game,
        "skin": {
            "name": context.skin_name,
            "folder": context.skin_folder,
            "files": if stable { host.display_path(selected, "skins", None).is_some() } else { context.skin_files },
            "builtin": context.skin_builtin,
            "base": context.skin_base,
        },
        "beatmap": beatmap,
    })
}

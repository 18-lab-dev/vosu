//! API v1 extension allowlist shared by publication, tree entries and job
//! artifacts.

/// A job artifact's type (AF.12): `name`'s extension against the whole
/// allowlist, with no per-kind family bound, else `application/octet-stream`.
pub fn artifact_type(name: &str) -> &'static str {
    classify(name).map_or("application/octet-stream", |(_, mime)| mime)
}

pub(crate) fn published_type(name: &str, families: &[String]) -> &'static str {
    match classify(name) {
        Some((family, mime)) if families.iter().any(|f| f == family) => mime,
        _ => "application/octet-stream",
    }
}

/// `(family, type)` of an allowlisted extension.
fn classify(name: &str) -> Option<(&'static str, &'static str)> {
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    Some(match extension.to_ascii_lowercase().as_str() {
        "png" => ("image", "image/png"),
        "jpg" | "jpeg" => ("image", "image/jpeg"),
        "webp" => ("image", "image/webp"),
        "gif" => ("image", "image/gif"),
        "mp3" => ("audio", "audio/mpeg"),
        "ogg" => ("audio", "audio/ogg"),
        "wav" => ("audio", "audio/wav"),
        "flac" => ("audio", "audio/flac"),
        "mp4" => ("video", "video/mp4"),
        "webm" => ("video", "video/webm"),
        "osu" | "osb" => ("chart-text", "text/plain; charset=utf-8"),
        _ => return None,
    })
}

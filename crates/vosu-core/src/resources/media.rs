//! API v1 extension allowlist shared by publication and tree entries.
pub(crate) fn published_type(name: &str, families: &[String]) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let (family, mime) = match extension.to_ascii_lowercase().as_str() {
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
        _ => return "application/octet-stream",
    };
    if families.iter().any(|f| f == family) {
        mime
    } else {
        "application/octet-stream"
    }
}

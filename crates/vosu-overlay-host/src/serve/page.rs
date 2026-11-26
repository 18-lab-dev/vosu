//! A full overlay HTML page: the original bytes, then the `COUNTER_PATH`
//! identity script, and for a dashboard preview the socket marker first.

use bytes::Bytes;
use vosu_surface::files::{FileError, OpenedFile};
use vosu_surface::routes::{ResponseBody, SurfaceResponse};

use super::{COUNTER_HTML_LIMIT, PREVIEW_HTML_LIMIT, internal_error, read_all};

pub(super) struct Page<'a> {
    /// The still-encoded logical page path (`counter/index.html`).
    pub encoded_html_path: &'a str,
    /// A `vosuProfile` override, already resolved to a known profile.
    pub profile: Option<String>,
    /// `?vosu-preview=1`: the dashboard asked for this page as a card preview.
    pub preview: bool,
}

impl Page<'_> {
    pub(super) async fn respond(
        &self,
        file: OpenedFile,
        content_type: &'static str,
    ) -> SurfaceResponse {
        let source = match read_all(&file, COUNTER_HTML_LIMIT).await {
            Ok(source) => source,
            Err(FileError::LimitExceeded) => return internal_error("overlay page is too large"),
            Err(_) => return internal_error("failed to read file"),
        };
        let mut page = if self.preview && file.metadata.size <= PREVIEW_HTML_LIMIT {
            with_preview_socket_marker(&source)
        } else {
            source
        };
        if let Some(script) = counter_path_script(self.encoded_html_path, self.profile.as_deref()) {
            page.extend_from_slice(script.as_bytes());
        }
        SurfaceResponse {
            status: 200,
            headers: vec![
                ("content-type".into(), content_type.to_owned()),
                ("content-length".into(), page.len().to_string()),
            ],
            body: ResponseBody::Bytes(Bytes::from(page)),
        }
    }
}

/// Browsers send no Referer on a WebSocket handshake, so a dashboard preview
/// marks its own sockets: every WebSocket the preview page opens to this
/// server carries `vosu-preview=1`, which the connected count skips. The
/// marker is appended without re-encoding the overlay's own query (`l=`),
/// and only preview pages get it; the URLs users copy are unchanged.
const PREVIEW_SOCKET_SCRIPT: &str = concat!(
    r#"<script>(()=>{const W=window.WebSocket;if(!W)return;"#,
    r#"const local=new Set([location.hostname,"localhost","127.0.0.1","[::1]"]);"#,
    r#"window.WebSocket=class extends W{constructor(url,protocols){"#,
    r#"const target=new URL(url,location.href);"#,
    r#"if(target.port===location.port&&local.has(target.hostname))"#,
    r#"target.search=(target.search?target.search+"&":"?")+"vosu-preview=1";"#,
    r#"super(target.href,protocols)}}})();</script>"#,
);

/// Inserts [`PREVIEW_SOCKET_SCRIPT`] after the `<head>` start tag, else
/// after `<html>` or the doctype, else first, so it runs before any of the
/// overlay's scripts.
pub(super) fn with_preview_socket_marker(source: &[u8]) -> Vec<u8> {
    let lower = source.to_ascii_lowercase();
    // End of the first start tag `name` (`<head`, not `<header`).
    let after_tag = |name: &[u8]| {
        let start = lower
            .windows(name.len())
            .enumerate()
            .find_map(|(index, window)| {
                (window == name
                    && lower.get(index + name.len()).is_some_and(|&next| {
                        next == b'>' || next == b'/' || next.is_ascii_whitespace()
                    }))
                .then_some(index)
            })?;
        let end = lower[start..].iter().position(|&byte| byte == b'>')?;
        Some(start + end + 1)
    };
    let at = after_tag(b"<head")
        .or_else(|| after_tag(b"<html"))
        .or_else(|| after_tag(b"<!doctype"))
        .unwrap_or(0);
    let mut page = Vec::with_capacity(source.len() + PREVIEW_SOCKET_SCRIPT.len());
    page.extend_from_slice(&source[..at]);
    page.extend_from_slice(PREVIEW_SOCKET_SCRIPT.as_bytes());
    page.extend_from_slice(&source[at..]);
    page
}

/// `window.COUNTER_PATH` is the decoded logical parent folder of the page,
/// qualified by a pinned profile (`window.VOSU_PROFILE`).
pub(super) fn counter_path_script(
    encoded_html_path: &str,
    profile: Option<&str>,
) -> Option<String> {
    let decoded_html_path = super::percent_decode_once(encoded_html_path)?;
    let logical_path = decoded_html_path.trim_start_matches('/');
    let counter_path = logical_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let mut script = String::with_capacity(counter_path.len() + 48);
    script.push_str("<script>window.COUNTER_PATH=");
    if let Some(profile) = profile {
        push_javascript_string(
            &mut script,
            &crate::qualified_counter(counter_path, profile),
        );
        script.push_str(";window.VOSU_PROFILE=");
        push_javascript_string(&mut script, profile);
    } else {
        push_javascript_string(&mut script, counter_path);
    }
    script.push_str(";</script>");
    Some(script)
}

fn push_javascript_string(output: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000C}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '<' => output.push_str("\\u003C"),
            '>' => output.push_str("\\u003E"),
            '&' => output.push_str("\\u0026"),
            '\u{2028}' => output.push_str("\\u2028"),
            '\u{2029}' => output.push_str("\\u2029"),
            '\u{0000}'..='\u{001F}' => {
                let byte = character as u8;
                output.push_str("\\u00");
                output.push(HEX[usize::from(byte >> 4)] as char);
                output.push(HEX[usize::from(byte & 0x0F)] as char);
            }
            _ => output.push(character),
        }
    }
    output.push('"');
}

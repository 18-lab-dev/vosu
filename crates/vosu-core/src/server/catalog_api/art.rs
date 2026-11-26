//! Catalog card art fetched on the dashboard's behalf.
//!
//! Catalog `icon_url`/`banner_url`/`preview` URLs often point at release
//! downloads that redirect to storage serving `application/octet-stream`,
//! which browsers refuse to render as SVG. The dashboard therefore loads card
//! art through `GET /api/catalog/art?url=<listed URL>`: vosu fetches only
//! URLs listed by the current validated catalog snapshot, exactly as
//! written there, through the public-only, credential-free artwork client
//! (HTTPS only, public destinations, bounded redirects and size, even when
//! the catalog came from a private or credentialed source), decides the
//! image type from the content and the URL's extension, and serves it with a
//! type the browser accepts. Every response, success or error, is a
//! sandboxed inert document if opened directly.
use super::{Inner, network::MAX_ART_BYTES};
use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, StatusCode, header},
    response::Response,
};
use std::{collections::VecDeque, sync::Arc};

const CACHE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 256;
/// Sent on every art response: opened directly, the document gets an opaque
/// origin and can run nothing, so an SVG can never act as a dashboard page.
pub(super) const ART_CSP: &str =
    "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArtKind {
    Svg,
    Png,
    Webp,
    Jpeg,
}

impl ArtKind {
    fn content_type(self) -> &'static str {
        match self {
            Self::Svg => "image/svg+xml",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// True when the text, after an optional byte-order mark, XML declaration,
/// comments and a doctype without an internal subset, starts with `<svg`.
fn svg_root(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    loop {
        rest = rest.trim_start();
        let skip = if rest.starts_with("<?xml") {
            rest.find("?>").map(|end| end + 2)
        } else if rest.starts_with("<!--") {
            rest.find("-->").map(|end| end + 3)
        } else if rest.len() >= 9 && rest[..9].eq_ignore_ascii_case("<!doctype") {
            // An internal subset can declare entities; such files are refused.
            rest.find('>')
                .filter(|end| !rest[..*end].contains('['))
                .map(|end| end + 1)
        } else {
            break;
        };
        let Some(skip) = skip else {
            return false;
        };
        rest = &rest[skip..];
    }
    rest.strip_prefix("<svg")
        .and_then(|after| after.chars().next())
        .is_some_and(|next| next.is_ascii_whitespace() || next == '>' || next == '/')
}

pub(super) fn sniff(body: &[u8]) -> Option<ArtKind> {
    if body.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ArtKind::Png)
    } else if body.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ArtKind::Jpeg)
    } else if body.len() >= 12 && &body[..4] == b"RIFF" && &body[8..12] == b"WEBP" {
        Some(ArtKind::Webp)
    } else if svg_root(body) {
        Some(ArtKind::Svg)
    } else {
        None
    }
}

/// The type is the content's; a file extension on the listed URL, when
/// present, must name the same type. Server-sent types are ignored.
pub(super) fn classify(url: &str, body: &[u8]) -> Result<ArtKind, String> {
    let kind = sniff(body).ok_or("art is not an SVG, PNG, WebP or JPEG image")?;
    let path = url::Url::parse(url).map_err(|e| e.to_string())?;
    let name = path
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or_default();
    let Some((_, extension)) = name.rsplit_once('.') else {
        return Ok(kind);
    };
    let named = match extension.to_ascii_lowercase().as_str() {
        "svg" => ArtKind::Svg,
        "png" => ArtKind::Png,
        "webp" => ArtKind::Webp,
        "jpg" | "jpeg" => ArtKind::Jpeg,
        other => return Err(format!("unsupported art extension .{other}")),
    };
    if named != kind {
        return Err(format!(
            "art content does not match its .{extension} extension"
        ));
    }
    Ok(kind)
}

/// Fetched art for one catalog snapshot epoch, least recently used first. A
/// new publication drops everything: a release can serve new bytes at the
/// same URL.
#[derive(Default)]
pub(super) struct ArtCache {
    revision: u64,
    entries: VecDeque<(String, ArtKind, Bytes)>,
    bytes: usize,
}

impl ArtCache {
    pub(super) fn get(&mut self, revision: u64, url: &str) -> Option<(ArtKind, Bytes)> {
        if self.revision != revision {
            self.entries.clear();
            self.bytes = 0;
            self.revision = revision;
            return None;
        }
        let index = self.entries.iter().position(|(key, ..)| key == url)?;
        let entry = self.entries.remove(index)?;
        let found = (entry.1, entry.2.clone());
        self.entries.push_back(entry);
        Some(found)
    }
    pub(super) fn insert(&mut self, revision: u64, url: String, kind: ArtKind, body: Bytes) {
        if self.revision != revision {
            return;
        }
        if let Some(index) = self.entries.iter().position(|(key, ..)| *key == url)
            && let Some((.., old)) = self.entries.remove(index)
        {
            self.bytes -= old.len();
        }
        self.bytes += body.len();
        self.entries.push_back((url, kind, body));
        while self.bytes > CACHE_BYTES || self.entries.len() > CACHE_ENTRIES {
            let Some((.., old)) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.len();
        }
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn art_response(status: StatusCode, content_type: &'static str, body: Bytes) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::CONTENT_SECURITY_POLICY, ART_CSP)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::HeaderName::from_static("cross-origin-resource-policy"),
            "same-origin",
        )
        .header(header::CONTENT_DISPOSITION, "inline")
        .header(header::REFERRER_POLICY, "no-referrer")
        // The URL stays the same across catalog releases; the browser asks
        // again and vosu answers from its own epoch-keyed cache.
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        )
        .body(Body::from(body))
        .expect("valid art response")
}

fn art_error(status: StatusCode, message: impl Into<String>) -> Response {
    art_response(
        status,
        "text/plain; charset=utf-8",
        Bytes::from(message.into()),
    )
}

/// The current snapshot epoch when `url` is listed by one of its entries.
fn listed(inner: &Inner, url: &str) -> Option<u64> {
    let snapshot = inner
        .runtime()
        .fetch()?
        .sources()
        .candidate_snapshot()
        .ok()??;
    let wanted = Some(url);
    snapshot
        .candidates()
        .any(|candidate| {
            [
                &candidate.entry.icon_url,
                &candidate.entry.banner_url,
                &candidate.entry.preview,
            ]
            .iter()
            .any(|value| value.as_deref() == wanted)
        })
        .then(|| inner.epoch(&snapshot))
}

/// `GET /api/catalog/art?url=<URL>`: URLs not listed by the current snapshot
/// are 404; fetch, size and type failures are 502.
pub(super) async fn serve(inner: &Arc<Inner>, query: Option<&str>) -> Response {
    let Some(url) = query.and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "url")
            .map(|(_, value)| value.into_owned())
    }) else {
        return art_error(StatusCode::NOT_FOUND, "Not Found");
    };
    let Some(revision) = listed(inner, &url) else {
        return art_error(StatusCode::NOT_FOUND, "Not Found");
    };
    if let Some((kind, body)) = inner.art.lock().get(revision, &url) {
        return art_response(StatusCode::OK, kind.content_type(), body);
    }
    let fetched = async {
        let (status, body) = inner
            .services
            .network
            .art(&url, &inner.services.cancel)
            .await?;
        if !(200..300).contains(&status) {
            return Err(format!("art request returned HTTP {status}"));
        }
        if body.len() as u64 > MAX_ART_BYTES {
            return Err(format!("response exceeds {MAX_ART_BYTES} bytes"));
        }
        let kind = classify(&url, &body)?;
        Ok::<_, String>((kind, body))
    }
    .await;
    match fetched {
        Ok((kind, body)) => {
            inner.art.lock().insert(revision, url, kind, body.clone());
            art_response(StatusCode::OK, kind.content_type(), body)
        }
        Err(message) => art_error(StatusCode::BAD_GATEWAY, message),
    }
}

#[cfg(test)]
#[path = "art_tests.rs"]
mod tests;

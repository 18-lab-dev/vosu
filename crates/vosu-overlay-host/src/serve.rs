//! Overlay static serving (A7.1, ADR 0005): `GET`/`HEAD /<folder>/…` on the
//! overlay listener, alias redirects and `COUNTER_PATH` injection. Every
//! byte is read through the core-minted, read-only [`RootLease`] of the
//! current [`OverlayRootView`]; this host never opens a path itself. A
//! request keeps the view it started with, so a root change never mixes two
//! roots in one response.

mod http;
mod page;

use std::sync::Arc;

use arc_swap::ArcSwapOption;
use bytes::Bytes;
use vosu_surface::files::{FileError, OpenedFile, RelativeChild, RootLease};
use vosu_surface::routes::{
    ProviderResponse, ResponseBody, ResponseFuture, RouteProvider, SurfaceRequest, SurfaceResponse,
};

use crate::ReloadableCounterService;
use http::ByteRange;

/// The overlay root as core pushes it: latest-only, one generation per
/// effective `STATIC_FOLDER_PATH` value. It carries no path.
#[derive(Clone)]
pub struct OverlayRootView {
    pub generation: u64,
    pub root: RootLease,
}

/// Larger preview pages are served unmarked rather than buffered.
const PREVIEW_HTML_LIMIT: u64 = 2 * 1024 * 1024;
/// A full HTML page is buffered to append `COUNTER_PATH`; a larger page is
/// refused rather than served without the identity its scripts read.
const COUNTER_HTML_LIMIT: u64 = 64 * 1024 * 1024;

pub struct OverlayStatic {
    counters: Arc<ReloadableCounterService>,
    view: ArcSwapOption<OverlayRootView>,
}

impl OverlayStatic {
    pub fn new(counters: Arc<ReloadableCounterService>) -> Self {
        Self {
            counters,
            view: ArcSwapOption::empty(),
        }
    }

    /// Installs `view` unless a newer generation is already installed.
    pub fn set_view(&self, view: OverlayRootView) {
        let view = Arc::new(view);
        self.view.rcu(|current| match current {
            Some(current) if current.generation > view.generation => Some(Arc::clone(current)),
            _ => Some(Arc::clone(&view)),
        });
    }

    /// No root is configured or the lease could not be minted: nothing is served.
    pub fn clear_view(&self) {
        self.view.store(None);
    }

    pub fn generation(&self) -> Option<u64> {
        self.view.load().as_ref().map(|view| view.generation)
    }

    pub async fn serve(&self, request: &SurfaceRequest) -> SurfaceResponse {
        if !matches!(request.method.as_str(), "GET" | "HEAD") {
            return not_found();
        }
        let Some(view) = self.view.load_full() else {
            return not_found();
        };
        if let Some(redirect) = self.alias_redirect(&view.root, request).await {
            return redirect;
        }
        let path = request.path.as_str();
        let relative = path.trim_start_matches('/');
        let encoded = if path.ends_with('/') {
            format!("{relative}index.html")
        } else {
            relative.to_owned()
        };
        let Some((decoded, child)) = decode_child(&encoded) else {
            return not_found();
        };
        let file = match view.root.open(&child).await {
            Ok(file) => file,
            Err(FileError::InvalidPath)
                if !path.ends_with('/') && is_directory(&view.root, &child).await =>
            {
                return redirect(301, format!("{path}/{}", query_suffix(&request.query)));
            }
            Err(_) => return not_found(),
        };
        let range = header(request, "range");
        let html_page = page::Page {
            encoded_html_path: &encoded,
            profile: self.profile(request),
            preview: query_value(&request.query, "vosu-preview") == Some("1"),
        };
        respond_file(file, &decoded, range, &html_page).await
    }

    /// `vosuProfile` pins `COUNTER_PATH` to a profile: the requested one, or
    /// the active one when it is unknown.
    fn profile(&self, request: &SurfaceRequest) -> Option<String> {
        let requested = query_value(&request.query, "vosuProfile")?;
        let store = self.counters.settings_store();
        store
            .resolve_profile(Some(requested))
            .or_else(|_| store.resolve_profile(None))
            .ok()
    }

    /// An overlay URL alias (`/acc/…`) redirects to its folder
    /// (`/accuracy%20by%20sxlvia/…`) so relative assets, `COUNTER_PATH` and
    /// settings identity stay on the real folder. A real entry of the same
    /// name always wins. 302, because the alias can change later.
    async fn alias_redirect(
        &self,
        root: &RootLease,
        request: &SurfaceRequest,
    ) -> Option<SurfaceResponse> {
        let path = request.path.strip_prefix('/')?;
        let (alias, rest) = path.split_once('/').unwrap_or((path, ""));
        if alias.is_empty() {
            return None;
        }
        if let Ok(child) = RelativeChild::parse(alias)
            && (root.open(&child).await.is_ok() || is_directory(root, &child).await)
        {
            return None;
        }
        let folder = self.counters.names().folder_for_alias(alias)?;
        let mut location = crate::cards::overlay_path(&folder);
        location.push_str(rest);
        location.push_str(&query_suffix(&request.query));
        let mut response = redirect(302, location);
        response
            .headers
            .push(("cache-control".into(), "no-store".into()));
        Some(response)
    }
}

impl RouteProvider for OverlayStatic {
    fn respond(&self, request: SurfaceRequest) -> ResponseFuture<'_, ProviderResponse> {
        Box::pin(async move { ProviderResponse::Http(self.serve(&request).await) })
    }
}

/// Decodes the request path exactly once: malformed or double encoding never
/// broadens a match (`%252e%252e` stays the literal name `%2e%2e`), and the
/// reserved `tools` prefix never reaches the root.
fn decode_child(encoded: &str) -> Option<(String, RelativeChild)> {
    let decoded = percent_decode_once(encoded)?;
    let first = decoded.split(['/', '\\']).next().unwrap_or_default();
    if first.eq_ignore_ascii_case("tools") {
        return None;
    }
    let child = RelativeChild::parse(&decoded).ok()?;
    Some((decoded, child))
}

fn percent_decode_once(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = (*bytes.get(index + 1)? as char).to_digit(16)?;
            let low = (*bytes.get(index + 2)? as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.contains('\0')).then_some(decoded)
}

async fn is_directory(root: &RootLease, child: &RelativeChild) -> bool {
    root.directory(child).await.is_ok()
}

async fn respond_file(
    file: OpenedFile,
    decoded: &str,
    range: Option<&str>,
    html_page: &page::Page<'_>,
) -> SurfaceResponse {
    let length = file.metadata.size;
    let content_type = vosu_surface::files::content_type(std::path::Path::new(decoded));
    let ranged = http::media_range_eligible(decoded);
    let Ok(range) = http::parse_range(range, length) else {
        return SurfaceResponse {
            status: 416,
            headers: vec![("content-range".into(), format!("bytes */{length}"))],
            body: ResponseBody::Empty,
        };
    };
    if range == ByteRange::Full && content_type == "text/html; charset=utf-8" {
        return html_page.respond(file, content_type).await;
    }
    let (status, offset, count, content_range) = match range {
        ByteRange::Full => (200, 0, length, None),
        ByteRange::Partial {
            start,
            end_inclusive,
        } => (
            206,
            start,
            end_inclusive - start + 1,
            Some(format!("bytes {start}-{end_inclusive}/{length}")),
        ),
    };
    let mut headers = vec![
        ("content-type".into(), content_type.to_owned()),
        ("content-length".into(), count.to_string()),
    ];
    if ranged || status == 206 {
        headers.push(("accept-ranges".into(), "bytes".into()));
    }
    if let Some(content_range) = content_range {
        headers.push(("content-range".into(), content_range));
    }
    SurfaceResponse {
        status,
        headers,
        body: ResponseBody::File {
            file,
            offset,
            length: count,
        },
    }
}

/// Reads a whole opened file through the lease, bounded by `limit`.
async fn read_all(file: &OpenedFile, limit: u64) -> Result<Vec<u8>, FileError> {
    let size = file.metadata.size;
    if size > limit {
        return Err(FileError::LimitExceeded);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    while (bytes.len() as u64) < size {
        let len =
            (size - bytes.len() as u64).min(u64::from(vosu_surface::resources::MAX_READ_BYTES));
        let chunk = file.read(bytes.len() as u64, len as u32).await?;
        if chunk.is_empty() || chunk.len() as u64 > len {
            return Err(FileError::Io);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn header<'a>(request: &'a SurfaceRequest, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find_map(|(key, value)| (key == name).then_some(value.as_str()))
}

fn query_value<'a>(query: &'a [(String, String)], key: &str) -> Option<&'a str> {
    query
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}

/// The admitted query, re-encoded once, so a redirect keeps it.
fn query_suffix(query: &[(String, String)]) -> String {
    if query.is_empty() {
        return String::new();
    }
    let mut encoded = url::form_urlencoded::Serializer::for_suffix(String::from("?"), 1);
    encoded.extend_pairs(query);
    encoded.finish()
}

fn redirect(status: u16, location: String) -> SurfaceResponse {
    SurfaceResponse {
        status,
        headers: vec![("location".into(), location)],
        body: ResponseBody::Empty,
    }
}

fn not_found() -> SurfaceResponse {
    SurfaceResponse {
        status: 404,
        headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
        body: ResponseBody::Bytes(Bytes::from_static(b"Not Found")),
    }
}

fn internal_error(message: &'static str) -> SurfaceResponse {
    SurfaceResponse {
        status: 500,
        headers: vec![(
            "content-type".into(),
            "application/json; charset=utf-8".into(),
        )],
        body: ResponseBody::Bytes(Bytes::from(
            serde_json::json!({ "error": message }).to_string(),
        )),
    }
}

#[cfg(test)]
mod tests;

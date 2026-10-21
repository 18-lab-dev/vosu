//! Embedded dashboard/page primitives shared by the HTTP router.
//!
//! This module deliberately does not know about counters, settings persistence,
//! reports, or updates.  It classifies the retained routes and returns either
//! immutable embedded bytes or a typed delegation request for those services.

use std::borrow::Cow;
use std::net::{IpAddr, SocketAddr};

use bytes::Bytes;
use url::Url;

use crate::assets::ServerAssets;

pub const PAGE_NOT_FOUND: &[u8] = b"<html>page not found</html>";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheMetadata {
    /// Compatibility behavior: do not emit a `Cache-Control` header.
    Unspecified,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Bytes,
    pub cache: CacheMetadata,
}

impl EmbeddedResponse {
    fn ok(content_type: &'static str, body: impl Into<Bytes>) -> Self {
        Self {
            status: 200,
            content_type,
            body: body.into(),
            cache: CacheMetadata::Unspecified,
        }
    }

    pub fn not_found() -> Self {
        Self {
            status: 404,
            content_type: "text/html",
            body: Bytes::from_static(PAGE_NOT_FOUND),
            cache: CacheMetadata::Unspecified,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DynamicPage {
    Dashboard,
    Settings,
    EmptySettings,
    Available,
    Tools,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StaticFallback {
    /// The source redirects extensionless (and one-character-extension) paths.
    RedirectTrailingSlash { location: String },
    /// A counter/static service should serve this relative path.
    CounterPath {
        request_path: String,
        index_path: String,
    },
}

impl StaticFallback {
    pub fn status(&self) -> u16 {
        match self {
            Self::RedirectTrailingSlash { .. } => 301,
            Self::CounterPath { .. } => 200,
        }
    }

    pub fn content_type(&self) -> Option<&'static str> {
        match self {
            Self::RedirectTrailingSlash { .. } => None,
            Self::CounterPath { index_path, .. } => Some(content_type(index_path)),
        }
    }

    pub fn cache(&self) -> CacheMetadata {
        CacheMetadata::Unspecified
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PageLookup {
    Embedded(EmbeddedResponse),
    Dynamic(DynamicPage),
    Static(StaticFallback),
    NotFound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelegatedApi {
    SettingsSave,
    RunUpdates,
    GenerateReport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HttpMethod {
    Get,
    Post,
    Other,
}

/// Effective listener identity after successful bind/rebind.
///
/// `host` is the original configured hostname (rather than a resolver-produced
/// IP); `bound` supplies the effective port and an IP fallback for an empty host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveListener {
    pub host: String,
    pub bound: SocketAddr,
}

impl EffectiveListener {
    pub fn new(host: impl Into<String>, bound: SocketAddr) -> Self {
        Self {
            host: host.into(),
            bound,
        }
    }

    pub fn dashboard_url(&self) -> Url {
        Url::parse(&format!("http://{}/", self.authority()))
            .expect("listener authority always forms a valid HTTP URL")
    }

    pub fn available_url(&self) -> Url {
        let mut url = self.dashboard_url();
        url.set_path("/available");
        url
    }

    pub fn authority(&self) -> String {
        let host = self.host.trim();
        let host = if host.is_empty() {
            Cow::Owned(self.bound.ip().to_string())
        } else {
            Cow::Borrowed(host)
        };
        let unbracketed = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(&host);
        let parsed_ip = unbracketed.parse::<IpAddr>().ok();
        let display = if parsed_ip.is_some_and(|ip| ip.is_unspecified()) {
            Cow::Borrowed("localhost")
        } else if parsed_ip.is_some_and(|ip| ip.is_ipv6()) {
            Cow::Owned(format!("[{unbracketed}]"))
        } else {
            Cow::Borrowed(unbracketed)
        };
        format!("{display}:{}", self.bound.port())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PageService;

impl PageService {
    pub fn dashboard_template(&self) -> EmbeddedResponse {
        embedded_named("homepage.html").unwrap_or_else(EmbeddedResponse::not_found)
    }

    pub fn asset(&self, requested: &str) -> EmbeddedResponse {
        let Some(name) = safe_asset_name(requested) else {
            return EmbeddedResponse::not_found();
        };
        embedded_named(name).unwrap_or_else(EmbeddedResponse::not_found)
    }

    pub fn favicon(&self) -> EmbeddedResponse {
        embedded_named("favicon.ico").unwrap_or_else(EmbeddedResponse::not_found)
    }

    /// Classify retained GET pages.
    pub fn lookup_get(&self, path: &str, settings_overlay_query_present: bool) -> PageLookup {
        if path == "/favicon.ico" {
            return PageLookup::Embedded(self.favicon());
        }
        if let Some(name) = path.strip_prefix("/assets/") {
            return PageLookup::Embedded(self.asset(name));
        }
        match path {
            "/" => PageLookup::Dynamic(DynamicPage::Dashboard),
            "/settings" if settings_overlay_query_present => {
                PageLookup::Dynamic(DynamicPage::EmptySettings)
            }
            "/settings" => PageLookup::Dynamic(DynamicPage::Settings),
            "/available" => PageLookup::Dynamic(DynamicPage::Available),
            "/tools" => PageLookup::Dynamic(DynamicPage::Tools),
            path if path.starts_with("/.well-know") => PageLookup::NotFound,
            path => PageLookup::Static(static_fallback(path)),
        }
    }

    pub fn delegated_api(&self, method: HttpMethod, path: &str) -> Option<DelegatedApi> {
        match (method, path) {
            (HttpMethod::Post, "/api/settingsSave") => Some(DelegatedApi::SettingsSave),
            (HttpMethod::Post, "/api/runUpdates") => Some(DelegatedApi::RunUpdates),
            (HttpMethod::Post, "/api/generateReport") => Some(DelegatedApi::GenerateReport),
            _ => None,
        }
    }
}

pub fn static_fallback(path: &str) -> StaticFallback {
    let last = path.rsplit('/').next().unwrap_or_default();
    let extension_len = last
        .rfind('.')
        .filter(|dot| *dot > 0)
        .map_or(0, |dot| last.len() - dot);
    if extension_len < 3 && !path.ends_with('/') {
        return StaticFallback::RedirectTrailingSlash {
            // Prevent untrusted paths from creating network-path Location values.
            location: format!(
                "/{}/",
                path.trim_start_matches(['/', '\\']).trim_end_matches('/')
            ),
        };
    }
    let index_path = if path.ends_with('/') {
        format!("{path}index.html")
    } else {
        path.to_owned()
    };
    StaticFallback::CounterPath {
        request_path: path.to_owned(),
        index_path,
    }
}

pub fn content_type(path: &str) -> &'static str {
    let filename = path.rsplit('/').next().unwrap_or_default();
    let extension = filename
        .rfind('.')
        .filter(|dot| *dot > 0)
        .map(|dot| &filename[dot + 1..]);
    match extension {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png; charset=utf-8",
        Some("jpg") => "image/jpg; charset=utf-8",
        Some("jpeg") => "image/jpeg; charset=utf-8",
        Some("gif") => "image/gif; charset=utf-8",
        Some("svg") => "image/svg+xml; charset=utf-8",
        Some("wav") => "audio/wav; charset=utf-8",
        Some("mp4") => "video/mp4; charset=utf-8",
        Some("woff") => "application/font-woff; charset=utf-8",
        Some("ttf") => "application/font-ttf; charset=utf-8",
        Some("eot") => "application/vnd.ms-fontobject; charset=utf-8",
        Some("otf") => "application/font-otf; charset=utf-8",
        Some("wasm") => "application/wasm; charset=utf-8",
        Some("aac") => "audio/aac; charset=utf-8",
        Some("abw") => "application/x-abiword; charset=utf-8",
        Some("apng") => "image/apng; charset=utf-8",
        Some("arc") => "application/x-freearc; charset=utf-8",
        Some("avif") => "image/avif; charset=utf-8",
        Some("avi") => "video/x-msvideo; charset=utf-8",
        Some("bin") => "application/octet-stream; charset=utf-8",
        Some("bmp") => "image/bmp; charset=utf-8",
        Some("bz") => "application/x-bzip; charset=utf-8",
        Some("bz2") => "application/x-bzip2; charset=utf-8",
        Some("cda") => "application/x-cdf; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("gz") => "application/gzip; charset=utf-8",
        Some("ico") => "image/vnd.microsoft.icon; charset=utf-8",
        Some("jsonld") => "application/ld+json; charset=utf-8",
        Some("mid" | "midi") => "audio/x-midi; charset=utf-8",
        Some("mp3") => "audio/mpeg; charset=utf-8",
        Some("mpeg") => "video/mpeg; charset=utf-8",
        Some("mpkg") => "application/vnd.apple.installer+xml; charset=utf-8",
        Some("ogg" | "oga") => "audio/ogg; charset=utf-8",
        Some("ogv") => "video/ogg; charset=utf-8",
        Some("ogx") => "application/ogg; charset=utf-8",
        Some("opus") => "audio/opus; charset=utf-8",
        Some("rar") => "application/vnd.rar; charset=utf-8",
        Some("sh") => "application/x-sh; charset=utf-8",
        Some("tar") => "application/x-tar; charset=utf-8",
        Some("tif" | "tiff") => "image/tiff; charset=utf-8",
        Some("ts") => "video/mp2t; charset=utf-8",
        Some("txt" | "osu") => "text/plain; charset=utf-8",
        Some("weba") => "audio/webm; charset=utf-8",
        Some("webm") => "video/webm; charset=utf-8",
        Some("webp") => "image/webp; charset=utf-8",
        Some("woff2") => "font/woff2; charset=utf-8",
        Some("xhtml") => "application/xhtml+xml; charset=utf-8",
        Some("xml") => "application/xmlz; charset=utf-8",
        Some("xul") => "application/vnd.mozilla.xul+xml; charset=utf-8",
        Some("zip") => "application/zip; charset=utf-8",
        Some("3gp") => "video/3gpp;; charset=utf-8",
        Some("3g2") => "video/3gpp2;; charset=utf-8",
        Some("7z") => "application/x-7z-compressed; charset=utf-8",
        Some("swf") => "application/x-shockwave-flash; charset=utf-8",
        _ => "",
    }
}

fn safe_asset_name(requested: &str) -> Option<&str> {
    if requested.is_empty()
        || requested.starts_with('/')
        || requested.starts_with('\\')
        || requested
            .split(['/', '\\'])
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(requested)
}

fn embedded_named(name: &str) -> Option<EmbeddedResponse> {
    let asset = ServerAssets::get(name)?;
    Some(EmbeddedResponse::ok(
        content_type(name),
        asset.data.into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_assets_missing_and_special_routes_are_classified() {
        let pages = PageService;
        assert_eq!(
            pages.dashboard_template().body.as_ref(),
            include_bytes!("../../assets/server/homepage.html")
        );
        assert_eq!(
            pages.favicon().content_type,
            "image/vnd.microsoft.icon; charset=utf-8"
        );
        assert_eq!(pages.asset("missing.js").status, 404);
        assert_eq!(pages.asset("../homepage.html").status, 404);
        assert_eq!(pages.asset("missing.js").content_type, "text/html");
        assert_eq!(
            pages.lookup_get("/.well-known/x", false),
            PageLookup::NotFound
        );
        assert_eq!(
            pages.lookup_get("/settings", true),
            PageLookup::Dynamic(DynamicPage::EmptySettings)
        );
        assert_eq!(
            pages.lookup_get("/local-overlays", false),
            PageLookup::Static(static_fallback("/local-overlays")),
            "the removed local-overlays guide must fall through to counter paths"
        );
    }

    #[test]
    fn static_fallback_matches_source_trailing_slash_rule() {
        assert_eq!(
            static_fallback("/counter"),
            StaticFallback::RedirectTrailingSlash {
                location: "/counter/".into()
            }
        );
        assert_eq!(
            static_fallback("/x.a"),
            StaticFallback::RedirectTrailingSlash {
                location: "/x.a/".into()
            }
        );
        assert_eq!(
            static_fallback("/.hidden"),
            StaticFallback::RedirectTrailingSlash {
                location: "/.hidden/".into()
            }
        );
        for hostile in ["//evil.example/counter", "/\\\\evil.example/counter"] {
            let StaticFallback::RedirectTrailingSlash { location } = static_fallback(hostile)
            else {
                panic!("expected trailing-slash redirect");
            };
            assert!(location.starts_with('/') && !location.starts_with("//"));
            assert!(!location.contains("\\"));
        }
        assert_eq!(content_type(".html"), "");
        assert_eq!(
            static_fallback("/counter/"),
            StaticFallback::CounterPath {
                request_path: "/counter/".into(),
                index_path: "/counter/index.html".into()
            }
        );
        assert_eq!(
            static_fallback("/x.js"),
            StaticFallback::CounterPath {
                request_path: "/x.js".into(),
                index_path: "/x.js".into()
            }
        );
        let file = static_fallback("/x.js");
        assert_eq!(file.status(), 200);
        assert_eq!(file.content_type(), Some("text/javascript; charset=utf-8"));
        assert_eq!(file.cache(), CacheMetadata::Unspecified);
        let redirect = static_fallback("/counter");
        assert_eq!(redirect.status(), 301);
        assert_eq!(redirect.content_type(), None);
    }

    #[test]
    fn delegated_apis_require_exact_methods() {
        let pages = PageService;
        assert_eq!(
            pages.delegated_api(HttpMethod::Post, "/api/settingsSave"),
            Some(DelegatedApi::SettingsSave)
        );
        assert_eq!(
            pages.delegated_api(HttpMethod::Post, "/api/runUpdates"),
            Some(DelegatedApi::RunUpdates)
        );
        assert_eq!(
            pages.delegated_api(HttpMethod::Post, "/api/generateReport"),
            Some(DelegatedApi::GenerateReport)
        );
        assert_eq!(
            pages.delegated_api(HttpMethod::Get, "/api/settingsSave"),
            None
        );
    }

    #[test]
    fn effective_listener_urls_preserve_hostname_and_format_ipv6() {
        let hostname = EffectiveListener::new("localhost", "127.0.0.1:3210".parse().unwrap());
        assert_eq!(hostname.dashboard_url().as_str(), "http://localhost:3210/");
        assert_eq!(
            hostname.available_url().as_str(),
            "http://localhost:3210/available"
        );

        let ipv6 = EffectiveListener::new("::1", "[::1]:24050".parse().unwrap());
        assert_eq!(ipv6.authority(), "[::1]:24050");
        assert_eq!(ipv6.dashboard_url().as_str(), "http://[::1]:24050/");

        let fallback = EffectiveListener::new("", "[2001:db8::1]:99".parse().unwrap());
        assert_eq!(
            fallback.dashboard_url().as_str(),
            "http://[2001:db8::1]:99/"
        );

        let wildcard_v4 = EffectiveListener::new("0.0.0.0", "0.0.0.0:24050".parse().unwrap());
        assert_eq!(
            wildcard_v4.dashboard_url().as_str(),
            "http://localhost:24050/"
        );
        let wildcard_v6 = EffectiveListener::new("::", "[::]:24051".parse().unwrap());
        assert_eq!(
            wildcard_v6.dashboard_url().as_str(),
            "http://localhost:24051/"
        );
    }
}

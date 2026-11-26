//! Embedded page primitives shared by the HTTP router.
//!
//! The dashboard's page shells and assets belong to `vosu-dashboard` and the
//! design-system set to `vosu-tool-host`; this module keeps the styled 404
//! page, the overlay preview marker and listener URL formatting.

use std::borrow::Cow;
use std::net::{IpAddr, SocketAddr};

use bytes::Bytes;
use url::Url;

/// The 404 page a person sees, in the dashboard's design: both listeners
/// serve `/assets/`, and `/` on the overlay listener redirects to the
/// dashboard. The stored colour mode applies before first paint.
pub const PAGE_NOT_FOUND: &[u8] = concat!(
    r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="color-scheme" content="dark light"><title>Not found · vosu</title>"#,
    r#"<script src="/assets/theme-init.js"></script><link rel="stylesheet" href="/assets/tokens.css"><link rel="stylesheet" href="/assets/system.css">"#,
    r#"<style>body{display:grid;gap:var(--space-3);place-content:center;justify-items:start;min-height:100vh;margin:0;padding:var(--space-6);color:var(--fg);font:400 .875rem/1.5 var(--font-body);background:var(--bg)}h1{margin:0;font-size:1.375rem;font-weight:600}p{margin:0;color:var(--fg-dim)}</style></head>"#,
    r#"<body><h1>Page not found</h1><p>Nothing is served at this address.</p><a class="v-btn v-btn--primary" href="/">Open the vosu dashboard</a></body></html>"#,
)
.as_bytes();

/// A page a person opened: a GET that accepts HTML. Such a request gets
/// [`PAGE_NOT_FOUND`] for a 404; scripts and API clients keep the plain
/// `Not Found` body.
pub fn wants_page(method: &axum::http::Method, headers: &axum::http::HeaderMap) -> bool {
    method == axum::http::Method::GET
        && headers
            .get(axum::http::header::ACCEPT)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|accept| accept.contains("text/html"))
}

/// Query the dashboard adds to its overlay preview frames. Sockets opened by
/// such a page are not counted as connected browser sources.
pub const OVERLAY_PREVIEW_QUERY: &str = "vosu-preview=1";

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
    pub fn not_found() -> Self {
        Self {
            status: 404,
            content_type: "text/html",
            body: Bytes::from_static(PAGE_NOT_FOUND),
            cache: CacheMetadata::Unspecified,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

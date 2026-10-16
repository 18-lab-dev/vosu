//! Request authorization shared by HTTP and WebSocket upgrade handlers.
//!
//! Read access retains the overlay-compatible peer/hostname policy. Mutations
//! additionally require exact same-origin browser provenance, never just loopback.

use axum::http::{HeaderMap, header};
use std::net::{IpAddr, Ipv4Addr};
use url::Url;

/// Captured once at HTTP dispatch and retained for the lifetime of a socket.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MutationAuthorization(pub bool);

impl MutationAuthorization {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        Self(same_origin(headers))
    }
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = single_header(headers, header::HOST) else {
        return false;
    };
    // The native listener is HTTP. Do not trust forwarded headers or URL parser
    // recovery of malformed authorities (credentials, whitespace, paths).
    if host.is_empty()
        || host
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b"/\\@?#".contains(&b))
    {
        return false;
    }
    let Ok(target) = Url::parse(&format!("http://{host}")) else {
        return false;
    };
    let is_origin = headers.contains_key(header::ORIGIN);
    let Some(value) = single_header(
        headers,
        if is_origin {
            header::ORIGIN
        } else {
            header::REFERER
        },
    ) else {
        return false;
    };
    let Ok(source) = Url::parse(value) else {
        return false;
    };
    if source.scheme() != "http"
        || !value.starts_with("http://")
        || !source.username().is_empty()
        || source.password().is_some()
        || source.fragment().is_some()
        || value.bytes().any(|b| b.is_ascii_whitespace() || b == b'\\')
    {
        return false;
    }
    // Origin is a serialized origin, not an arbitrary URL. Referer may include
    // a path/query, but must still have the exact same scheme, host and port.
    (!is_origin || source.origin().ascii_serialization() == value)
        && source.origin() == target.origin()
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationPolicy {
    allowed: Vec<String>,
    server_host: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RequestHeaders<'a> {
    pub origin: Option<&'a str>,
    pub referer: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorizationDecision {
    pub peer_ok: bool,
    pub header_ok: bool,
}

impl AuthorizationDecision {
    /// The one authorization predicate used by both HTTP and WebSocket handlers.
    pub const fn allowed(self) -> bool {
        self.peer_ok && self.header_ok
    }
}

impl AuthorizationPolicy {
    pub fn new(allowed_ips: &str, server_host: impl Into<String>) -> Self {
        Self {
            allowed: allowed_ips
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(str::to_owned)
                .collect(),
            server_host: server_host.into().trim().to_owned(),
        }
    }

    pub fn authorize(&self, peer: IpAddr, headers: RequestHeaders<'_>) -> AuthorizationDecision {
        let peer_ok = self.peer_allowed(peer);
        // Origin is authoritative whenever the header exists, including an empty or
        // malformed value. Referer is considered only when Origin is absent.
        let selected = headers.origin.or(headers.referer);
        let header_ok = selected.is_none_or(|value| {
            // Opaque/file browser origins can consume overlay data, never mutate.
            value == "null" || self.header_allowed(value)
        });

        AuthorizationDecision { peer_ok, header_ok }
    }

    pub fn is_allowed(&self, peer: IpAddr, headers: RequestHeaders<'_>) -> bool {
        self.authorize(peer, headers).allowed()
    }

    pub fn peer_allowed(&self, peer: IpAddr) -> bool {
        let peer = normalize_ip(peer);
        if peer.is_loopback() {
            return true;
        }

        self.allowed.iter().any(|token| {
            if let Some(pattern) = Ipv4Pattern::parse(token) {
                return matches!(peer, IpAddr::V4(address) if pattern.matches(address));
            }

            token
                .parse::<IpAddr>()
                .is_ok_and(|allowed| normalize_ip(allowed) == peer)
        })
    }

    pub fn header_allowed(&self, value: &str) -> bool {
        match Url::parse(value) {
            Ok(url) => url
                .host_str()
                .is_some_and(|hostname| self.hostname_allowed(hostname)),
            Err(_) => self.raw_header_allowed(value),
        }
    }

    fn hostname_allowed(&self, hostname: &str) -> bool {
        // `url` may retain brackets around IPv6 hosts. Normalize mapped IPv6
        // before comparing it with either exact IPv4 tokens or IPv4 wildcards.
        let hostname = hostname
            .trim()
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(hostname.trim());
        let parsed_host = hostname.parse::<IpAddr>().ok().map(normalize_ip);
        self.header_tokens().any(|token| {
            if let Some(pattern) = Ipv4Pattern::parse(token) {
                return matches!(parsed_host, Some(IpAddr::V4(address)) if pattern.matches(address));
            }

            if let (Ok(expected), Some(actual)) = (token.parse::<IpAddr>(), parsed_host) {
                return normalize_ip(expected) == actual;
            }

            token.eq_ignore_ascii_case(hostname)
        })
    }

    fn raw_header_allowed(&self, value: &str) -> bool {
        let value = value.trim();
        self.header_tokens()
            .any(|token| token.eq_ignore_ascii_case(value))
    }

    fn header_tokens(&self) -> impl Iterator<Item = &str> {
        self.allowed
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(self.server_host.as_str()))
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }
}

/// Normalize IPv4-mapped IPv6 without treating arbitrary IPv4-compatible IPv6
/// addresses as IPv4.
pub fn normalize_ip(address: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = address else {
        return address;
    };
    let octets = v6.octets();
    if octets[..10] == [0; 10] && octets[10..12] == [0xff, 0xff] {
        IpAddr::V4(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ))
    } else {
        IpAddr::V6(v6)
    }
}

#[derive(Clone, Copy, Debug)]
struct Ipv4Pattern([Option<u8>; 4]);

impl Ipv4Pattern {
    fn parse(value: &str) -> Option<Self> {
        let mut parts = value.trim().split('.');
        let mut result = [None; 4];
        for item in &mut result {
            let part = parts.next()?;
            *item = if part == "*" {
                None
            } else {
                // Reject signs, whitespace, empty components, and non-canonical
                // numeric spellings rather than accepting a loose integer grammar.
                if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                    return None;
                }
                let parsed = part.parse::<u8>().ok()?;
                if parsed.to_string() != part {
                    return None;
                }
                Some(parsed)
            };
        }
        parts.next().is_none().then_some(Self(result))
    }

    fn matches(self, address: Ipv4Addr) -> bool {
        self.0
            .into_iter()
            .zip(address.octets())
            .all(|(expected, actual)| expected.is_none_or(|expected| expected == actual))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn policy() -> AuthorizationPolicy {
        AuthorizationPolicy::new(
            "127.0.0.1, localhost, absolute, 192.0.2.*, 198.51.100.7, ::1",
            "dashboard.local",
        )
    }

    #[test]
    fn localhost_settings_mutations_match_request_host_not_listener_address() {
        use axum::http::HeaderValue;
        let policy = AuthorizationPolicy::new("127.0.0.1,localhost,absolute", "127.0.0.1");
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:24050"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:24050"),
        );
        assert!(policy.is_allowed(
            ip("127.0.0.1"),
            RequestHeaders {
                origin: Some("http://localhost:24050"),
                referer: None,
            },
        ));
        assert!(MutationAuthorization::from_headers(&headers).0);
        // Loopback aliases are not interchangeable origins.
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:24050"));
        assert!(!MutationAuthorization::from_headers(&headers).0);
        headers.insert(header::HOST, HeaderValue::from_static("localhost:24051"));
        assert!(!MutationAuthorization::from_headers(&headers).0);
    }

    #[test]
    fn mutation_provenance_rejects_ambiguous_headers_and_parser_recovery() {
        use axum::http::HeaderValue;
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("localhost:24050"));
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("http://localhost:24050/settings?overlay=x"),
        );
        assert!(same_origin(&headers));
        for origin in [
            "null",
            "",
            "localhost",
            "http://localhost:24050/",
            "http://user@localhost:24050",
            "http://localhost:24050#x",
            "http://localhost:24050 http://evil.example",
        ] {
            headers.insert(header::ORIGIN, HeaderValue::from_str(origin).unwrap());
            assert!(!same_origin(&headers), "{origin}");
        }
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:24050"),
        );
        assert!(same_origin(&headers));
        headers.append(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:24050"),
        );
        assert!(!same_origin(&headers));
        headers.remove(header::ORIGIN);
        headers.append(
            header::REFERER,
            HeaderValue::from_static("http://localhost:24050/"),
        );
        assert!(!same_origin(&headers));
        headers.remove(header::REFERER);
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://localhost:24050"),
        );
        headers.append(header::HOST, HeaderValue::from_static("localhost:24050"));
        assert!(!same_origin(&headers));
        headers.insert(header::HOST, HeaderValue::from_static("[::1]:24050"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://[::1]:24050"),
        );
        assert!(same_origin(&headers));
    }

    #[test]
    fn exhaustive_http_and_websocket_matrix_uses_same_predicate() {
        let cases = [
            ("127.0.0.1", None, None, true, "loopback without headers"),
            ("::ffff:127.0.0.1", None, None, true, "mapped loopback"),
            ("192.0.2.44", None, None, true, "four-part wildcard"),
            ("198.51.100.7", None, None, true, "exact remote IP"),
            ("203.0.113.9", None, None, false, "hostile remote"),
            (
                "127.0.0.1",
                Some("http://localhost:24050"),
                None,
                true,
                "approved origin with port",
            ),
            (
                "127.0.0.1",
                Some("HTTPS://DASHBOARD.LOCAL/path"),
                None,
                true,
                "server host token",
            ),
            (
                "127.0.0.1",
                Some("http://[::ffff:192.0.2.44]:24050"),
                None,
                true,
                "mapped IPv6 origin matches wildcard",
            ),
            (
                "127.0.0.1",
                Some("http://evil.example"),
                None,
                false,
                "hostile origin",
            ),
            (
                "127.0.0.1",
                None,
                Some("http://localhost/page"),
                true,
                "referer fallback",
            ),
            (
                "127.0.0.1",
                None,
                Some("http://evil.example"),
                false,
                "hostile referer",
            ),
            (
                "127.0.0.1",
                Some("http://evil.example"),
                Some("http://localhost"),
                false,
                "origin wins conflict",
            ),
            (
                "127.0.0.1",
                Some("http://localhost"),
                Some("http://evil.example"),
                true,
                "approved origin wins conflict",
            ),
            (
                "127.0.0.1",
                Some("absolute"),
                None,
                true,
                "raw absolute token",
            ),
            (
                "127.0.0.1",
                Some("localhost:24050"),
                None,
                false,
                "raw localhost port is not hostname",
            ),
            (
                "127.0.0.1",
                Some(" LOCALHOST "),
                None,
                true,
                "malformed raw exact token",
            ),
            (
                "127.0.0.1",
                Some(""),
                Some("http://localhost"),
                false,
                "empty origin remains authoritative",
            ),
            (
                "192.0.2.77",
                Some("http://localhost"),
                None,
                true,
                "wildcard peer and header",
            ),
            (
                "203.0.113.9",
                Some("http://localhost"),
                None,
                false,
                "good header cannot authorize hostile peer",
            ),
            (
                "198.51.100.7",
                Some("http://evil.example"),
                None,
                false,
                "good peer cannot bypass hostile header",
            ),
            (
                "203.0.113.9",
                Some("http://evil.example"),
                None,
                false,
                "both hostile",
            ),
        ];

        for (peer, origin, referer, expected, label) in cases {
            let headers = RequestHeaders { origin, referer };
            assert_eq!(
                policy().is_allowed(ip(peer), headers),
                expected,
                "HTTP: {label}"
            );
            assert_eq!(
                policy().authorize(ip(peer), headers).allowed(),
                expected,
                "WS: {label}"
            );
        }
    }

    #[test]
    fn wildcard_and_mapped_rules_are_strict() {
        let malformed = AuthorizationPolicy::new(
            "10.*.*, 10.*.*.*.1, 010.0.0.*, 300.0.0.*, host*name",
            "localhost",
        );
        assert!(!malformed.peer_allowed(ip("10.2.3.4")));
        assert!(!malformed.header_allowed("http://10.2.3.4"));

        let mapped = AuthorizationPolicy::new("198.51.100.7", "localhost");
        assert!(mapped.peer_allowed(ip("::ffff:198.51.100.7")));
        assert_eq!(normalize_ip(ip("2001:db8::1")), ip("2001:db8::1"));
        assert_eq!(
            normalize_ip(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
            ip("::1")
        );
    }

    #[test]
    fn hostname_matching_does_not_apply_peer_hostname_tokens() {
        let policy = AuthorizationPolicy::new("localhost", "dashboard.local");
        assert!(!policy.peer_allowed(ip("203.0.113.1")));
        assert!(policy.header_allowed("http://LOCALHOST:24050/path"));
        assert!(policy.header_allowed("https://dashboard.local"));
        assert!(!policy.header_allowed("https://dashboard.local.evil"));
    }
}

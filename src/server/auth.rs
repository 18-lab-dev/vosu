//! Request authorization shared by HTTP and WebSocket upgrade handlers.
//!
//! Authorization deliberately has two independent gates: the transport peer must be
//! trusted and, when supplied, the browser origin must also be trusted.

use std::net::{IpAddr, Ipv4Addr};
use url::Url;

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
        let header_ok = selected.is_none_or(|value| self.header_allowed(value));

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

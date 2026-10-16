//! Separate trusted dashboard pages from untrusted overlay scripts without moving overlay storage.
use super::{
    auth::{AuthorizationDecision, RequestHeaders},
    routes::RequestAuthorizer,
};
use axum::http::{HeaderMap, header};
use parking_lot::RwLock;
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub struct Origins {
    pub overlay: RwLock<String>,
    pub dashboard: RwLock<String>,
    listeners: RwLock<Option<[tokio::sync::watch::Receiver<super::runtime::ListenerState>; 2]>>,
    pub csrf: String,
    pub previous_dashboard: RwLock<Option<String>>,
}
impl Origins {
    pub fn new(overlay: String, dashboard: String) -> std::io::Result<Self> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(std::io::Error::other)?;
        Ok(Self {
            overlay: RwLock::new(overlay.trim_end_matches('/').to_owned()),
            dashboard: RwLock::new(dashboard.trim_end_matches('/').to_owned()),
            csrf: random.iter().map(|v| format!("{v:02x}")).collect(),
            listeners: RwLock::new(None),
            previous_dashboard: RwLock::new(None),
        })
    }
    pub fn observe_listeners(
        &self,
        overlay: tokio::sync::watch::Receiver<super::runtime::ListenerState>,
        dashboard: tokio::sync::watch::Receiver<super::runtime::ListenerState>,
    ) {
        *self.listeners.write() = Some([overlay, dashboard]);
    }
    fn effective(&self, dashboard: bool) -> Option<String> {
        let listeners = self.listeners.read();
        let state = listeners.as_ref()?[usize::from(dashboard)].borrow();
        let effective = state.effective.as_ref()?;
        Some(
            crate::desktop::dashboard_url(&effective.target.host, effective.address.port())
                .trim_end_matches('/')
                .to_owned(),
        )
    }
    pub fn overlay(&self) -> String {
        self.effective(false)
            .unwrap_or_else(|| self.overlay.read().clone())
    }
    pub fn dashboard(&self) -> String {
        self.effective(true)
            .unwrap_or_else(|| self.dashboard.read().clone())
    }
    pub fn dashboard_target_matches(&self, host: &str, port: u16) -> bool {
        self.target_matches(true, host, port)
    }
    pub fn overlay_target_matches(&self, host: &str, port: u16) -> bool {
        self.target_matches(false, host, port)
    }
    fn target_matches(&self, dashboard: bool, host: &str, port: u16) -> bool {
        let listeners = self.listeners.read();
        let Some(listeners) = listeners.as_ref() else {
            return false;
        };
        listeners[usize::from(dashboard)]
            .borrow()
            .effective
            .as_ref()
            .is_some_and(|effective| effective.target.host == host && effective.target.port == port)
    }
    fn accepts_ip(&self, dashboard: bool, ip: std::net::IpAddr) -> bool {
        let listeners = self.listeners.read();
        let Some(listeners) = listeners.as_ref() else {
            return false;
        };
        let state = listeners[usize::from(dashboard)].borrow();
        state.effective.as_ref().is_some_and(|effective| {
            effective.address.ip().is_unspecified()
                || super::auth::normalize_ip(effective.address.ip())
                    == super::auth::normalize_ip(ip)
        })
    }
    // Only accept the effective configured hostname or a loopback alias, never arbitrary Host.
    pub fn safe_origin(
        &self,
        dashboard: bool,
        headers: &HeaderMap,
        local_ip: Option<std::net::IpAddr>,
    ) -> String {
        let configured = if dashboard {
            self.dashboard()
        } else {
            self.overlay()
        };
        let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
            return configured;
        };
        let Ok(incoming) = url::Url::parse(&format!("http://{host}")) else {
            return configured;
        };
        let Ok(mut target) = url::Url::parse(&configured) else {
            return configured;
        };
        let name = incoming.host_str().unwrap_or_default();
        let loopback = |name: &str| {
            name == "localhost"
                || name
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        };
        let transport_host = local_ip.is_some_and(|local| {
            self.accepts_ip(dashboard, local)
                && name
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|host| {
                        super::auth::normalize_ip(host) == super::auth::normalize_ip(local)
                    })
        });
        if incoming.username().is_empty()
            && incoming.password().is_none()
            && incoming.path() == "/"
            && incoming.query().is_none()
            && incoming.fragment().is_none()
            && (name == target.host_str().unwrap_or_default()
                || (loopback(name) && loopback(target.host_str().unwrap_or_default()))
                || transport_host)
        {
            let _ = target.set_host(Some(name));
        }
        target.origin().ascii_serialization()
    }
    pub fn authorized(&self, headers: &HeaderMap, local_ip: Option<std::net::IpAddr>) -> bool {
        let mut tokens = headers.get_all("x-vosu-csrf").iter();
        let token_ok = tokens
            .next()
            .is_some_and(|value| bool::from(value.as_bytes().ct_eq(self.csrf.as_bytes())))
            && tokens.next().is_none();
        super::auth::MutationAuthorization::from_headers(headers).0
            && token_ok
            && headers
                .get(header::HOST)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|host| {
                    format!("http://{host}") == self.safe_origin(true, headers, local_ip)
                })
    }
    pub fn page_context(&self, headers: &HeaderMap, local_ip: Option<std::net::IpAddr>) -> String {
        let context = serde_json::json!({
            "overlayOrigin": self.safe_origin(false, headers, local_ip),
            "csrf": self.csrf,
            "rollbackOrigin": self.previous_dashboard.read().clone(),
        })
        .to_string()
        .replace('<', "\\u003c");
        format!(r#"<script id="vosu-context" type="application/json">{context}</script>"#)
    }
}

pub struct OriginAuthorizer {
    pub inner: Arc<dyn RequestAuthorizer>,
    pub origins: Arc<Origins>,
    pub dashboard: bool,
}
impl RequestAuthorizer for OriginAuthorizer {
    fn authorize(
        &self,
        peer: std::net::IpAddr,
        headers: RequestHeaders<'_>,
    ) -> AuthorizationDecision {
        let mut decision = self.inner.authorize(peer, headers);
        if !decision.header_ok
            && let Some(source) = headers
                .origin
                .or(headers.referer)
                .and_then(|v| url::Url::parse(v).ok())
            && let Ok(dashboard) = url::Url::parse(&self.origins.dashboard())
            && source.origin() == dashboard.origin()
        {
            decision.header_ok = true;
        }
        decision
    }
    fn origins(&self) -> Option<(&Origins, bool)> {
        Some((&self.origins, self.dashboard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wildcard_redirects_and_management_use_only_transport_observed_local_hosts() {
        use crate::server::runtime::{EffectiveListener, ListenTarget, ListenerState};
        let origins = Origins::new(
            "http://127.0.0.1:24860".into(),
            "http://127.0.0.1:24861".into(),
        )
        .unwrap();
        let state = |port| ListenerState {
            effective: Some(EffectiveListener {
                target: ListenTarget::new("0.0.0.0", port),
                address: format!("0.0.0.0:{port}").parse().unwrap(),
            }),
            ..ListenerState::default()
        };
        let (_, overlay) = tokio::sync::watch::channel(state(24860));
        let (_, dashboard) = tokio::sync::watch::channel(state(24861));
        origins.observe_listeners(overlay, dashboard);
        let local = Some("192.168.1.7".parse().unwrap());
        assert_eq!(origins.dashboard(), "http://127.0.0.1:24861");
        for (host, expected, authorized) in [
            ("192.168.1.7:24861", "http://192.168.1.7:24861", true),
            ("192.168.1.99:24861", "http://127.0.0.1:24861", false),
            ("attacker.invalid:24861", "http://127.0.0.1:24861", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, format!("http://{host}").parse().unwrap());
            headers.insert("x-vosu-csrf", origins.csrf.parse().unwrap());
            assert_eq!(origins.safe_origin(true, &headers, local), expected);
            assert_eq!(origins.authorized(&headers, local), authorized);
        }
    }
}

/// Counts sockets, not people or browser sources. Only explicit overlay identities count.
#[derive(Clone, Default)]
struct OverlayConnections(Arc<Mutex<HashMap<String, usize>>>);

struct OverlayConnection {
    registry: OverlayConnections,
    overlay: String,
}

impl OverlayConnections {
    fn register(&self, overlay: Option<String>) -> Option<OverlayConnection> {
        let overlay = overlay?;
        *self.0.lock().unwrap_or_else(PoisonError::into_inner).entry(overlay.clone()).or_default() += 1;
        Some(OverlayConnection { registry: self.clone(), overlay })
    }

    fn counts(&self) -> HashMap<String, usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Drop for OverlayConnection {
    fn drop(&mut self) {
        let mut counts = self.registry.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.overlay) {
            *count -= 1;
            if *count == 0 { counts.remove(&self.overlay); }
        }
    }
}

fn socket_overlay(request: &Request<Body>) -> Option<String> {
    if let Some(raw) = request.uri().query().and_then(|query| decoded_query_value(query, "l")) {
        let client = CommandClient::from_query(0, Some(&raw)).ok()?;
        let (overlay, _) = crate::counters::split_counter(&client.overlay);
        if valid_overlay_component(overlay) { return Some(overlay.to_owned()); }
    }
    // WebSocket Origin contains no path. It cannot distinguish overlays on one origin.
    // A Referer is useful only when it names a concrete local overlay path.
    let referer = request.headers().get(header::REFERER)?.to_str().ok()?;
    let url = url::Url::parse(referer).ok()?;
    let host = request.headers().get(header::HOST)?.to_str().ok()?;
    if url[url::Position::BeforeHost..url::Position::AfterPort] != *host { return None; }
    let encoded = url.path().trim_start_matches('/').split('/').next()?;
    let overlay = percent_decode_once(encoded).ok()?;
    valid_overlay_component(&overlay).then_some(overlay)
}

fn valid_overlay_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

#[cfg(test)]
mod dashboard_tests {
    use super::*;

    #[test]
    fn attributes_profiles_and_paths_but_not_origin_alone() {
        let request = |uri: &str| Request::builder().uri(uri).header(header::HOST, "127.0.0.1:24050").body(Body::empty()).unwrap();
        assert_eq!(socket_overlay(&request("/websocket/commands?l=%2F%40vosu-profile%2Fstream%2Fcounter")), Some("counter".into()));
        let mut origin = request("/websocket/v2");
        origin.headers_mut().insert(header::ORIGIN, "http://127.0.0.1:24050".parse().unwrap());
        assert_eq!(socket_overlay(&origin), None);
        origin.headers_mut().insert(header::REFERER, "http://127.0.0.1:24050/counter/index.html".parse().unwrap());
        assert_eq!(socket_overlay(&origin), Some("counter".into()));
        origin.headers_mut().insert(header::REFERER, "http://untrusted.test/counter/".parse().unwrap());
        assert_eq!(socket_overlay(&origin), None);
    }

    #[test]
    fn registrations_count_live_sockets_and_drop_cleanly() {
        let registry = OverlayConnections::default();
        let first = registry.register(Some("counter".into()));
        let second = registry.register(Some("counter".into()));
        assert_eq!(registry.counts()["counter"], 2);
        drop(first);
        assert_eq!(registry.counts()["counter"], 1);
        drop(second);
        assert!(registry.counts().is_empty());
        assert!(registry.register(None).is_none());
    }
}

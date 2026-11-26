/// `DashboardStatus.removed_routes` (A7.4): `{"overlay": null | {"id","name"},
/// "feature", "attempts", "first_seen_seconds_ago", "last_seen_seconds_ago"}`.
/// `notices` is already expired and revalidated against `names`; the names
/// are raw strings the dashboard escapes.
fn removed_routes_view(
    notices: &[crate::removed_routes::RemovedNotice],
    names: &HashMap<String, String>,
    now: std::time::Instant,
) -> Value {
    let ago = |instant: std::time::Instant| now.saturating_duration_since(instant).as_secs();
    Value::Array(
        notices
            .iter()
            .filter_map(|notice| {
                let overlay = match &notice.overlay {
                    None => Value::Null,
                    Some(id) => json!({ "id": id, "name": names.get(id)? }),
                };
                Some(json!({
                    "overlay": overlay,
                    "feature": notice.feature.id(),
                    "attempts": notice.attempts,
                    "first_seen_seconds_ago": ago(notice.first_seen),
                    "last_seen_seconds_ago": ago(notice.last_seen),
                }))
            })
            .collect(),
    )
}

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

fn socket_overlay(request: &Request<Body>, composition: &dyn OverlayComposition) -> Option<String> {
    // The dashboard's own preview frames are not browser sources: their
    // pages mark every socket they open with `vosu-preview=1`
    // (`PREVIEW_SOCKET_SCRIPT`), since a WebSocket handshake has no Referer.
    if is_preview_request(request) {
        return None;
    }
    if let Some(raw) = request.uri().query().and_then(|query| decoded_query_value(query, "l")) {
        let overlay = composition.query_overlay(&raw)?;
        if valid_overlay_component(&overlay) {
            return Some(overlay);
        }
    }
    // WebSocket Origin contains no path. It cannot distinguish overlays on one origin.
    // A Referer (sent by non-browser clients) is useful only when it names a
    // concrete local overlay path.
    let referer = request.headers().get(header::REFERER)?.to_str().ok()?;
    let url = url::Url::parse(referer).ok()?;
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = request.headers().get(header::HOST)?.to_str().ok()?;
    if url[url::Position::BeforeHost..url::Position::AfterPort] != *host { return None; }
    let encoded = url.path().trim_start_matches('/').split('/').next()?;
    let overlay = percent_decode_once(encoded).ok()?;
    valid_overlay_component(&overlay).then_some(overlay)
}

fn valid_overlay_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

/// A request from a dashboard card preview: a socket the preview page marked,
/// or an HTTP request whose Referer is the marked preview page itself.
fn is_preview_source(request: &Request<Body>) -> bool {
    is_preview_request(request)
        || request
            .headers()
            .get(header::REFERER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| url::Url::parse(value).ok())
            .and_then(|url| url.query().map(|query| query.split('&').any(|pair| pair == crate::server::pages::OVERLAY_PREVIEW_QUERY)))
            .unwrap_or(false)
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
        let mut preview = request("/websocket/v2?vosu-preview=1");
        preview.headers_mut().insert(header::REFERER, "http://127.0.0.1:24050/counter/index.html".parse().unwrap());
        assert_eq!(socket_overlay(&preview), None, "dashboard previews are not browser sources");
        assert_eq!(socket_overlay(&request("/websocket/commands?l=counter&vosu-preview=1")), None);
        assert_eq!(socket_overlay(&request("/websocket/commands?l=counter&vosu-preview=0")), Some("counter".into()));
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

#[cfg(test)]
mod removed_routes_view_tests {
    use super::*;
    use crate::removed_routes::{RemovedFeature, RemovedRoutes};

    #[test]
    fn view_names_installed_identities_and_never_an_unknown_or_stale_one() {
        let recorder = RemovedRoutes::default();
        let start = std::time::Instant::now();
        recorder.record(Some("<b>x</b>".into()), RemovedFeature::IngamePage, start);
        recorder.record(Some("gone".into()), RemovedFeature::SelectedV2Feed, start);
        recorder.record(None, RemovedFeature::IngamePage, start);
        let now = start + std::time::Duration::from_secs(90);
        let names = HashMap::from([("<b>x</b>".to_owned(), "<img src=x onerror=alert(1)>".to_owned())]);
        let notices = recorder.snapshot(now, |id| names.contains_key(id));
        let view = removed_routes_view(&notices, &names, now);
        assert_eq!(
            view,
            json!([
                {"overlay": {"id": "<b>x</b>", "name": "<img src=x onerror=alert(1)>"}, "feature": "ingame-page",
                 "attempts": 1, "first_seen_seconds_ago": 90, "last_seen_seconds_ago": 90},
                {"overlay": null, "feature": "ingame-page", "attempts": 1,
                 "first_seen_seconds_ago": 90, "last_seen_seconds_ago": 90},
            ])
        );
        // A name missing from the read's installed set is never shown, even
        // if the snapshot still held its identity.
        let all = recorder.snapshot(now, |_| true);
        assert_eq!(removed_routes_view(&all, &HashMap::new(), now).as_array().unwrap().len(), 1);
    }
}

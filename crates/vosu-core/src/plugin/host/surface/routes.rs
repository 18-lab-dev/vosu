use super::capture::Requester;
use super::*;
use crate::state::surface::SurfaceRoute;
use vosu_surface::listener::ListenerRole;
use vosu_surface::routes::{
    ProviderResponse, RegistrationError, RegistrationLease, RouteDescriptor, RoutePath,
    RouteProvider, SurfaceRequest,
};

const MAX_ROUTES: usize = 256;
struct Unregister {
    host: Weak<PluginHost>,
    owner: u64,
    ids: Vec<u64>,
}
impl Drop for Unregister {
    fn drop(&mut self) {
        if let Some(host) = self.host.upgrade() {
            let removed = {
                let mut surfaces = host.surfaces.lock().unwrap();
                let mut removed = Vec::new();
                let mut index = 0;
                while index < surfaces.routes.len() {
                    if surfaces.routes[index].owner == self.owner
                        && self.ids.contains(&surfaces.routes[index].id)
                    {
                        removed.push(surfaces.routes.swap_remove(index));
                    } else {
                        index += 1;
                    }
                }
                removed
            };
            // Provider destructors can reenter the host.
            drop(removed);
        }
    }
}

fn parts(path: &RoutePath) -> Result<(Vec<&str>, bool), RegistrationError> {
    let (text, template, prefix) = match path {
        RoutePath::Exact(text) => (text, false, false),
        RoutePath::Template(text) => (text, true, false),
        RoutePath::Prefix(text) => (text, false, true),
        RoutePath::Children(text) => (text, false, true),
    };
    if !text.starts_with('/')
        || text.contains(['?', '#', '%', '\\'])
        || text.contains("//")
        || (text.len() > 1 && text.ends_with('/'))
        || text.len() > 2048
    {
        return Err(RegistrationError::InvalidDescriptor);
    }
    let segments: Vec<_> = text
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segments.iter().any(|segment| {
        *segment == "."
            || *segment == ".."
            || segment.chars().any(char::is_control)
            || ((segment.contains('{') || segment.contains('}'))
                && !(template
                    && segment.starts_with('{')
                    && segment.ends_with('}')
                    && segment.len() > 2
                    && segment[1..segment.len() - 1]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')))
    }) {
        return Err(RegistrationError::InvalidDescriptor);
    }
    Ok((segments, prefix))
}
fn intersects(a: &RoutePath, b: &RoutePath) -> bool {
    if let RoutePath::Exact(path) = a {
        return matches(b, path);
    }
    if let RoutePath::Exact(path) = b {
        return matches(a, path);
    }
    let a_children = matches!(a, RoutePath::Children(_));
    let b_children = matches!(b, RoutePath::Children(_));
    let (a, a_prefix) = parts(a).expect("validated descriptor");
    let (b, b_prefix) = parts(b).expect("validated descriptor");
    let minimum = (a.len() + usize::from(a_children)).max(b.len() + usize::from(b_children));
    let maximum = (if a_prefix { usize::MAX } else { a.len() }).min(if b_prefix {
        usize::MAX
    } else {
        b.len()
    });
    if minimum > maximum {
        return false;
    }
    a.iter()
        .zip(&b)
        .all(|(a, b)| a == b || a.starts_with('{') || b.starts_with('{'))
}
fn matches(path: &RoutePath, request: &str) -> bool {
    if let RoutePath::Exact(exact) = path {
        return exact == request;
    }
    if !request.starts_with('/')
        || request.contains("//")
        || (matches!(path, RoutePath::Template(_)) && request.ends_with('/'))
    {
        return false;
    }
    let (segments, prefix) = parts(path).expect("validated descriptor");
    let request: Vec<_> = request
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    (!matches!(path, RoutePath::Children(_)) || segments.len() < request.len())
        && (segments.len() == request.len() || (prefix && segments.len() <= request.len()))
        && segments
            .iter()
            .zip(request)
            .all(|(pattern, value)| pattern.starts_with('{') || *pattern == value)
}
fn collides(a: &RouteDescriptor, b: &RouteDescriptor) -> bool {
    a.listener == b.listener && a.method == b.method && intersects(&a.path, &b.path)
}
fn validate(route: &RouteDescriptor) -> Result<(), RegistrationError> {
    parts(&route.path)?;
    if !matches!(
        route.method.as_str(),
        "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
    ) || (route.websocket
        && (route.method != "GET" || route.listener == ListenerRole::Dashboard))
    {
        return Err(RegistrationError::InvalidDescriptor);
    }
    // Removed exact routes are tombstones: no template or prefix can revive them.
    if ["/websocket/v2/subscribe", "/files/skin/"]
        .iter()
        .any(|retired| matches(&route.path, retired))
    {
        return Err(RegistrationError::Collision);
    }
    Ok(())
}

pub(super) fn register(
    requester: &Requester,
    routes: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
) -> Result<RegistrationLease, RegistrationError> {
    let host = requester
        .host
        .upgrade()
        .ok_or(RegistrationError::UnknownRequester)?;
    let instances = host.instances.lock().unwrap();
    let reader = requester
        .live(&instances)
        .map_err(|_| RegistrationError::UnknownRequester)?
        .plugin_id
        .clone();
    if !matches!(
        reader.as_str(),
        "vosu.adapter.upstream"
            | "vosu.adapter.gosumemory"
            | "vosu.adapter.streamcompanion"
            | "vosu.overlayhost"
    ) {
        return Err(RegistrationError::UnknownRequester);
    }
    let result = install(
        &host,
        requester.instance.0,
        &reader,
        &mut routes
            .into_iter()
            .map(|(route, provider)| (route, Some(provider)))
            .collect(),
    );
    drop(instances);
    result
}
fn install(
    host: &Arc<PluginHost>,
    owner: u64,
    reader: &str,
    routes: &mut Vec<(RouteDescriptor, Option<Arc<dyn RouteProvider>>)>,
) -> Result<RegistrationLease, RegistrationError> {
    for (route, _) in routes.iter() {
        validate(route)?;
    }
    for (index, (route, _)) in routes.iter().enumerate() {
        if routes[..index]
            .iter()
            .any(|(other, _)| collides(route, other))
        {
            return Err(RegistrationError::Collision);
        }
    }
    let mut surfaces = host.surfaces.lock().unwrap();
    if surfaces.routes.len().saturating_add(routes.len()) > MAX_ROUTES {
        return Err(RegistrationError::Capacity);
    }
    if routes.iter().any(|(route, _)| {
        surfaces
            .routes
            .iter()
            .any(|other| collides(route, &other.descriptor))
    }) {
        return Err(RegistrationError::Collision);
    }
    let end = surfaces
        .next_route
        .checked_add(routes.len() as u64)
        .ok_or(RegistrationError::Capacity)?;
    let mut ids = Vec::with_capacity(routes.len());
    for (descriptor, provider) in routes.drain(..) {
        surfaces.next_route += 1;
        let id = surfaces.next_route;
        ids.push(id);
        surfaces.routes.push(SurfaceRoute {
            id,
            owner,
            reader: reader.to_owned(),
            descriptor,
            provider,
            feeds: Vec::new(),
        });
    }
    debug_assert_eq!(surfaces.next_route, end);
    Ok(RegistrationLease::new(Unregister {
        host: Arc::downgrade(host),
        owner,
        ids,
    }))
}

impl PluginHost {
    /// Trusted transport calls this only after exact dashboard-origin admission.
    /// The secret is checked here and is never returned through the surface API.
    pub fn surface_management_provenance(
        &self,
        listener: ListenerRole,
        supplied_csrf: Option<&str>,
        expected_csrf: &str,
    ) -> Option<vosu_surface::management::ManagementProvenance> {
        self.surfaces
            .lock()
            .unwrap()
            .management
            .authorize(listener, supplied_csrf, expected_csrf)
    }

    /// Core-owned routes must reserve their descriptors in this same registry.
    pub fn register_core_surface_routes(
        self: &Arc<Self>,
        routes: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
    ) -> Result<RegistrationLease, RegistrationError> {
        install(
            self,
            0,
            "",
            &mut routes
                .into_iter()
                .map(|(route, provider)| (route, Some(provider)))
                .collect(),
        )
    }

    /// Reserves transport-owned handlers that remain outside provider dispatch.
    /// Reservations participate in the same atomic collision and lifetime rules.
    pub fn reserve_core_surface_routes(
        self: &Arc<Self>,
        routes: Vec<RouteDescriptor>,
    ) -> Result<RegistrationLease, RegistrationError> {
        install(
            self,
            0,
            "",
            &mut routes.into_iter().map(|route| (route, None)).collect(),
        )
    }

    /// Transport performs peer/origin/key admission before calling this seam.
    /// The provider Arc is cloned under the lock and invoked only after release.
    pub async fn dispatch_surface_route(
        &self,
        listener: ListenerRole,
        websocket: bool,
        mut request: SurfaceRequest,
    ) -> Option<ProviderResponse> {
        if request.body.len() > 1024 * 1024
            || request.path.len() > 2048
            || !request.valid_metadata()
        {
            return None;
        }
        let (id, provider) = {
            let instances = self.instances.lock().unwrap();
            let surfaces = self.surfaces.lock().unwrap();
            if listener == ListenerRole::Overlay
                || request
                    .management
                    .as_ref()
                    .is_some_and(|proof| !surfaces.management.recognizes(proof))
            {
                request.management = None;
            }
            surfaces.routes.iter().find(|route| {
                (route.owner == 0 || matches!(instances.get(&crate::identity::PluginInstanceId(route.owner)), Some(InstanceSlot::Live(entry)) if entry.publisher.bundled)) &&
                route.descriptor.listener == listener && route.descriptor.websocket == websocket && route.descriptor.method == request.method && matches(&route.descriptor.path, &request.path)
            }).and_then(|route| route.provider.as_ref().map(|provider| (route.id, provider.clone())))
        }?;
        let response = provider.respond(request).await;
        match &response {
            ProviderResponse::Http(_) if websocket => return None,
            ProviderResponse::Feed(feed) => {
                if !websocket {
                    feed.state.close();
                    return None;
                }
                let admitted = {
                    let mut surfaces = self.surfaces.lock().unwrap();
                    surfaces
                        .routes
                        .iter_mut()
                        .find(|route| route.id == id)
                        .is_some_and(|route| {
                            route.feeds.retain(|feed| feed.strong_count() != 0);
                            if route.feeds.len() >= MAX_SUBSCRIPTIONS {
                                return false;
                            }
                            route.feeds.push(Arc::downgrade(&feed.state));
                            true
                        })
                };
                if !admitted {
                    feed.state.close();
                    return None;
                }
            }
            ProviderResponse::Http(response) if !response.valid_metadata() => return None,
            ProviderResponse::Http(_) => {}
        }
        Some(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlapping_match_sets_and_methods() {
        let exact = RoutePath::Exact("/json/v2".into());
        assert!(intersects(
            &exact,
            &RoutePath::Template("/json/{schema}".into())
        ));
        assert!(intersects(&exact, &RoutePath::Prefix("/json".into())));
        assert!(!intersects(&exact, &RoutePath::Prefix("/js".into())));
        assert!(!intersects(
            &exact,
            &RoutePath::Template("/json/{schema}/more".into())
        ));
        let mut first = RouteDescriptor {
            listener: ListenerRole::Overlay,
            method: "GET".into(),
            path: exact,
            websocket: false,
        };
        let mut second = first.clone();
        second.websocket = true;
        assert!(collides(&first, &second));
        second.method = "HEAD".into();
        assert!(!collides(&first, &second));
        second.method = "GET".into();
        first.listener = ListenerRole::Dashboard;
        assert!(!collides(&first, &second));
    }
    #[test]
    fn descendants_exclude_the_retired_base_without_hiding_real_collisions() {
        let children = RoutePath::Children("/files/skin".into());
        assert!(!matches(&children, "/files/skin"));
        assert!(!matches(&children, "/files/skin/"));
        assert!(matches(&children, "/files/skin/note.png"));
        assert!(!intersects(
            &children,
            &RoutePath::Exact("/files/skin/".into())
        ));
        assert!(!intersects(
            &children,
            &RoutePath::Template("/files/{kind}".into())
        ));
        assert!(intersects(
            &children,
            &RoutePath::Template("/files/{kind}/{name}".into())
        ));
        assert!(intersects(&children, &RoutePath::Prefix("/files".into())));
        assert!(intersects(
            &children,
            &RoutePath::Children("/files/skin/subdir".into())
        ));
    }
    #[test]
    fn retired_and_malformed_matchers_fail_closed() {
        for path in [
            RoutePath::Prefix("/".into()),
            RoutePath::Template("/websocket/{version}/subscribe".into()),
            RoutePath::Exact("/a/../b".into()),
            RoutePath::Template("/a/{bad-name}".into()),
        ] {
            assert!(
                validate(&RouteDescriptor {
                    listener: ListenerRole::Overlay,
                    method: "GET".into(),
                    path,
                    websocket: false
                })
                .is_err()
            );
        }
    }
}

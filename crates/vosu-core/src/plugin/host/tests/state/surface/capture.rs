use super::*;
use vosu_surface::listener::ListenerRole;
use vosu_surface::routes::{
    ProviderResponse, RegistrationError, RouteDescriptor, RoutePath, RouteProvider, SurfaceRequest,
    SurfaceResponse,
};

fn reader(
    host: &Arc<crate::plugin::host::PluginHost>,
    name: &str,
    instance: u64,
    fields: &[&str],
) -> Arc<dyn vosu_surface::requester::BundledRequester> {
    let requirements = fields
        .iter()
        .map(|field| format!("[[requires]]\nid='{field}'\nversion='*'\n"))
        .collect::<String>();
    let manifest =
        session_manifest(name, &[name]).replace("[match]", &format!("{requirements}[match]"));
    publisher::register_manifest(name, &manifest);
    let _events = publisher::register(name);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &publisher::manifest(name),
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: publisher::vosu_plugin_vtable,
    }])
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(instance),
        &loaded[0],
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    host.surface_requester(PluginInstanceId::new(instance))
        .unwrap()
}
fn descriptor(path: RoutePath) -> RouteDescriptor {
    RouteDescriptor {
        listener: ListenerRole::Overlay,
        method: "GET".into(),
        path,
        websocket: false,
    }
}
struct Reentrant(Arc<crate::plugin::host::PluginHost>);
impl RouteProvider for Reentrant {
    fn respond(
        &self,
        request: SurfaceRequest,
    ) -> vosu_surface::routes::ResponseFuture<'_, ProviderResponse> {
        Box::pin(async move {
            // These would deadlock if dispatch retained either registry or instance locks.
            drop(self.0.surfaces.lock().unwrap());
            drop(self.0.instances.lock().unwrap());
            assert!(request.management.is_none());
            ProviderResponse::Http(SurfaceResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: vosu_surface::routes::ResponseBody::Bytes(bytes::Bytes::from_static(b"42")),
            })
        })
    }
}
fn request(context: Arc<vosu_surface::state::CapturedSelection>, path: &str) -> SurfaceRequest {
    SurfaceRequest {
        method: "GET".into(),
        path: path.into(),
        query: Vec::new(),
        headers: Vec::new(),
        body: Arc::from([]),
        context,
        management: None,
    }
}
fn dispatch(
    rt: &tokio::runtime::Runtime,
    host: &crate::plugin::host::PluginHost,
    request: SurfaceRequest,
) -> Option<ProviderResponse> {
    rt.block_on(tokio::time::timeout(
        RECV_TIMEOUT,
        host.dispatch_surface_route(ListenerRole::Overlay, false, request),
    ))
    .expect("surface dispatch timed out")
}

#[test]
fn coherent_capture_grants_revisions_roster_and_no_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let foreground = Arc::new(SyntheticForegroundSource::new(Some(8871)));
    let (_logs, host) = test_host_with(rt.handle().clone(), foreground.clone(), RECV_TIMEOUT);
    let first = publisher_instance(&host, "test.p0.first", 8871, 8871);
    let second = publisher_instance(&host, "test.p0.second", 8872, 8872);
    let api = reader(
        &host,
        "test.p0.reader",
        8873,
        &["field:vosu.client.pid", "field:vosu.play.combo"],
    );
    assert_eq!(
        send(
            "test.p0.first",
            &batch(first, &[("vosu.play", "/combo", PublishValueRef::U64(31))])
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(
        send(
            "test.p0.second",
            &batch(second, &[("vosu.play", "/combo", PublishValueRef::U64(72))])
        ),
        vosu_status::VOSU_OK
    );
    let selectors = [
        selector("vosu.client", "/pid"),
        selector("vosu.play", "/combo"),
        selector("vosu.client", "/name"),
    ];
    let old = api.capture(SessionTarget::Focused, &selectors).unwrap();
    assert_eq!(old.values[0].value, Some(CborValue::Integer(8871.into())));
    assert_eq!(old.values[1].value, Some(CborValue::Integer(31.into())));
    // Name is present through the legacy read, proving the ungranted absence is not vacuous.
    assert!(
        host.surface_host()
            .read("test.p0.reader", SessionTarget::Focused, &selectors[2])
            .unwrap()
            .value
            .is_some()
    );
    assert_eq!(old.values[2].value, None);
    assert_eq!(old.roster.sessions.len(), 2);
    assert!(
        old.roster
            .sessions
            .iter()
            .any(|session| session.foreground == Some(true)
                && session.session == old.session.as_ref().unwrap().session)
    );
    foreground.set(Some(8872));
    host.foreground_changed();
    let new = api.capture(SessionTarget::Focused, &selectors).unwrap();
    assert_eq!(new.values[0].value, Some(CborValue::Integer(8872.into())));
    assert_eq!(new.values[1].value, Some(CborValue::Integer(72.into())));
    assert_ne!(old.session, new.session);
    assert!(new.focus_revision > old.focus_revision);
    assert!(new.roster.revision > old.roster.revision);
    assert_eq!(old.values[1].value, Some(CborValue::Integer(31.into())));
    let fixed = old.session.as_ref().unwrap().session.clone();
    let before = api
        .capture(SessionTarget::Id(fixed.clone()), &selectors)
        .unwrap();
    assert_eq!(
        send(
            "test.p0.first",
            &batch(first, &[("vosu.play", "/combo", PublishValueRef::U64(32))])
        ),
        vosu_status::VOSU_OK
    );
    let after = api
        .capture(SessionTarget::Id(fixed.clone()), &selectors)
        .unwrap();
    assert_eq!(
        before
            .namespace_revisions
            .iter()
            .find(|(ns, _)| ns == "vosu.client"),
        after
            .namespace_revisions
            .iter()
            .find(|(ns, _)| ns == "vosu.client")
    );
    assert_ne!(
        before
            .namespace_revisions
            .iter()
            .find(|(ns, _)| ns == "vosu.play"),
        after
            .namespace_revisions
            .iter()
            .find(|(ns, _)| ns == "vosu.play")
    );
    for handle in [first, second] {
        assert!(
            host.state()
                .subscriptions
                .lock()
                .unwrap()
                .demand(SessionKey(handle), Some("play"))
                .selectors
                .is_empty()
        );
    }
    host.process_exited(8871);
    assert!(matches!(
        api.capture(SessionTarget::Id(fixed), &selectors),
        Err(SurfaceError::UnknownSession)
    ));
    assert!(
        api.capture_roster("not-this-game")
            .unwrap()
            .sessions
            .is_empty()
    );
    host.destroy_all(RECV_TIMEOUT);
    assert!(matches!(
        api.capture(SessionTarget::Focused, &selectors),
        Err(SurfaceError::UnknownReader)
    ));
}

#[test]
fn routes_are_adapter_only_atomic_role_method_scoped_and_raii() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    publisher_instance(&host, "test.p0.game", 8881, 8881);
    let foreign = reader(&host, "test.p0.foreign", 8882, &[]);
    let adapter = reader(&host, "vosu.adapter.upstream", 8883, &[]);
    let other = reader(&host, "vosu.adapter.gosumemory", 8884, &[]);
    let route = descriptor(RoutePath::Exact("/p0-test".into()));
    let provider: Arc<dyn RouteProvider> = Arc::new(Reentrant(host.clone()));
    assert!(matches!(
        foreign.register_routes(vec![(route.clone(), provider.clone())]),
        Err(RegistrationError::UnknownRequester)
    ));
    let lease = adapter
        .register_routes(vec![(route.clone(), provider.clone())])
        .unwrap();
    let overlapping = descriptor(RoutePath::Template("/{name}".into()));
    assert!(matches!(
        other.register_routes(vec![
            (
                descriptor(RoutePath::Exact("/p0-new".into())),
                provider.clone()
            ),
            (overlapping, provider.clone())
        ]),
        Err(RegistrationError::Collision)
    ));
    let context = Arc::new(adapter.capture(SessionTarget::Focused, &[]).unwrap());
    assert!(dispatch(&rt, &host, request(context.clone(), "/p0-new")).is_none());
    assert!(
        matches!(dispatch(&rt, &host, request(context.clone(), "/p0-test")), Some(ProviderResponse::Http(response)) if response.status == 200)
    );
    let mut head = route.clone();
    head.method = "HEAD".into();
    let head_lease = other
        .register_routes(vec![(head, provider.clone())])
        .unwrap();
    let mut dashboard = route.clone();
    dashboard.listener = ListenerRole::Dashboard;
    let dashboard_lease = other
        .register_routes(vec![(dashboard, provider.clone())])
        .unwrap();
    drop(lease);
    assert!(dispatch(&rt, &host, request(context.clone(), "/p0-test")).is_none());
    let lease = other.register_routes(vec![(route, provider)]).unwrap();
    host.destroy_all(RECV_TIMEOUT);
    assert!(dispatch(&rt, &host, request(context, "/p0-test")).is_none());
    assert!(host.surfaces.lock().unwrap().routes.is_empty());
    drop((lease, head_lease, dashboard_lease));
}

#[test]
fn replacement_is_atomic_and_disconnect_preserves_other_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let handle = publisher_instance(&host, "test.p0.replace", 8891, 8891);
    let api = reader(
        &host,
        "test.p0.replace-reader",
        8892,
        &["field:vosu.play.combo"],
    );
    let first = api
        .subscribe(
            SessionTarget::Focused,
            vec![selector("vosu.play", "/combo")],
        )
        .unwrap();
    let second = api
        .subscribe(
            SessionTarget::Focused,
            vec![selector("vosu.play", "/combo")],
        )
        .unwrap();
    assert!(
        first
            .replace_selectors(vec![selector("vosu.play", "/~2")])
            .is_err()
    );
    assert!(
        !host
            .state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    );
    first
        .replace_selectors(vec![selector("vosu.client", "/name")])
        .unwrap();
    assert_eq!(first.updates.borrow().values[0].value, None);
    drop(second);
    assert!(
        host.state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    );
    drop(first);
    host.destroy_all(RECV_TIMEOUT);
}

struct Feed(Arc<dyn vosu_surface::requester::BundledRequester>);
struct TestFeed(std::sync::Mutex<Option<vosu_surface::state::SurfaceSubscription>>);
impl vosu_surface::routes::FeedControl for TestFeed {
    fn id(&self) -> u64 {
        1
    }
    fn receive(
        &self,
        _: vosu_surface::routes::Inbound,
    ) -> vosu_surface::routes::ResponseFuture<'_, Option<bytes::Bytes>> {
        Box::pin(async { None })
    }
    fn replace_filters(&self, _: bytes::Bytes) -> Result<(), String> {
        Err("not used".into())
    }
    fn write_completed(&self) {}
    fn send_text(&self, _: bytes::Bytes) -> vosu_surface::routes::ResponseFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn try_send_text(&self, _: bytes::Bytes) -> bool {
        false
    }
    fn close(&self) {
        self.0.lock().unwrap().take();
    }
    fn is_closed(&self) -> bool {
        self.0.lock().unwrap().is_none()
    }
}
struct TestReceiver;
impl vosu_surface::routes::FeedReceiver for TestReceiver {
    fn recv(
        &mut self,
    ) -> vosu_surface::routes::ResponseFuture<'_, Option<vosu_surface::routes::Outbound>> {
        Box::pin(async { None })
    }
}
impl RouteProvider for Feed {
    fn respond(
        &self,
        _request: SurfaceRequest,
    ) -> vosu_surface::routes::ResponseFuture<'_, ProviderResponse> {
        Box::pin(async move {
            let subscription = self
                .0
                .subscribe(
                    SessionTarget::Focused,
                    vec![selector("vosu.play", "/combo")],
                )
                .unwrap();
            ProviderResponse::Feed(vosu_surface::routes::SurfaceFeed {
                state: Arc::new(TestFeed(std::sync::Mutex::new(Some(subscription)))),
                receiver: Box::new(TestReceiver),
            })
        })
    }
}

#[test]
fn unregistering_feed_releases_connected_client_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let handle = publisher_instance(&host, "test.p0.feed", 8893, 8893);
    let api = reader(
        &host,
        "vosu.adapter.streamcompanion",
        8894,
        &["field:vosu.play.combo"],
    );
    let mut route = descriptor(RoutePath::Exact("/p0-feed".into()));
    route.websocket = true;
    let lease = api
        .register_routes(vec![(route.clone(), Arc::new(Feed(api.clone())))])
        .unwrap();
    route.websocket = false;
    assert!(matches!(
        api.register_routes(vec![(route, Arc::new(Feed(api.clone())))]),
        Err(RegistrationError::Collision)
    ));
    let context = Arc::new(api.capture(SessionTarget::Focused, &[]).unwrap());
    let Some(ProviderResponse::Feed(feed)) = rt
        .block_on(tokio::time::timeout(
            RECV_TIMEOUT,
            host.dispatch_surface_route(ListenerRole::Overlay, true, request(context, "/p0-feed")),
        ))
        .unwrap()
    else {
        panic!("feed was not opened")
    };
    assert!(!feed.state.is_closed());
    assert!(
        !host
            .state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    );
    drop(lease);
    assert!(feed.state.is_closed());
    assert!(
        host.state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    );
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn concurrent_focus_changes_never_mix_namespace_images() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let foreground = Arc::new(SyntheticForegroundSource::new(Some(8895)));
    let (_logs, host) = test_host_with(rt.handle().clone(), foreground.clone(), RECV_TIMEOUT);
    let first = publisher_instance(&host, "test.p0.concurrent-first", 8895, 8895);
    let second = publisher_instance(&host, "test.p0.concurrent-second", 8896, 8896);
    assert_eq!(
        send(
            "test.p0.concurrent-first",
            &batch(first, &[("vosu.play", "/combo", PublishValueRef::U64(51))])
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(
        send(
            "test.p0.concurrent-second",
            &batch(second, &[("vosu.play", "/combo", PublishValueRef::U64(96))])
        ),
        vosu_status::VOSU_OK
    );
    let api = reader(
        &host,
        "test.p0.concurrent-reader",
        8897,
        &["field:vosu.client.pid", "field:vosu.play.combo"],
    );
    let stop = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn({
        let host = host.clone();
        let stop = stop.clone();
        move || {
            let mut switches = 0;
            while !stop.load(Ordering::Acquire) {
                foreground.set(Some(if switches % 2 == 0 { 8896 } else { 8895 }));
                host.foreground_changed();
                switches += 1;
                if switches == 1 {
                    started_tx.send(()).unwrap();
                }
            }
            done_tx.send(switches).unwrap();
        }
    });
    started_rx.recv_timeout(RECV_TIMEOUT).unwrap();
    let selectors = [
        selector("vosu.client", "/pid"),
        selector("vosu.play", "/combo"),
    ];
    for _ in 0..256 {
        let capture = api.capture(SessionTarget::Focused, &selectors).unwrap();
        let pair = (&capture.values[0].value, &capture.values[1].value);
        assert!(
            pair == (
                &Some(CborValue::Integer(8895.into())),
                &Some(CborValue::Integer(51.into()))
            ) || pair
                == (
                    &Some(CborValue::Integer(8896.into())),
                    &Some(CborValue::Integer(96.into()))
                )
        );
    }
    stop.store(true, Ordering::Release);
    assert!(done_rx.recv_timeout(RECV_TIMEOUT).unwrap() > 0);
    worker.join().unwrap();
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn priority_selected_session_does_not_hide_nonselected_foreground() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let foreground = Arc::new(SyntheticForegroundSource::new(Some(8898)));
    let (_logs, host) = test_host_with(rt.handle().clone(), foreground.clone(), RECV_TIMEOUT);
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(8898),
        "test.p0.priority",
        &["p0-manager", "p0-spectator"],
        SessionPluginConfig {
            focus_priority: true,
            on_attach: OnAttach::Identify,
            ..Default::default()
        },
    );
    host.process_appeared(test_process(8898, "p0-manager"));
    let SessionEvent::Attached(manager) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("manager attach")
    };
    assert!(matches!(
        events.recv_timeout(RECV_TIMEOUT).unwrap(),
        SessionEvent::Focus(_)
    ));
    host.process_appeared(test_process(8899, "p0-spectator"));
    let SessionEvent::Attached(spectator) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("spectator attach")
    };
    assert!(matches!(
        events.recv_timeout(RECV_TIMEOUT).unwrap(),
        SessionEvent::Focus(_)
    ));
    let api = reader(&host, "test.p0.foreground-reader", 8900, &[]);
    foreground.set(Some(8899));
    host.foreground_changed();
    let capture = api.capture(SessionTarget::Focused, &[]).unwrap();
    assert_eq!(capture.session.as_ref().unwrap().session, manager.session);
    assert_eq!(capture.session.as_ref().unwrap().foreground, Some(false));
    assert!(capture.roster.sessions.iter().any(|session| session.session == spectator.session && session.foreground == Some(true)));
    assert!(
        capture
            .roster
            .sessions
            .iter()
            .any(|session| session.foreground == Some(true))
    );
    host.destroy_all(RECV_TIMEOUT);
}

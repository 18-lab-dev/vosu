use super::*;
use crate::state::{ClientFacts, PublisherGrant};
use vosu_plugin_api::{CborValue, Selector};
use vosu_surface::state::{SessionTarget, SurfaceError, SurfaceSubscription};

fn selector(ns: &str, pointer: &str) -> Selector {
    Selector {
        ns: ns.into(),
        pointer: pointer.into(),
    }
}
fn changed(rt: &tokio::runtime::Runtime, subscription: &mut SurfaceSubscription) {
    rt.block_on(async {
        tokio::time::timeout(RECV_TIMEOUT, subscription.updates.changed())
            .await
            .unwrap()
            .unwrap();
    });
}

#[test]
fn surface_subscription_creates_owner_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k7.demand";
    let handle = publisher_instance(&host, id, 8701, 8701);
    let subscription = host
        .surface_host()
        .subscribe(
            id,
            SessionTarget::Focused,
            vec![selector("vosu.play", "/combo")],
        )
        .unwrap();
    let demand = host
        .state()
        .subscriptions
        .lock()
        .unwrap()
        .demand(SessionKey(handle), Some("play"));
    assert!(demand.regular);
    assert_eq!(demand.selectors.len(), 1);
    assert_eq!(
        subscription.updates.borrow().values[0].value,
        Some(CborValue::Null)
    );
    drop(subscription);
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
fn surface_read_creates_no_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k7.read";
    let handle = publisher_instance(&host, id, 8702, 8702);
    let entry = host
        .surface_host()
        .read(id, SessionTarget::Focused, &selector("vosu.client", "/pid"))
        .unwrap();
    assert_eq!(entry.value, Some(CborValue::Integer(8702.into())));
    assert!(
        host.state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    );
    assert_eq!(
        host.surface_host().read(
            "unknown",
            SessionTarget::Focused,
            &selector("vosu.client", "")
        ),
        Err(SurfaceError::UnknownReader)
    );
    assert_eq!(
        host.surface_host().read(
            id,
            SessionTarget::Id("unknown".into()),
            &selector("vosu.client", "")
        ),
        Err(SurfaceError::UnknownSession)
    );
    assert!(matches!(
        host.surface_host()
            .read(id, SessionTarget::Focused, &selector("vosu.client", "/~2")),
        Err(SurfaceError::InvalidSelector(_))
    ));
    assert!(matches!(
        host.surface_host().subscribe(
            id,
            SessionTarget::Focused,
            vec![selector("vosu.client", ""); 257]
        ),
        Err(SurfaceError::TooManySelectors)
    ));
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn focused_target_rebinds_on_focus_change() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let fg = Arc::new(SyntheticForegroundSource::new(Some(8703)));
    let (_logs, host) = test_host_with(rt.handle().clone(), fg.clone(), RECV_TIMEOUT);
    let id = "test.k7.focus-a";
    let first = publisher_instance(&host, id, 8703, 8703);
    let api = host.surface_host();
    let mut subscription = api
        .subscribe(
            id,
            SessionTarget::Focused,
            vec![selector("vosu.client", "/pid")],
        )
        .unwrap();
    let old_session = subscription.updates.borrow().session.clone();
    let mut fixed = api
        .subscribe(
            id,
            SessionTarget::Id(old_session.clone().unwrap()),
            vec![selector("vosu.client", "/pid")],
        )
        .unwrap();
    fg.set(Some(8704));
    let second = publisher_instance(&host, "test.k7.focus-b", 8704, 8704);
    changed(&rt, &mut subscription);
    assert_ne!(subscription.updates.borrow().session, old_session);
    assert_eq!(
        subscription.updates.borrow().values[0].value,
        Some(CborValue::Integer(8704.into()))
    );
    assert_eq!(fixed.updates.borrow().session, old_session);
    drop(fixed.updates.borrow_and_update());
    host.process_exited(8703);
    changed(&rt, &mut fixed);
    assert_eq!(fixed.updates.borrow().session, None);
    assert_eq!(fixed.updates.borrow().values[0].value, None);
    assert!(
        host.state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(first), None)
            .selectors
            .is_empty()
    );
    drop(subscription);
    assert!(
        host.state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(second), None)
            .selectors
            .is_empty()
    );
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn surface_reader_without_grant_never_sees_internal_and_granted_reader_sees_internal_paths() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k7.internal";
    let handle = publisher_instance(&host, id, 8705, 8705);
    host.state()
        .registry
        .write()
        .unwrap()
        .register(
            "vosu.osu",
            "field:vosu.osu",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/v2/schemas/vosu.osu.schema.json"
            )),
        )
        .unwrap();
    let manifest = vosu_plugin_api::PluginManifest::parse("kind='plugin'\nmanifest_version=1\nid='vosu.osu'\nname='osu'\nversion='1.0.0'\nhost='>=1.0.0'\nabi_version=1\nruntime='in-process'\n[[provides]]\nid='field:vosu.osu'\nversion='1.2.0'\nschema='osu.json'\n").unwrap();
    let grant = PublisherGrant::from_manifest(&manifest).unwrap();
    host.state().store.close_session(SessionKey(handle));
    host.state().store.open_session(
        SessionKey(handle),
        &grant,
        ClientFacts {
            game: "test".into(),
            variant: "test".into(),
            name: "test".into(),
            version: "1".into(),
            pid: 8705,
            session: "internal-session".into(),
            plugin_id: "vosu.osu".into(),
            plugin_version: "1".into(),
        },
        true,
    );
    let outcome = host
        .state()
        .store
        .commit(
            &grant,
            SessionKey(handle),
            [vosu_plugin_api::PublishFieldRef {
                handle,
                ns: "vosu.osu",
                pointer: "/internal/paths/stable",
                value: PublishValueRef::String("private-path"),
            }]
            .into_iter(),
        )
        .unwrap();
    host.surface_committed(&outcome);
    let api = host.surface_host();
    let internal = selector("vosu.osu", "/internal/paths/stable");
    assert_eq!(
        api.read(id, SessionTarget::Focused, &internal)
            .unwrap()
            .value,
        None
    );
    let granted = "test.k7.granted";
    let manifest = session_manifest(granted, &[granted]).replace(
        "[match]",
        "[[requires]]\nid='field:vosu.osu.internal.paths.stable'\nversion='*'\n[match]",
    );
    publisher::register_manifest(granted, &manifest);
    let _events = publisher::register(granted);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &publisher::manifest(granted),
        settings_schema: None,
        namespace_schemas: &[],
        vtable: publisher::vosu_plugin_vtable,
    }])
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(8706),
        &loaded[0],
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    let mut subscription = api
        .subscribe(granted, SessionTarget::Focused, vec![internal.clone()])
        .unwrap();
    assert_eq!(
        subscription.updates.borrow().values[0].value,
        Some(CborValue::Text("private-path".into()))
    );
    assert_eq!(
        api.read(granted, SessionTarget::Focused, &internal)
            .unwrap()
            .value,
        Some(CborValue::Text("private-path".into()))
    );
    assert_eq!(
        api.read(
            granted,
            SessionTarget::Focused,
            &selector("vosu.osu", "/internal/paths/lazer")
        )
        .unwrap()
        .value,
        None
    );
    let outcome = host
        .state()
        .store
        .commit(
            &grant,
            SessionKey(handle),
            [vosu_plugin_api::PublishFieldRef {
                handle,
                ns: "vosu.osu",
                pointer: "/internal/paths/stable",
                value: PublishValueRef::String("new-private-path"),
            }]
            .into_iter(),
        )
        .unwrap();
    host.surface_committed(&outcome);
    changed(&rt, &mut subscription);
    assert_eq!(
        subscription.updates.borrow().values[0].value,
        Some(CborValue::Text("new-private-path".into()))
    );
    host.destroy_all(RECV_TIMEOUT);
    assert!(subscription.updates.has_changed().is_err());
}

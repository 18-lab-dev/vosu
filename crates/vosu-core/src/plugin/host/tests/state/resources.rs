use super::*;
use crate::resources::RegistryError;
use vosu_plugin_api::{CborValue, ResourceRef, encode};

#[test]
fn abi_reference_publication_and_detach_update_membership_without_resolving() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.r1.abi-reference";
    let manifest = format!(
        "{}\n[[resource_kinds]]\nkind='audio'\nfamilies=['audio']\n",
        session_manifest(id, &[id])
    );
    publisher::register_manifest(id, &manifest);
    let events = publisher::register(id);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &publisher::manifest(id),
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: publisher::vosu_plugin_vtable,
    }])
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(18731),
        &loaded[0],
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    host.process_appeared(test_process(18731, id));
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    let handle = loop {
        match events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            publisher::PublisherEvent::Attached { handle } => break handle,
            publisher::PublisherEvent::Demand(_) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    };
    let reference = encode(&ResourceRef {
        kind: "audio".into(),
        key: CborValue::Integer(43.into()),
        name: "fixture.ogg".into(),
        content_id: CborValue::Text("identity-57".into()),
    })
    .unwrap();
    assert_eq!(
        send(
            id,
            &batch(
                handle,
                &[(
                    "vosu.chart",
                    "/resources/audio",
                    PublishValueRef::ResourceRef(&reference)
                )]
            )
        ),
        vosu_status::VOSU_OK
    );
    let state = host.state().store.current(SessionKey(handle)).unwrap();
    let Some(StateValue::String(url)) = state.get(
        "vosu.chart",
        &Pointer::parse("/resources/audio/url").unwrap(),
    ) else {
        panic!("missing projected resource")
    };
    let token = url.rsplit('/').next().unwrap();
    let resources = &host.state().store.resources;
    {
        let registry = resources.registry.lock().unwrap();
        let identity = registry.lookup(&resources.secret, token, &[id]).unwrap();
        assert_eq!(identity.media_type, "audio/ogg");
        assert_eq!(identity.reference.key, CborValue::Integer(43.into()));
        assert_eq!(registry.stats().bindings, 1);
    }
    // The publisher fixture has no resource resolver: successful publication is
    // already a negative control for accidental eager resolution.
    host.process_exited(18731);
    assert!(matches!(
        resources
            .registry
            .lock()
            .unwrap()
            .lookup(&resources.secret, token, &[id]),
        Err(RegistryError::Gone)
    ));
    host.destroy_all(Duration::from_secs(5));
}

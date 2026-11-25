use super::commit::{KEY, field, grant, store};
use crate::{resources::RegistryError, state::*};
use std::sync::Arc;
use vosu_plugin_api::{CborValue, PublishValueRef, ResourceRef, encode};

fn reference(kind: &str, name: &str) -> Vec<u8> {
    encode(&ResourceRef {
        kind: kind.into(),
        key: CborValue::Text("opaque-key-17".into()),
        name: name.into(),
        content_id: CborValue::Text("content-29".into()),
    })
    .unwrap()
}
fn publisher() -> PublisherGrant {
    grant(
        "test.reference",
        "\"field:vosu.chart\",\"field:vosu.skin\"",
        r#"
[[provides]]
id='resource:test.reference.custom'
version='1.0.0'
[[resource_kinds]]
kind='audio'
families=['audio']
[[resource_kinds]]
kind='set'
tree=true
[[resource_kinds]]
kind='skin'
tree=true
[[resource_kinds]]
kind='custom'
families=['image']
"#,
    )
}
fn token(store: &StateStore, ns: &str, path: &str) -> String {
    let state = store.current(KEY).unwrap();
    let value = state
        .get(ns, &Pointer::parse(&format!("{path}/url")).unwrap())
        .unwrap();
    let StateValue::String(url) = value else {
        panic!("reference URL")
    };
    url.split('/').nth(4).unwrap().into()
}
fn lookup(store: &StateStore, token: &str) -> Result<&'static str, RegistryError> {
    let registry = store.resources.registry.lock().unwrap();
    registry
        .lookup(&store.resources.secret, token, &["test.reference"])
        .map(|r| r.media_type)
}
#[test]
fn state_reference_publication_projects_fixed_type_and_no_resolver() {
    let grant = publisher();
    let store = store(&grant);
    // No roots, files or plugin dispatcher exist: publication must still work.
    let bytes = reference("audio", "TRACK.MP3");
    store
        .commit_from_instance(
            &grant,
            KEY,
            67,
            [field(
                "vosu.chart",
                "/resources/audio",
                PublishValueRef::ResourceRef(&bytes),
            )]
            .into_iter(),
        )
        .unwrap();
    let id = token(&store, "vosu.chart", "/resources/audio");
    assert_eq!(lookup(&store, &id), Ok("audio/mpeg"));
    let state = store.current(KEY).unwrap();
    assert_eq!(
        state.get(
            "vosu.chart",
            &Pointer::parse("/resources/audio/type").unwrap()
        ),
        Some(&StateValue::String(Arc::from("audio/mpeg")))
    );
    assert_eq!(store.resources.registry.lock().unwrap().stats().bindings, 1);
}
#[test]
fn state_reference_failed_final_entry_and_budget_are_atomic() {
    let grant = publisher();
    let store = store(&grant);
    let bytes = reference("audio", "song.mp3");
    let old = store.current(KEY).unwrap();
    assert!(
        store
            .commit(
                &grant,
                KEY,
                [
                    field(
                        "vosu.chart",
                        "/resources/audio",
                        PublishValueRef::ResourceRef(&bytes)
                    ),
                    field("test.reference", "invalid", PublishValueRef::Bool(true)),
                ]
                .into_iter()
            )
            .is_err()
    );
    assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
    assert_eq!(store.resources.registry.lock().unwrap().stats().records, 0);
    let oversized = "x".repeat(4 * 1024 * 1024);
    assert!(matches!(
        store.commit(
            &grant,
            KEY,
            [
                field(
                    "vosu.chart",
                    "/resources/audio",
                    PublishValueRef::ResourceRef(&bytes)
                ),
                field(
                    "test.reference",
                    "/huge",
                    PublishValueRef::String(&oversized)
                ),
            ]
            .into_iter()
        ),
        Err(CommitError::DocumentBudget)
    ));
    assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
    assert_eq!(store.resources.registry.lock().unwrap().stats().bindings, 0);
}
#[test]
fn state_reference_class_kind_and_location_are_enforced() {
    let grant = publisher();
    let store = store(&grant);
    let audio = reference("audio", "song.mp3");
    let unknown = reference("unknown", "song.mp3");
    for entry in [
        field(
            "vosu.chart",
            "/resources/audio",
            PublishValueRef::TreeRef(&audio),
        ),
        field(
            "vosu.chart",
            "/resources/audio",
            PublishValueRef::ResourceRef(&unknown),
        ),
        field("vosu.chart", "/title", PublishValueRef::ResourceRef(&audio)),
        field(
            "vosu.chart",
            "/resources/audio/nested",
            PublishValueRef::ResourceRef(&audio),
        ),
    ] {
        assert!(matches!(
            store.commit(&grant, KEY, [entry].into_iter()),
            Err(CommitError::InvalidReference)
        ));
    }
    assert_eq!(store.current(KEY).unwrap().revision, 0);
    assert_eq!(store.resources.registry.lock().unwrap().stats().records, 0);
}
#[test]
fn state_reference_ancestor_null_root_and_detach_retire_bindings() {
    let grant = publisher();
    let store = store(&grant);
    let bytes = reference("custom", "private-sentinel.svg");
    for path in ["/parent", ""] {
        store
            .commit(
                &grant,
                KEY,
                [field(
                    "test.reference",
                    "/parent/ref",
                    PublishValueRef::ResourceRef(&bytes),
                )]
                .into_iter(),
            )
            .unwrap();
        let id = token(&store, "test.reference", "/parent/ref");
        assert_eq!(lookup(&store, &id), Ok("application/octet-stream"));
        store
            .commit(
                &grant,
                KEY,
                [field("test.reference", path, PublishValueRef::Null)].into_iter(),
            )
            .unwrap();
        assert_eq!(lookup(&store, &id), Err(RegistryError::Gone));
    }
    let skin = reference("skin", "skin");
    store
        .commit(
            &grant,
            KEY,
            [field("vosu.skin", "/tree", PublishValueRef::TreeRef(&skin))].into_iter(),
        )
        .unwrap();
    let id = token(&store, "vosu.skin", "/tree");
    assert!(lookup(&store, &id).is_ok());
    store.close_session(KEY).unwrap();
    assert_eq!(lookup(&store, &id), Err(RegistryError::Gone));
    assert_eq!(store.resources.registry.lock().unwrap().stats().bindings, 0);
}
#[test]
fn state_reference_identical_id_survives_one_session_detach() {
    let grant = publisher();
    let store = store(&grant);
    let other = SessionKey(KEY.0 + 1);
    store.open_session(
        other,
        &grant,
        ClientFacts {
            game: "game".into(),
            variant: "variant".into(),
            name: "other".into(),
            version: "2".into(),
            pid: 7359,
            session: "other-session".into(),
            plugin_id: "test.reference".into(),
            plugin_version: "1".into(),
        },
        false,
    );
    let bytes = reference("audio", "shared.mp3");
    let entry = field(
        "vosu.chart",
        "/resources/audio",
        PublishValueRef::ResourceRef(&bytes),
    );
    store
        .commit_from_instance(&grant, KEY, 71, [entry].into_iter())
        .unwrap();
    let id = token(&store, "vosu.chart", "/resources/audio");
    let mut entry = field(
        "vosu.chart",
        "/resources/audio",
        PublishValueRef::ResourceRef(&bytes),
    );
    entry.handle = other.0;
    store
        .commit_from_instance(&grant, other, 72, [entry].into_iter())
        .unwrap();
    assert_eq!(store.resources.registry.lock().unwrap().stats().records, 1);
    assert_eq!(store.resources.registry.lock().unwrap().stats().bindings, 2);
    store.close_session(KEY);
    assert_eq!(lookup(&store, &id), Ok("audio/mpeg"));
    store.close_session(other);
    assert_eq!(lookup(&store, &id), Err(RegistryError::Gone));
}

#[test]
fn state_reference_registry_capacity_rejection_is_atomic() {
    use crate::resources::{RegistryLimits, ResourceRegistry, ResourceSecret};
    let grant = publisher();
    let mut store = store(&grant);
    assert!(!store.resources.is_persistent());
    store.resources = Arc::new(ReferenceStore::new(
        ResourceSecret::ephemeral().unwrap(),
        ResourceRegistry::new(RegistryLimits {
            records: 0,
            metadata_bytes: 1024,
        }),
    ));
    let bytes = reference("audio", "song.mp3");
    let previous = store.current(KEY).unwrap();
    assert!(matches!(
        store.commit(
            &grant,
            KEY,
            [
                field("test.reference", "/valid", PublishValueRef::I64(31)),
                field(
                    "vosu.chart",
                    "/resources/audio",
                    PublishValueRef::ResourceRef(&bytes)
                ),
            ]
            .into_iter()
        ),
        Err(CommitError::Registry(RegistryError::Capacity))
    ));
    assert!(Arc::ptr_eq(&previous, &store.current(KEY).unwrap()));
    assert_eq!(store.resources.registry.lock().unwrap().stats().records, 0);
}

#[test]
fn state_reference_map_reset_and_skin_null_have_distinct_shapes() {
    let grant = publisher();
    let store = store(&grant);
    let audio = reference("audio", "song.mp3");
    let skin = reference("skin", "skin");
    store
        .commit(
            &grant,
            KEY,
            [
                field(
                    "vosu.chart",
                    "/resources/audio",
                    PublishValueRef::ResourceRef(&audio),
                ),
                field("vosu.skin", "/tree", PublishValueRef::TreeRef(&skin)),
            ]
            .into_iter(),
        )
        .unwrap();
    let audio_id = token(&store, "vosu.chart", "/resources/audio");
    let skin_id = token(&store, "vosu.skin", "/tree");
    store
        .commit(
            &grant,
            KEY,
            [
                field("vosu.chart", "/resources", PublishValueRef::Null),
                field("vosu.skin", "/tree", PublishValueRef::Null),
            ]
            .into_iter(),
        )
        .unwrap();
    assert_eq!(lookup(&store, &audio_id), Err(RegistryError::Gone));
    assert_eq!(lookup(&store, &skin_id), Err(RegistryError::Gone));
    let state = store.current(KEY).unwrap();
    assert!(
        matches!(state.get("vosu.chart", &Pointer::parse("/resources").unwrap()), Some(StateValue::Object(object)) if object.is_empty())
    );
    assert_eq!(
        state.get("vosu.skin", &Pointer::parse("/tree").unwrap()),
        Some(&StateValue::Null)
    );
    let fake = encode(&serde_json::json!({"url":"/api/v1/resources/forged", "type":"audio/mpeg"}))
        .unwrap();
    assert!(matches!(
        store.commit(
            &grant,
            KEY,
            [field(
                "vosu.chart",
                "/resources/audio",
                PublishValueRef::Cbor(&fake)
            ),]
            .into_iter()
        ),
        Err(CommitError::InvalidReference)
    ));
}

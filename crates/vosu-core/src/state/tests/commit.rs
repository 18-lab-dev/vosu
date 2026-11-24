use crate::state::*;
use proptest::prelude::*;
use std::sync::{Arc, RwLock};
use vosu_plugin_api::{PluginManifest, PublishFieldRef, PublishValueRef};

pub(super) const KEY: SessionKey = SessionKey(7301);
pub(super) fn grant(id: &str, covers: &str, kinds: &str) -> PublisherGrant {
    let text = format!(
        r#"kind = "plugin"
manifest_version = 1
id = "{id}"
name = "Commit test"
version = "1.0.0"
host = "*"
abi_version = 1
runtime = "in-process"
covers = [{covers}]
[[provides]]
id = "field:{id}"
version = "1.0.0"
schema = "schema.json"
{kinds}
"#
    );
    PublisherGrant::from_manifest(&PluginManifest::parse(&text).unwrap()).unwrap()
}
pub(super) fn store(grant: &PublisherGrant) -> StateStore {
    let store = StateStore::new(Arc::new(RwLock::new(SchemaRegistry::with_core().unwrap())));
    store.open_session(
        KEY,
        grant,
        ClientFacts {
            game: "osu".into(),
            variant: "stable".into(),
            name: "osu!".into(),
            version: "123".into(),
            pid: 7301,
            session: "test-session".into(),
            plugin_id: grant.plugin_id.to_string(),
            plugin_version: "1.0.0".into(),
        },
        true,
    );
    store
}
pub(super) fn field<'a>(
    ns: &'a str,
    pointer: &'a str,
    value: PublishValueRef<'a>,
) -> PublishFieldRef<'a> {
    PublishFieldRef {
        handle: KEY.0,
        ns,
        pointer,
        value,
    }
}
fn get(store: &StateStore, ns: &str, pointer: &str) -> Option<StateValue> {
    store
        .current(KEY)
        .unwrap()
        .get(ns, &Pointer::parse(pointer).unwrap())
        .cloned()
}

#[test]
fn covered_namespaces_materialize_null_skeleton_and_resources_empty() {
    let grant = grant(
        "test.commit-skeleton",
        "\"field:vosu.chart.title\", \"field:vosu.skin.name\"",
        "",
    );
    let store = store(&grant);
    assert_eq!(get(&store, "vosu.chart", "/title"), Some(StateValue::Null));
    assert_eq!(
        get(&store, "vosu.chart", "/resources"),
        Some(StateValue::Object(Arc::new(StateObject::default())))
    );
    assert_eq!(get(&store, "vosu.skin", "/tree"), Some(StateValue::Null));
    let schemas = SchemaRegistry::with_core().unwrap();
    for ns in ["vosu.chart", "vosu.skin"] {
        for key in &schemas.get(ns).unwrap().required_top_level {
            assert!(
                store
                    .current(KEY)
                    .unwrap()
                    .namespace(ns)
                    .unwrap()
                    .get(&[key])
                    .is_some()
            );
        }
    }
}
#[test]
fn uncovered_namespace_is_absent_and_own_appears_on_first_commit() {
    let grant = grant("test.commit-omission", "", "");
    let store = store(&grant);
    assert!(store.current(KEY).unwrap().namespace("vosu.play").is_none());
    assert!(
        store
            .current(KEY)
            .unwrap()
            .namespace("test.commit-omission")
            .is_none()
    );
    store
        .commit(
            &grant,
            KEY,
            [field("test.commit-omission", "/a", PublishValueRef::I64(1))].into_iter(),
        )
        .unwrap();
    assert_eq!(
        get(&store, "test.commit-omission", "/a"),
        Some(StateValue::I64(1))
    );
}
#[test]
fn vosu_client_has_all_eight_fields() {
    let grant = grant("test.commit-client", "", "");
    let store = store(&grant);
    let state = store.current(KEY).unwrap();
    let StateValue::Object(client) = state.namespace("vosu.client").unwrap() else {
        panic!("client object")
    };
    assert_eq!(client.len(), 8);
    for (key, value) in [
        ("game", "osu"),
        ("variant", "stable"),
        ("name", "osu!"),
        ("version", "123"),
        ("session", "test-session"),
    ] {
        assert_eq!(client.get(key), Some(&StateValue::String(Arc::from(value))));
    }
    assert_eq!(client.get("pid"), Some(&StateValue::U64(7301)));
    assert_eq!(client.get("focused"), Some(&StateValue::Bool(true)));
    assert_eq!(
        state.get("vosu.client", &Pointer::parse("/plugin/id").unwrap()),
        Some(&StateValue::String(Arc::from("test.commit-client")))
    );
    assert_eq!(
        state.get("vosu.client", &Pointer::parse("/plugin/version").unwrap()),
        Some(&StateValue::String(Arc::from("1.0.0")))
    );
    assert_eq!(store.set_focused(KEY, false).unwrap().current.revision, 1);
    assert!(store.set_focused(KEY, false).is_none());
    assert_eq!(
        get(&store, "vosu.client", "/focused"),
        Some(StateValue::Bool(false))
    );
}
#[test]
fn null_at_resource_member_removes_key() {
    let grant = grant(
        "test.commit-resource",
        "",
        "[[resource_kinds]]\nkind = \"audio\"\nfamilies = [\"audio\"]",
    );
    let store = store(&grant);
    for value in [PublishValueRef::String("reference"), PublishValueRef::Null] {
        store
            .commit(
                &grant,
                KEY,
                [field("vosu.chart", "/resources/audio", value)].into_iter(),
            )
            .unwrap();
    }
    assert_eq!(get(&store, "vosu.chart", "/resources/audio"), None);
    store
        .commit(
            &grant,
            KEY,
            [field("vosu.chart", "/resources", PublishValueRef::Null)].into_iter(),
        )
        .unwrap();
    assert_eq!(
        get(&store, "vosu.chart", "/resources"),
        Some(StateValue::Object(Arc::new(StateObject::default())))
    );
}
#[test]
fn intermediate_objects_created_and_arrays_published_whole() {
    let grant = grant("test.commit-descent", "", "");
    let store = store(&grant);
    store
        .commit(
            &grant,
            KEY,
            [field(
                "test.commit-descent",
                "/a/b/c",
                PublishValueRef::I64(7),
            )]
            .into_iter(),
        )
        .unwrap();
    assert_eq!(
        get(&store, "test.commit-descent", "/a/b/c"),
        Some(StateValue::I64(7))
    );
    store
        .commit(
            &grant,
            KEY,
            [field(
                "test.commit-descent",
                "/a",
                PublishValueRef::Cbor(&[0x81, 0x01]),
            )]
            .into_iter(),
        )
        .unwrap();
    let old = store.current(KEY).unwrap();
    let error = store
        .commit(
            &grant,
            KEY,
            [field(
                "test.commit-descent",
                "/a/0",
                PublishValueRef::I64(2),
            )]
            .into_iter(),
        )
        .err()
        .unwrap();
    assert!(matches!(error, CommitError::ArrayDescent(_)));
    assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
}

#[test]
fn refusals_leave_revision_and_tree_unchanged() {
    let grant = grant("test.commit-refusals", "\"field:vosu.chart\"", "");
    let store = store(&grant);
    let cases = [
        (
            field("vosu.client", "/focused", PublishValueRef::Bool(false)),
            "client",
        ),
        (
            field("vosu.sessions", "", PublishValueRef::Null),
            "reserved",
        ),
        (
            field("test.foreign", "/x", PublishValueRef::Null),
            "foreign",
        ),
        (
            field("vosu.play", "/combo", PublishValueRef::I64(1)),
            "uncovered",
        ),
        (
            field("test.commit-refusals", "bad", PublishValueRef::Null),
            "pointer",
        ),
        (
            field(
                "test.commit-refusals",
                "/bad",
                PublishValueRef::Cbor(&[0xff]),
            ),
            "value",
        ),
        (
            field(
                "test.commit-refusals",
                "/ref",
                PublishValueRef::ResourceRef(&[]),
            ),
            "ref",
        ),
        (
            field(
                "test.commit-refusals",
                "/tree",
                PublishValueRef::TreeRef(&[]),
            ),
            "ref",
        ),
        (
            field("vosu.chart", "/resources/audio", PublishValueRef::Null),
            "uncovered",
        ),
        (
            field(
                "vosu.chart",
                "",
                PublishValueRef::Cbor(&[
                    0xa1, 0x69, b'r', b'e', b's', b'o', b'u', b'r', b'c', b'e', b's', 0xa0,
                ]),
            ),
            "uncovered",
        ),
    ];
    for (bad, reason) in cases {
        let old = store.current(KEY).unwrap();
        let error = store
            .commit(
                &grant,
                KEY,
                [
                    field("test.commit-refusals", "/valid", PublishValueRef::I64(9)),
                    bad,
                ]
                .into_iter(),
            )
            .err()
            .unwrap();
        assert!(match reason {
            "client" => matches!(error, CommitError::ClientNamespace),
            "reserved" => matches!(error, CommitError::ReservedNamespace),
            "foreign" => matches!(error, CommitError::ForeignNamespace(_)),
            "uncovered" => matches!(error, CommitError::Uncovered { .. }),
            "pointer" => matches!(error, CommitError::Pointer(_)),
            "value" => matches!(error, CommitError::Value(_)),
            "ref" => matches!(error, CommitError::ResourceRefBeforeM3),
            _ => false,
        });
        assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
        assert_eq!(store.current(KEY).unwrap().revision, 0);
    }
}
#[test]
fn overlaps_handles_ownership_and_scalar_descent_refused() {
    let grant = grant("test.commit-overlap", "", "");
    let store = store(&grant);
    for (a, b) in [("/a", "/a"), ("/a", "/a/b"), ("", "/a")] {
        assert!(matches!(
            store.commit(
                &grant,
                KEY,
                [
                    field("test.commit-overlap", a, PublishValueRef::Null),
                    field("test.commit-overlap", b, PublishValueRef::Null)
                ]
                .into_iter()
            ),
            Err(CommitError::Overlap { .. })
        ));
        assert_eq!(store.current(KEY).unwrap().revision, 0);
    }
    let foreign = self::grant("test.commit-other-owner", "", "");
    assert!(matches!(
        store.commit(&foreign, KEY, [].into_iter()),
        Err(CommitError::NotOwner)
    ));
    assert_eq!(store.current(KEY).unwrap().revision, 0);
    let mut wrong = field("test.commit-overlap", "/x", PublishValueRef::Null);
    wrong.handle += 1;
    assert!(matches!(
        store.commit(&grant, KEY, [wrong].into_iter()),
        Err(CommitError::UnknownSession)
    ));
    assert_eq!(store.current(KEY).unwrap().revision, 0);
    store
        .commit(
            &grant,
            KEY,
            [field(
                "test.commit-overlap",
                "/a",
                PublishValueRef::Bool(true),
            )]
            .into_iter(),
        )
        .unwrap();
    assert!(matches!(
        store.commit(
            &grant,
            KEY,
            [field("test.commit-overlap", "/a/b", PublishValueRef::Null)].into_iter()
        ),
        Err(CommitError::ArrayDescent(_))
    ));
    assert_eq!(store.current(KEY).unwrap().revision, 1);
    let receiver = store.revisions(KEY).unwrap();
    assert_eq!(*receiver.borrow(), 1);
    assert!(store.close_session(KEY).is_some());
    assert!(receiver.has_changed().is_err());
    assert!(store.current(KEY).is_none());
    assert!(matches!(
        store.commit(&grant, KEY, [].into_iter()),
        Err(CommitError::UnknownSession)
    ));
}

#[test]
fn null_intermediate_initializes_but_scalar_returns_invalid_argument() {
    let grant = grant("test.commit-null-descent", "\"field:vosu.chart.title\"", "");
    let store = store(&grant);
    assert_eq!(get(&store, "vosu.chart", "/title"), Some(StateValue::Null));
    store
        .commit(
            &grant,
            KEY,
            [field(
                "vosu.chart",
                "/title/unicode",
                PublishValueRef::String("title"),
            )]
            .into_iter(),
        )
        .unwrap();
    assert_eq!(
        get(&store, "vosu.chart", "/title/unicode"),
        Some(StateValue::String(Arc::from("title")))
    );
    let old = store.current(KEY).unwrap();
    let error = store
        .commit(
            &grant,
            KEY,
            [field(
                "vosu.chart",
                "/title/unicode/child",
                PublishValueRef::Null,
            )]
            .into_iter(),
        )
        .err()
        .unwrap();
    assert_eq!(
        vosu_plugin_api::vosu_status::from(error),
        vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
}

#[test]
fn grant_ancestors_and_reserved_kinds_are_segment_bounded() {
    let grant = grant(
        "test.commit-grants",
        "\"field:vosu.play.combo\"",
        "[[resource_kinds]]\nkind = \"skin\"\ntree = true",
    );
    for pointer in ["/combo", "/combo/current"] {
        assert_eq!(
            grant.permits("vosu.play", &Pointer::parse(pointer).unwrap()),
            Permit::Covered
        );
    }
    for pointer in ["", "/comboExtra", "/accuracy"] {
        assert_eq!(
            grant.permits("vosu.play", &Pointer::parse(pointer).unwrap()),
            Permit::Uncovered
        );
    }
    assert_eq!(
        grant.permits("vosu.skin", &Pointer::parse("/tree").unwrap()),
        Permit::Covered
    );
    assert_eq!(
        grant.permits("vosu.chart", &Pointer::parse("/resources/audio").unwrap()),
        Permit::Uncovered
    );
    assert_eq!(
        grant.covered_core_namespaces(),
        vec![Arc::from("vosu.play"), Arc::from("vosu.skin")]
    );
    assert_eq!(
        vosu_plugin_api::vosu_status::from(CommitError::Detached),
        vosu_plugin_api::vosu_status::VOSU_ERR_TARGET_GONE
    );
    assert_eq!(
        vosu_plugin_api::vosu_status::from(CommitError::NotOwner),
        vosu_plugin_api::vosu_status::VOSU_ERR_PERMISSION_DENIED
    );
    assert_eq!(
        vosu_plugin_api::vosu_status::from(CommitError::NotIdentified),
        vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]
    #[test]
    fn prop_commit_equals_sequential_json_patch(values in prop::collection::vec((0usize..12, any::<i64>()), 1..80)) {
        let grant = grant("test.commit-property", "", "");
        let store = store(&grant);
        let mut expected = serde_json::Map::new();
        for (key, value) in values {
            let pointer = format!("/key{key}");
            store.commit(&grant, KEY, [field("test.commit-property", &pointer, PublishValueRef::I64(value))].into_iter()).unwrap();
            expected.insert(format!("key{key}"), value.into());
            let mut json = Vec::new();
            store.current(KEY).unwrap().namespace("test.commit-property").unwrap().write_json(&mut json);
            prop_assert_eq!(serde_json::from_slice::<serde_json::Value>(&json).unwrap(), serde_json::Value::Object(expected.clone()));
        }
    }
}

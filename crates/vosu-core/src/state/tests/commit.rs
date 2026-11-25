use crate::state::*;
use proptest::prelude::*;
use std::sync::{Arc, RwLock};
use vosu_plugin_api::{PublishFieldRef, PublishValueRef};

pub(super) const KEY: SessionKey = SessionKey(7301);
pub(super) fn grant(id: &str, covers: &str, kinds: &str) -> PublisherGrant {
    let text = format!(
        r#"kind = "plugin"
manifest_version = 1
id = "{id}"
name = "Commit test"
version = "1.0.0"
host='>=2.0.0-alpha.1, <3.0.0'
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
    PublisherGrant::from_manifest(
        &vosu_plugin_api::manifest::PackageManifest::parse(&text)
            .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
            .unwrap(),
    )
    .unwrap()
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

fn audio_reference(store: &StateStore, grant: &PublisherGrant) -> (Vec<u8>, serde_json::Value) {
    let reference = vosu_plugin_api::ResourceRef {
        kind: "audio".into(),
        key: vosu_plugin_api::CborValue::Integer(23.into()),
        name: "track.mp3".into(),
        content_id: vosu_plugin_api::CborValue::Integer(41.into()),
    };
    let encoded = vosu_plugin_api::encode(&reference).unwrap();
    let identity = crate::resources::ReferenceIdentity::new(
        &store.resources.secret,
        grant.plugin_id(),
        crate::resources::ReferenceClass::Resource,
        reference,
    )
    .unwrap();
    (
        encoded,
        serde_json::json!({
            "url": format!("/api/v1/resources/{}", identity.id.as_str()),
            "type": "audio/mpeg",
        }),
    )
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
    let (reference, _) = audio_reference(&store, &grant);
    for value in [
        PublishValueRef::ResourceRef(&reference),
        PublishValueRef::Null,
    ] {
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
                    0xa1, 0x69, b'r', b'e', b's', b'o', b'u', b'r', b'c', b'e', b's', 0xa1, 0x65,
                    b'a', b'u', b'd', b'i', b'o', 0x01,
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
            "ref" => matches!(error, CommitError::InvalidReference),
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
    assert!(matches!(error, CommitError::ArrayDescent(_)));
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
    fn prop_commit_equals_sequential_json_patch(values in prop::collection::vec((0usize..12, any::<i64>(), any::<bool>(), any::<bool>()), 1..60)) {
        let grant = grant("test.commit-property", "", "[[resource_kinds]]\nkind = \"audio\"\nfamilies = [\"audio\"]");
        let store = store(&grant);
        let (reference, projected_reference) = audio_reference(&store, &grant);
        let mut expected = serde_json::json!({});
        let mut resources = serde_json::json!({});
        for (key, value, clear, remove) in values {
            // Whole CBOR groups, null-reset intermediates and independent nested
            // leaves coexist with resource-map insertion/removal in one batch.
            let group = format!("key{key}");
            let pointer = format!("/{group}");
            let nested = format!("/{group}/nested/value");
            let cbor = vosu_plugin_api::encode(&serde_json::json!({"items": [value, null], "text": "a\\\"b"})).unwrap();
            let entries = [
                field("test.commit-property", if clear { &pointer } else { &nested }, if clear { PublishValueRef::Null } else { PublishValueRef::I64(value) }),
                field("test.commit-property", "/subtree", PublishValueRef::Cbor(&cbor)),
                field("vosu.chart", "/resources/audio", if remove { PublishValueRef::Null } else { PublishValueRef::ResourceRef(&reference) }),
            ];
            let old = store.current(KEY).unwrap();
            store.commit(&grant, KEY, entries.into_iter()).unwrap();
            if clear { expected[&group] = serde_json::Value::Null; }
            else {
                if expected.get(&group).is_none_or(serde_json::Value::is_null) { expected[&group] = serde_json::json!({}); }
                expected[&group]["nested"] = serde_json::json!({"value": value});
            }
            expected["subtree"] = serde_json::json!({"items": [value, null], "text": "a\\\"b"});
            if remove { resources.as_object_mut().unwrap().remove("audio"); }
            else { resources["audio"] = projected_reference.clone(); }
            let current = store.current(KEY).unwrap();
            prop_assert_eq!(current.revision, old.revision + 1);
            let mut json = Vec::new();
            current.namespace("test.commit-property").unwrap().write_json(&mut json);
            prop_assert_eq!(serde_json::from_slice::<serde_json::Value>(&json).unwrap(), expected.clone());
            json.clear();
            current.get("vosu.chart", &Pointer::parse("/resources").unwrap()).unwrap().write_json(&mut json);
            prop_assert_eq!(serde_json::from_slice::<serde_json::Value>(&json).unwrap(), resources.clone());
        }
    }
}

#[test]
fn core_roots_preserve_required_reference_skeletons() {
    let grant = grant(
        "test.commit-roots",
        "\"field:vosu.chart\", \"field:vosu.skin\", \"field:vosu.play\"",
        "",
    );
    let store = store(&grant);
    for (ns, field_name) in [("vosu.chart", "resources"), ("vosu.skin", "tree")] {
        for input in [serde_json::json!({}), serde_json::json!({field_name: null})] {
            let encoded = vosu_plugin_api::encode(&input).unwrap();
            let outcome = store
                .commit(
                    &grant,
                    KEY,
                    [field(ns, "", PublishValueRef::Cbor(&encoded))].into_iter(),
                )
                .unwrap();
            assert!(matches!(
                outcome.current.namespace(ns),
                Some(StateValue::Object(_))
            ));
            assert_eq!(
                get(&store, ns, &format!("/{field_name}")),
                Some(if ns == "vosu.chart" {
                    StateValue::Object(Arc::new(StateObject::default()))
                } else {
                    StateValue::Null
                })
            );
        }
        for value in [
            PublishValueRef::Null,
            PublishValueRef::Bool(false),
            PublishValueRef::Cbor(&[0x80]),
        ] {
            let old = store.current(KEY).unwrap();
            assert!(matches!(
                store.commit(&grant, KEY, [field(ns, "", value)].into_iter()),
                Err(CommitError::CoreRoot)
            ));
            assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
        }
    }
}

#[test]
fn every_duplicate_protected_root_member_is_checked() {
    let grant = grant(
        "test.commit-duplicate-root",
        "\"field:vosu.chart\", \"field:vosu.skin\"",
        "[[resource_kinds]]\nkind = \"audio\"\nfamilies = [\"audio\"]",
    );
    let object = |fields: Vec<(&str, StateValue)>| {
        StateValue::Object(Arc::new(StateObject::new(
            fields.into_iter().map(|(k, v)| (Arc::from(k), v)).collect(),
        )))
    };
    // Exercise the root check independently of the CBOR decoder, which must
    // also reject duplicates. The second copy must never evade authority.
    let mut chart = object(vec![
        ("resources", object(vec![("audio", StateValue::Null)])),
        ("resources", object(vec![("background", StateValue::Null)])),
    ]);
    assert!(matches!(
        crate::state::commit::normalize_core_root(&grant, "vosu.chart", &mut chart),
        Err(CommitError::Uncovered { .. })
    ));
    let mut skin = object(vec![
        ("tree", StateValue::Null),
        ("tree", StateValue::Bool(true)),
    ]);
    assert!(matches!(
        crate::state::commit::normalize_core_root(&grant, "vosu.skin", &mut skin),
        Err(CommitError::Uncovered { .. })
    ));
}

#[test]
fn empty_batch_is_a_revision_and_watch_noop() {
    let grant = grant("test.commit-empty", "", "");
    let store = store(&grant);
    let watch = store.revisions(KEY).unwrap();
    let old = store.current(KEY).unwrap();
    let outcome = store.commit(&grant, KEY, [].into_iter()).unwrap();
    assert!(Arc::ptr_eq(&old, &outcome.current));
    assert!(Arc::ptr_eq(&outcome.previous, &outcome.current));
    assert_eq!(outcome.current.revision, 0);
    assert!(outcome.touched.is_empty());
    assert!(!watch.has_changed().unwrap());
}

#[test]
fn ten_thousand_entry_batches_are_bounded_and_keep_wire_key_order() {
    use std::time::{Duration, Instant};
    let grant = grant("test.commit-wide", "", "");
    let store = store(&grant);
    let paths: Vec<_> = (0..10_000).rev().map(|i| format!("/wide/key{i}")).collect();
    let started = Instant::now();
    // Run again against a wide existing object to exercise read indexing too.
    for value in [1, 2] {
        store
            .commit(
                &grant,
                KEY,
                paths
                    .iter()
                    .map(|p| field("test.commit-wide", p, PublishValueRef::I64(value))),
            )
            .unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    let state = store.current(KEY).unwrap();
    let StateValue::Object(wide) = state
        .get("test.commit-wide", &Pointer::parse("/wide").unwrap())
        .unwrap()
    else {
        panic!("wide object")
    };
    assert_eq!(wide.len(), 10_000);
    for ((key, value), expected) in wide.iter().zip((0..10_000).rev()) {
        assert_eq!(key, format!("key{expected}"));
        assert_eq!(value, &StateValue::I64(2));
    }
}

#[test]
fn namespace_byte_budget_is_atomic_and_replacement_releases_capacity() {
    let grant = grant("test.commit-byte-budget", "", "");
    let store = store(&grant);
    // The JSON escape expansion, not just payload bytes, counts.
    let payload = "\u{1}".repeat(200_000);
    for path in ["/a", "/b", "/c"] {
        store
            .commit(
                &grant,
                KEY,
                [field(
                    "test.commit-byte-budget",
                    path,
                    PublishValueRef::String(&payload),
                )]
                .into_iter(),
            )
            .unwrap();
    }
    let old = store.current(KEY).unwrap();
    let watch = store.revisions(KEY).unwrap();
    let error = store
        .commit(
            &grant,
            KEY,
            [
                field("test.commit-byte-budget", "/valid", PublishValueRef::Null),
                field(
                    "test.commit-byte-budget",
                    "/d",
                    PublishValueRef::String(&payload),
                ),
            ]
            .into_iter(),
        )
        .err()
        .unwrap();
    assert!(matches!(error, CommitError::DocumentBudget));
    assert_eq!(
        vosu_plugin_api::vosu_status::from(error),
        vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
    assert!(!watch.has_changed().unwrap());
    store
        .commit(
            &grant,
            KEY,
            [field(
                "test.commit-byte-budget",
                "/a",
                PublishValueRef::Null,
            )]
            .into_iter(),
        )
        .unwrap();
    store
        .commit(
            &grant,
            KEY,
            [field(
                "test.commit-byte-budget",
                "/d",
                PublishValueRef::String(&payload),
            )]
            .into_iter(),
        )
        .unwrap();
}

#[test]
fn session_key_budget_is_atomic_and_subtree_replacement_releases_keys() {
    let grant = grant("test.commit-key-budget", "", "");
    let store = store(&grant);
    let paths: Vec<_> = (0..10_000).map(|i| format!("/group{i}")).collect();
    for group in 0..6 {
        let paths: Vec<_> = paths.iter().map(|p| format!("/batch{group}{p}")).collect();
        store
            .commit(
                &grant,
                KEY,
                paths
                    .iter()
                    .map(|p| field("test.commit-key-budget", p, PublishValueRef::Null)),
            )
            .unwrap();
    }
    let old = store.current(KEY).unwrap();
    assert!(matches!(
        store.commit(
            &grant,
            KEY,
            paths
                .iter()
                .map(|p| field("test.commit-key-budget", p, PublishValueRef::Null))
        ),
        Err(CommitError::DocumentBudget)
    ));
    assert!(Arc::ptr_eq(&old, &store.current(KEY).unwrap()));
    store
        .commit(
            &grant,
            KEY,
            [field(
                "test.commit-key-budget",
                "/batch0",
                PublishValueRef::Null,
            )]
            .into_iter(),
        )
        .unwrap();
    store
        .commit(
            &grant,
            KEY,
            paths
                .iter()
                .map(|p| field("test.commit-key-budget", p, PublishValueRef::Null)),
        )
        .unwrap();
}

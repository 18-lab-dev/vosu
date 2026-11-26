//! Non-owning publication (`docs/v2/plugin-abi.md` §6.6, 2026-11-26): a
//! plugin publishes only plain values in its own namespace into a session
//! another plugin owns, and that namespace leaves with its provider.
use super::commit::{KEY, field, grant, store};
use crate::state::*;
use std::sync::Arc;
use vosu_plugin_api::PublishValueRef;
use vosu_plugin_api::vosu_status::VOSU_ERR_PERMISSION_DENIED;

const OWNER: &str = "test.guest-owner";
const GUEST: &str = "test.guest-tool";

fn owner() -> PublisherGrant {
    grant(OWNER, "\"field:vosu.play.combo\"", "")
}

fn guest() -> PublisherGrant {
    grant(
        GUEST,
        "\"field:vosu.play.accuracy\"",
        "[[resource_kinds]]\nkind = \"map\"\nfamilies = [\"chart-text\"]\n\
         [[provides]]\nid = \"resource:test.guest-tool.map\"\nversion = \"1.0.0\"",
    )
}

fn value(store: &StateStore, ns: &str, pointer: &str) -> Option<StateValue> {
    store
        .current(KEY)
        .unwrap()
        .get(ns, &Pointer::parse(pointer).unwrap())
        .cloned()
}

#[test]
fn guest_commits_plain_values_into_its_own_namespace_only() {
    let owner = owner();
    let guest = guest();
    let store = store(&owner);
    // The owner path still refuses a non-owner outright.
    assert!(matches!(
        store.commit(
            &guest,
            KEY,
            [field(GUEST, "/value", PublishValueRef::U64(1))].into_iter()
        ),
        Err(CommitError::NotOwner)
    ));
    let outcome = store
        .commit_as_guest(
            &guest,
            KEY,
            0,
            [field(GUEST, "/sections/current", PublishValueRef::U64(3))].into_iter(),
        )
        .unwrap();
    assert_eq!(outcome.current.revision, 1);
    assert_eq!(
        value(&store, GUEST, "/sections/current"),
        Some(StateValue::U64(3))
    );
    let before = store.current(KEY).unwrap();
    // Core, client, reserved, owner and unregistered foreign namespaces are
    // all refused, even a field the guest's own `covers` names.
    for ns in [
        "vosu.play",
        "vosu.client",
        "vosu.sessions",
        OWNER,
        "test.guest-other",
    ] {
        let error = store
            .commit_as_guest(
                &guest,
                KEY,
                0,
                [
                    field(GUEST, "/ok", PublishValueRef::Bool(true)),
                    field(ns, "/accuracy", PublishValueRef::F64(0.5)),
                ]
                .into_iter(),
            )
            .err()
            .unwrap();
        assert!(matches!(&error, CommitError::NotOwnNamespace(refused) if refused == ns));
        assert_eq!(
            vosu_plugin_api::vosu_status::from(error),
            VOSU_ERR_PERMISSION_DENIED
        );
    }
    let reference = vosu_plugin_api::encode(&vosu_plugin_api::ResourceRef {
        kind: "map".into(),
        key: vosu_plugin_api::CborValue::Integer(1.into()),
        name: "map.osu".into(),
        content_id: vosu_plugin_api::CborValue::Integer(2.into()),
    })
    .unwrap();
    let error = store
        .commit_as_guest(
            &guest,
            KEY,
            0,
            [field(
                GUEST,
                "/map",
                PublishValueRef::ResourceRef(&reference),
            )]
            .into_iter(),
        )
        .err()
        .unwrap();
    assert!(matches!(error, CommitError::GuestReference));
    assert_eq!(
        vosu_plugin_api::vosu_status::from(error),
        VOSU_ERR_PERMISSION_DENIED
    );
    assert!(Arc::ptr_eq(&before, &store.current(KEY).unwrap()));
}

#[test]
fn guest_commits_share_the_session_budgets() {
    let owner = owner();
    let guest = guest();
    let store = store(&owner);
    let large = "x".repeat(4 * 1024 * 1024);
    let error = store
        .commit_as_guest(
            &guest,
            KEY,
            0,
            [field(GUEST, "/blob", PublishValueRef::String(&large))].into_iter(),
        )
        .err()
        .unwrap();
    assert!(matches!(error, CommitError::DocumentBudget));
    assert_eq!(store.current(KEY).unwrap().revision, 0);
}

#[test]
fn closed_sessions_refuse_guests_and_provider_loss_clears_the_namespace() {
    let owner = owner();
    let guest = guest();
    let store = store(&owner);
    store
        .commit(
            &owner,
            KEY,
            [field("vosu.play", "/combo", PublishValueRef::U64(5))].into_iter(),
        )
        .unwrap();
    store
        .commit_as_guest(
            &guest,
            KEY,
            0,
            [field(GUEST, "/timeline", PublishValueRef::U64(8))].into_iter(),
        )
        .unwrap();
    let revisions = store.revisions(KEY).unwrap();
    // Clearing the owner's id never touches the session the owner holds.
    assert!(store.clear_guest_namespace(OWNER).is_empty());
    let cleared = store.clear_guest_namespace(GUEST);
    assert_eq!(cleared.len(), 1);
    assert_eq!(cleared[0].current.revision, 3);
    assert_eq!(
        cleared[0].touched,
        vec![(Arc::from(GUEST), Pointer::root())]
    );
    assert!(store.current(KEY).unwrap().namespace(GUEST).is_none());
    assert_eq!(*revisions.borrow(), 3);
    assert_eq!(
        value(&store, "vosu.play", "/combo"),
        Some(StateValue::U64(5))
    );
    assert!(store.clear_guest_namespace(GUEST).is_empty());
    // The namespace's accounting left with it: a publish 9 bytes under the
    // 4 MiB budget fits again; stale `{"timeline":8}` usage would not.
    let near = "y".repeat(4 * 1024 * 1024 - 20);
    store
        .commit_as_guest(
            &guest,
            KEY,
            0,
            [field(GUEST, "/blob", PublishValueRef::String(&near))].into_iter(),
        )
        .unwrap();
    assert!(store.close_session(KEY).is_some());
    let error = store
        .commit_as_guest(
            &guest,
            KEY,
            0,
            [field(GUEST, "/timeline", PublishValueRef::U64(9))].into_iter(),
        )
        .err()
        .unwrap();
    assert!(matches!(error, CommitError::UnknownSession));
    assert!(store.clear_guest_namespace(GUEST).is_empty());
}

#[test]
fn namespace_demand_reports_only_that_namespace() {
    let mut schemas = SchemaRegistry::with_core().unwrap();
    schemas
        .register(GUEST, GUEST, r#"{"title":"test.guest-tool","version":"1.0.0","type":"object","properties":{"value":{"type":"integer","x-sampling-lane":{"lane":"precise","activity":"always"}},"slow":{"type":"integer"}}}"#)
        .unwrap();
    let mut subscriptions = SubscriptionRegistry::default();
    subscriptions
        .replace(
            &schemas,
            SubscriberId::Api(1),
            KEY,
            ["/slow", "/value"]
                .into_iter()
                .map(|p| (Arc::from(GUEST), Pointer::parse(p).unwrap()))
                .chain([(Arc::from("vosu.play"), Pointer::parse("/combo").unwrap())])
                .collect(),
            InternalGrant::default(),
        )
        .unwrap();
    subscriptions
        .replace(
            &schemas,
            SubscriberId::Plugin(2),
            KEY,
            vec![(Arc::from(GUEST), Pointer::parse("/slow").unwrap())],
            InternalGrant::default(),
        )
        .unwrap();
    let own = subscriptions.namespace_demand(KEY, None, GUEST);
    assert_eq!(
        own.selectors,
        vec![
            (Arc::from(GUEST), Arc::from("/slow")),
            (Arc::from(GUEST), Arc::from("/value")),
        ]
    );
    assert!(own.precise && !own.regular);
    // Only the unannotated field remains: no acceleration lane.
    subscriptions
        .replace(
            &schemas,
            SubscriberId::Api(1),
            KEY,
            vec![(Arc::from("vosu.play"), Pointer::parse("/combo").unwrap())],
            InternalGrant::default(),
        )
        .unwrap();
    let own = subscriptions.namespace_demand(KEY, Some("play"), GUEST);
    assert_eq!(own.selectors, vec![(Arc::from(GUEST), Arc::from("/slow"))]);
    assert!(!own.precise && !own.regular);
    assert_eq!(
        subscriptions.demand(KEY, None).selectors.len(),
        2,
        "the owner's complete demand is unchanged"
    );
    assert_eq!(
        subscriptions.namespace_demand(KEY, None, "test.guest-none"),
        DemandSnapshot::default()
    );
}

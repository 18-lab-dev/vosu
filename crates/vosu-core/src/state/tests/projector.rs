//! The D2 projector's acceptance tests (K2; ss:297-339, E5).

use std::borrow::Cow;
use std::sync::Arc;

use crate::state::schema::SchemaRegistry;
use crate::state::{
    Audience, InternalGrant, Pointer, SessionKey, SessionState, StateObject, StateValue,
    project_document, project_namespace, project_select,
};

fn str_val(s: &str) -> StateValue {
    StateValue::String(Arc::from(s))
}

fn variant(game: Option<&str>) -> StateValue {
    StateValue::Object(Arc::new(StateObject::new(vec![
        (
            Arc::from("game"),
            game.map(str_val).unwrap_or(StateValue::Null),
        ),
        (Arc::from("songs"), StateValue::Null),
        (Arc::from("skins"), StateValue::Null),
        (Arc::from("replays"), StateValue::Null),
    ])))
}

fn osu_namespace() -> StateValue {
    StateValue::Object(Arc::new(StateObject::new(vec![
        (Arc::from("session"), StateValue::Null),
        (
            Arc::from("internal"),
            StateValue::Object(Arc::new(StateObject::new(vec![(
                Arc::from("paths"),
                StateValue::Object(Arc::new(StateObject::new(vec![
                    (Arc::from("stable"), variant(Some("/home/user/osu"))),
                    (Arc::from("lazer"), StateValue::Null),
                ]))),
            )]))),
        ),
    ])))
}

fn client_namespace() -> StateValue {
    StateValue::Object(Arc::new(StateObject::new(vec![(
        Arc::from("game"),
        str_val("osu"),
    )])))
}

fn session() -> (SchemaRegistry, SessionState) {
    let reg = SchemaRegistry::with_core().unwrap();
    let state = SessionState::new(
        SessionKey(1),
        1,
        vec![
            (Arc::from("vosu.client"), client_namespace()),
            (Arc::from("vosu.osu"), osu_namespace()),
        ],
    );
    (reg, state)
}

#[test]
fn cow_borrows_when_namespace_has_no_hidden_subtree() {
    let (reg, state) = session();
    let client = state.namespace("vosu.client").unwrap();
    let projected = project_namespace(&reg, "vosu.client", client, Audience::Api);
    assert!(matches!(projected, Cow::Borrowed(_)));
    assert_eq!(&*projected, client);
}

#[test]
fn api_projection_never_contains_internal() {
    let (reg, state) = session();
    let osu = state.namespace("vosu.osu").unwrap();

    // Whole document.
    let doc = project_document(&reg, &state, Audience::Api);
    let osu_doc = doc
        .iter()
        .find(|(ns, _)| &**ns == "vosu.osu")
        .unwrap()
        .1
        .clone();
    let StateValue::Object(obj) = &osu_doc else {
        panic!("vosu.osu projects to an object");
    };
    assert!(obj.get("internal").is_none());

    // Whole namespace.
    let projected_ns = project_namespace(&reg, "vosu.osu", osu, Audience::Api);
    let StateValue::Object(obj) = &*projected_ns else {
        panic!("vosu.osu projects to an object");
    };
    assert!(obj.get("internal").is_none());

    // `""` (whole value) and `/internal` selectors.
    let root = Pointer::parse("").unwrap();
    let root_selected = project_select(&reg, &state, "vosu.osu", &root, Audience::Api).unwrap();
    let StateValue::Object(obj) = &*root_selected else {
        panic!("vosu.osu projects to an object");
    };
    assert!(obj.get("internal").is_none());

    let internal_ptr = Pointer::parse("/internal").unwrap();
    assert!(project_select(&reg, &state, "vosu.osu", &internal_ptr, Audience::Api).is_none());

    let leaf_ptr = Pointer::parse("/internal/paths/stable/game").unwrap();
    assert!(project_select(&reg, &state, "vosu.osu", &leaf_ptr, Audience::Api).is_none());

    // The encoding cache.
    let json = state.api_json(&reg);
    assert!(!json.windows(8).any(|w| w == b"internal"));
}

#[test]
fn grant_exposes_only_named_subtree() {
    let (reg, state) = session();
    let osu = state.namespace("vosu.osu").unwrap();
    let grant =
        InternalGrant::from_subtrees(vec![Pointer::parse("/internal/paths/stable").unwrap()]);
    let projected = project_namespace(&reg, "vosu.osu", osu, Audience::Plugin(&grant));
    let StateValue::Object(obj) = &*projected else {
        panic!("vosu.osu projects to an object");
    };
    let StateValue::Object(internal) = obj.get("internal").expect("internal present") else {
        panic!("internal is an object");
    };
    let StateValue::Object(paths) = internal.get("paths").expect("paths present") else {
        panic!("paths is an object");
    };
    assert!(paths.get("stable").is_some());
    assert!(
        paths.get("lazer").is_none(),
        "an ungranted sibling subtree must stay absent"
    );

    let granted_leaf = Pointer::parse("/internal/paths/stable/game").unwrap();
    assert_eq!(
        project_select(
            &reg,
            &state,
            "vosu.osu",
            &granted_leaf,
            Audience::Plugin(&grant)
        )
        .as_deref(),
        Some(&str_val("/home/user/osu"))
    );
    let ungranted_leaf = Pointer::parse("/internal/paths/lazer").unwrap();
    assert!(
        project_select(
            &reg,
            &state,
            "vosu.osu",
            &ungranted_leaf,
            Audience::Plugin(&grant)
        )
        .is_none()
    );
}

#[test]
fn whole_namespace_requirement_does_not_grant_internal() {
    let (reg, state) = session();
    let osu = state.namespace("vosu.osu").unwrap();
    // `field:vosu.osu` resolves to no internal subtree at all (MG).
    let grant = InternalGrant::none();
    let projected = project_namespace(&reg, "vosu.osu", osu, Audience::Plugin(&grant));
    let StateValue::Object(obj) = &*projected else {
        panic!("vosu.osu projects to an object");
    };
    assert!(obj.get("internal").is_none());
}

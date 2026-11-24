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
    let mut reg = SchemaRegistry::with_core().unwrap();
    reg.register_versioned(
        "vosu.osu",
        "vosu.osu",
        &semver::Version::new(1, 2, 0),
        vosu_plugin_api::manifest::schema::SchemaProvenance::TrustedOfficial,
        include_str!("../../../../../docs/v2/schemas/vosu.osu.schema.json"),
    )
    .unwrap();
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

fn manifest_with(entries: &str) -> vosu_plugin_api::PluginManifest {
    let text = format!(
        "kind='plugin'\nmanifest_version=1\nid='acme.tool'\nname='Acme Tool'\n\
         version='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\n\
         runtime='in-process'\n\n{entries}"
    );
    vosu_plugin_api::manifest::PackageManifest::parse(&text)
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .expect("parses")
}

#[test]
fn from_manifest_with_only_whole_namespace_requirement_grants_no_internal() {
    let manifest = manifest_with("[[requires]]\nid='field:vosu.osu'\nversion='^1.0.0'\n");
    let grant = InternalGrant::from_manifest(&manifest);

    let (reg, state) = session();
    let osu = state.namespace("vosu.osu").unwrap();
    let projected = project_namespace(&reg, "vosu.osu", osu, Audience::Plugin(&grant));
    let StateValue::Object(obj) = &*projected else {
        panic!("vosu.osu projects to an object");
    };
    assert!(
        obj.get("internal").is_none(),
        "a whole-namespace field:vosu.osu requirement must not grant internal (MG)"
    );
}

#[test]
fn from_manifest_requires_only_paths_stable_hides_lazer() {
    let manifest = manifest_with(
        "[[requires]]\nid='field:vosu.osu.internal.paths.stable'\nversion='^1.0.0'\n",
    );
    let grant = InternalGrant::from_manifest(&manifest);

    let (reg, state) = session();
    let osu = state.namespace("vosu.osu").unwrap();
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
    assert!(
        paths.get("stable").is_some(),
        "the named subtree is granted"
    );
    assert!(
        paths.get("lazer").is_none(),
        "an ungranted sibling subtree stays absent even though its parent is visible"
    );
}

#[test]
fn from_manifest_optional_entry_grants_exactly_like_requires() {
    // Only `optional`, no `requires` at all: MG draws no distinction
    // between the two lists, so this must grant the same subtree
    // `from_manifest_requires_only_paths_stable_hides_lazer` does through
    // `requires`.
    let manifest = manifest_with(
        "[[optional]]\nid='field:vosu.osu.internal.paths.stable'\nversion='^1.0.0'\n",
    );
    let grant = InternalGrant::from_manifest(&manifest);

    let (reg, state) = session();
    let osu = state.namespace("vosu.osu").unwrap();
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
    assert!(paths.get("lazer").is_none());
}

/// P2 regression: a non-object namespace root used to hit the `.expect` in
/// `project_namespace`/`project_document` through the ancestor-only
/// `Recurse` branch, which treated "not an object" as "hidden" even when
/// the node was never inside the hidden subtree to begin with.
#[test]
fn non_object_namespace_root_does_not_panic() {
    let reg = SchemaRegistry::with_core().unwrap();
    let state = SessionState::new(
        SessionKey(1),
        1,
        vec![(Arc::from("vosu.osu"), StateValue::Null)],
    );
    let osu = state.namespace("vosu.osu").unwrap();

    let projected = project_namespace(&reg, "vosu.osu", osu, Audience::Api);
    assert_eq!(&*projected, &StateValue::Null);

    let doc = project_document(&reg, &state, Audience::Api);
    assert_eq!(doc, vec![(Arc::from("vosu.osu"), StateValue::Null)]);
}

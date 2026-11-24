//! `SessionState`/`ApiEncoding` tests (K2).

use std::sync::Arc;

use crate::state::schema::SchemaRegistry;
use crate::state::{Pointer, SessionKey, SessionState, StateObject, StateValue};

fn client_ns() -> StateValue {
    StateValue::Object(Arc::new(StateObject::new(vec![(
        Arc::from("game"),
        StateValue::String(Arc::from("osu")),
    )])))
}

#[test]
fn namespace_and_get_round_trip() {
    let state = SessionState::new(
        SessionKey(1),
        1,
        vec![(Arc::from("vosu.client"), client_ns())],
    );
    assert!(state.namespace("vosu.client").is_some());
    assert!(state.namespace("vosu.missing").is_none());
    let p = Pointer::parse("/game").unwrap();
    assert_eq!(
        state.get("vosu.client", &p),
        Some(&StateValue::String(Arc::from("osu")))
    );
}

#[test]
fn api_json_is_cached_and_stable() {
    let reg = SchemaRegistry::with_core().unwrap();
    let state = SessionState::new(
        SessionKey(1),
        1,
        vec![(Arc::from("vosu.client"), client_ns())],
    );
    let first = state.api_json(&reg);
    let first_ptr = first.as_ptr();
    assert!(first.starts_with(b"{"));
    // Compares the underlying buffer's address, not just equal bytes: two
    // equal `Bytes` would compare equal even if the JSON were rebuilt (and
    // reallocated) on every call, so this is the assertion that can
    // actually detect a broken cache.
    let second_ptr = state.api_json(&reg).as_ptr();
    assert_eq!(first_ptr, second_ptr);
}

#[test]
fn debug_never_contains_a_raw_value() {
    let osu_ns = StateValue::Object(Arc::new(StateObject::new(vec![(
        Arc::from("internal"),
        StateValue::Object(Arc::new(StateObject::new(vec![(
            Arc::from("paths"),
            StateValue::Object(Arc::new(StateObject::new(vec![(
                Arc::from("stable"),
                StateValue::Object(Arc::new(StateObject::new(vec![(
                    Arc::from("game"),
                    StateValue::String(Arc::from("/home/user/super-secret-install-path")),
                )]))),
            )]))),
        )]))),
    )])));
    let state = SessionState::new(
        SessionKey(1),
        1,
        vec![
            (Arc::from("vosu.client"), client_ns()),
            (Arc::from("vosu.osu"), osu_ns),
        ],
    );
    let rendered = format!("{state:?}");
    assert!(!rendered.contains("super-secret-install-path"));
    assert!(
        rendered.contains("vosu.osu"),
        "namespace names are fine to print"
    );
}

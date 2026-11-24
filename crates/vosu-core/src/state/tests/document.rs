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
    let first = state.api_json(&reg).clone();
    let second = state.api_json(&reg).clone();
    assert_eq!(first, second);
    assert!(first.starts_with(b"{"));
}

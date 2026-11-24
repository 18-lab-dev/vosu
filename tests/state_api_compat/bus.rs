//! Reads the committed bus image that the adapters actually selected.
//!
//! The adapter requester supplies the focused session identity; the host store
//! is read only to prove which source facts reached the bus, never to build wire.
use super::oracle::{Input, Replay};
use serde_json::Value;
use vosu_core::{
    plugin::PluginInstanceId,
    state::{Pointer, SessionKey},
};
use vosu_surface::state::SessionTarget;

/// The focused session as resolved through the upstream adapter's own grants.
pub fn selected(replay: &Replay) -> (u64, String) {
    let host = &replay.fixture.h.host;
    let capture = host
        .surface_requester(PluginInstanceId::new(2))
        .unwrap()
        .capture(SessionTarget::Focused, &[])
        .unwrap();
    let session = capture
        .session
        .expect("a focused published session")
        .session;
    let handle = host
        .diagnostic_sessions()
        .into_iter()
        .find(|candidate| candidate.session == session)
        .unwrap()
        .handle;
    (handle, session)
}

/// One committed fact of the focused session, as JSON (`None` when absent).
pub fn fact(replay: &Replay, ns: &str, pointer: &str) -> Option<Value> {
    let (handle, _) = selected(replay);
    let state = replay
        .fixture
        .h
        .host
        .state()
        .store
        .current(SessionKey(handle))?;
    let value = state.get(ns, &Pointer::parse(pointer).unwrap())?;
    let mut bytes = Vec::new();
    value.write_json(&mut bytes);
    Some(serde_json::from_slice(&bytes).unwrap())
}

/// Proves the replay committed this case's own facts: a nonzero revision, the
/// declared client variant, and the raw status/mode it was given. Returns the
/// committed `vosu.osu` image so callers can require distinct inputs.
pub fn published(replay: &Replay, input: &Input) -> Value {
    let (handle, _) = selected(replay);
    let state = replay
        .fixture
        .h
        .host
        .state()
        .store
        .current(SessionKey(handle))
        .expect("selected session has committed state");
    assert!(state.revision > 0, "{}: nothing was committed", input.id);
    assert_eq!(
        fact(replay, "vosu.client", "/variant"),
        Some(Value::from(input.source.clone().unwrap())),
        "{}",
        input.id
    );
    assert_eq!(
        fact(replay, "vosu.osu", "/session/state/number"),
        Some(Value::from(input.status)),
        "{}",
        input.id
    );
    assert_eq!(
        fact(replay, "vosu.osu", "/beatmap/selectionMode"),
        Some(Value::from(input.mode)),
        "{}",
        input.id
    );
    fact(replay, "vosu.osu", "").unwrap()
}

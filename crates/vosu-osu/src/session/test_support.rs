//! Read-only integration observations; no session is kept alive by this registry.
use std::sync::{Arc, Mutex, Weak};

use super::OsuSession;
use crate::settings::OsuSettings;

static SESSIONS: Mutex<Vec<Weak<OsuSession>>> = Mutex::new(Vec::new());

pub(crate) fn register(session: &Arc<OsuSession>) {
    let mut sessions = SESSIONS.lock().expect("session observations poisoned");
    sessions.retain(|session| session.strong_count() != 0);
    sessions.push(Arc::downgrade(session));
}

/// Counts live session objects, including cancelled objects still being joined.
pub fn live_sessions() -> usize {
    let mut sessions = SESSIONS.lock().expect("session observations poisoned");
    sessions.retain(|session| session.strong_count() != 0);
    sessions.len()
}

/// Effective B2 value actually delivered to this pid's session watch.
pub fn session_settings(pid: u32) -> Option<OsuSettings> {
    SESSIONS
        .lock()
        .expect("session observations poisoned")
        .iter()
        .filter_map(Weak::upgrade)
        .find(|session| session.pid == pid && !session.lifecycle_token().is_cancelled())
        .map(|session| *session.osu_settings.borrow())
}

/// Sampling demand on the exact watch receiver passed to capture.
pub fn session_demand(pid: u32) -> Option<vosu_osu_state::SubscriptionDemand> {
    SESSIONS
        .lock()
        .expect("session observations poisoned")
        .iter()
        .filter_map(Weak::upgrade)
        .find(|session| session.pid == pid && !session.lifecycle.is_cancelled())
        .and_then(|session| {
            session
                .publication
                .get()
                .map(|publication| *publication.demand.receiver().borrow())
        })
}

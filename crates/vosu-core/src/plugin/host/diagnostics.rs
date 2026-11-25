//! Host-only diagnostics. This grants no state, resource or plugin authority.
use super::PluginHost;

#[derive(Clone, Debug)]
pub struct DiagnosticSession {
    pub handle: u64,
    pub session: String,
    pub pid: u32,
    pub client: String,
    pub version: String,
    pub bitness: vosu_plugin_api::Bitness,
    pub state: &'static str,
}

impl PluginHost {
    /// Read-only operational metadata for the daemon's dashboard and reports.
    /// No plugin callback runs under the registry lock. Publication readiness
    /// comes from committed non-client namespaces, not a second lifecycle task.
    pub fn diagnostic_sessions(&self) -> Vec<DiagnosticSession> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .diagnostic_sessions();
        for session in &mut sessions {
            if let Some(state) = self
                .state
                .store
                .current(crate::state::SessionKey(session.handle))
                && state.namespaces().any(|(ns, _)| {
                    ns != "vosu.client"
                        && state
                            .namespace_revision(ns)
                            .is_some_and(|revision| revision != 0)
                })
            {
                session.state = "reading";
            }
        }
        sessions
    }
}

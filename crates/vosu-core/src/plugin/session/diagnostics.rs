use super::*;

impl SessionRegistry {
    pub(crate) fn diagnostic_sessions(&self) -> Vec<crate::plugin::host::DiagnosticSession> {
        use crate::plugin::host::DiagnosticSession;
        let mut sessions: Vec<_> = self
            .identified
            .iter()
            .map(|(handle, record)| DiagnosticSession {
                session: record.public_id.clone(),
                handle: handle.0,
                pid: record.pid,
                client: record.identify.variant.clone(),
                version: record.identify.version.clone(),
                bitness: record.bitness,
                state: "starting reader",
            })
            .chain(
                self.pending
                    .iter()
                    .map(|(handle, record)| DiagnosticSession {
                        session: record.public_id.clone(),
                        handle: handle.0,
                        pid: record.process.pid,
                        client: String::new(),
                        version: String::new(),
                        bitness: record.process.bitness,
                        state: "starting reader",
                    }),
            )
            .collect();
        sessions.sort_by_key(|session| session.pid);
        sessions
    }
}

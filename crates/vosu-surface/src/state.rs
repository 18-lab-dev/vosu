//! Permanent D3 state reads for bundled plugins. The host authorizes `reader`
//! against its loaded bundled manifests; subscriptions create sampling demand.
use std::fmt;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;
use vosu_plugin_api::{Selector, StateEntry};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTarget {
    Id(String),
    Focused,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SurfaceUpdate {
    pub session: Option<String>,
    pub revision: u64,
    /// Full current selection, not a delta.
    pub values: Vec<StateEntry>,
}

/// Identity is a host-issued session id and occurrence, never a process id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifiedSession {
    pub session: String,
    pub generation: u64,
    pub owner: String,
    pub owner_instance: u64,
    pub game: String,
    pub variant: String,
    pub foreground: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentifiedRoster {
    pub revision: u64,
    pub sessions: Arc<[IdentifiedSession]>,
}

#[derive(Clone, PartialEq)]
pub struct CapturedSelection {
    pub session: Option<IdentifiedSession>,
    pub focus_revision: u64,
    pub roster: IdentifiedRoster,
    pub revision: u64,
    pub namespace_revisions: Arc<[(String, u64)]>,
    pub values: Arc<[StateEntry]>,
}

impl fmt::Debug for CapturedSelection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedSelection")
            .field("session", &self.session)
            .field("focus_revision", &self.focus_revision)
            .field("roster_revision", &self.roster.revision)
            .field("namespace_revisions", &self.namespace_revisions)
            .field("selection_count", &self.values.len())
            .finish()
    }
}

pub trait StateReadHost: Send + Sync + 'static {
    fn subscribe(
        &self,
        reader: &str,
        target: SessionTarget,
        selectors: Vec<Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError>;

    /// One-shot reads do not create sampling demand.
    fn read(
        &self,
        reader: &str,
        target: SessionTarget,
        selector: &Selector,
    ) -> Result<StateEntry, SurfaceError>;
}

type ReplaceSelectors = dyn Fn(Vec<Selector>) -> Result<(), SurfaceError> + Send + Sync;

pub struct SurfaceSubscription {
    pub updates: watch::Receiver<Arc<SurfaceUpdate>>,
    _unsubscribe: Box<dyn Send + Sync>,
    replace: Option<Box<ReplaceSelectors>>,
}

impl SurfaceSubscription {
    /// Host implementation seam. `unsubscribe` releases the registration on drop.
    pub fn new(
        updates: watch::Receiver<Arc<SurfaceUpdate>>,
        unsubscribe: impl Send + Sync + 'static,
    ) -> Self {
        Self {
            updates,
            _unsubscribe: Box::new(unsubscribe),
            replace: None,
        }
    }

    pub fn with_replacement(
        mut self,
        replace: impl Fn(Vec<Selector>) -> Result<(), SurfaceError> + Send + Sync + 'static,
    ) -> Self {
        self.replace = Some(Box::new(replace));
        self
    }

    /// Atomic replacement retains other clients' subscriptions on success or error.
    pub fn replace_selectors(&self, selectors: Vec<Selector>) -> Result<(), SurfaceError> {
        self.replace.as_ref().ok_or_else(|| {
            SurfaceError::InvalidSelector("subscription cannot replace selectors".into())
        })?(selectors)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    UnknownReader,
    UnknownSession,
    InvalidSelector(String),
    TooManySelectors,
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownReader => f.write_str("reader is not a loaded bundled plugin"),
            Self::UnknownSession => f.write_str("session is not available"),
            Self::InvalidSelector(reason) => write!(f, "invalid selector: {reason}"),
            Self::TooManySelectors => f.write_str("state subscription capacity reached"),
        }
    }
}
impl std::error::Error for SurfaceError {}

static HOST: Mutex<Option<Arc<dyn StateReadHost>>> = Mutex::new(None);

#[derive(Debug)]
pub struct AlreadyInstalled;
impl fmt::Display for AlreadyInstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a state read host is already installed")
    }
}
impl std::error::Error for AlreadyInstalled {}

#[derive(Debug)]
#[must_use = "dropping this uninstalls the state read host"]
pub struct StateHostInstalled(());
impl Drop for StateHostInstalled {
    fn drop(&mut self) {
        // Drop host-owned resources outside the process-wide slot lock.
        let old = HOST.lock().unwrap().take();
        drop(old);
    }
}

pub fn install_state_host(
    host: Arc<dyn StateReadHost>,
) -> Result<StateHostInstalled, AlreadyInstalled> {
    let mut slot = HOST.lock().unwrap();
    if slot.is_some() {
        return Err(AlreadyInstalled);
    }
    *slot = Some(host);
    Ok(StateHostInstalled(()))
}

pub fn state_host() -> Option<Arc<dyn StateReadHost>> {
    HOST.lock().unwrap().clone()
}

#[cfg(test)]
mod tests;

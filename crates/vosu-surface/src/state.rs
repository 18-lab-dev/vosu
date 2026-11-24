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

pub struct SurfaceSubscription {
    pub updates: watch::Receiver<Arc<SurfaceUpdate>>,
    _unsubscribe: Box<dyn Send + Sync>,
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
        }
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
pub struct StateHostInstalled;
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
    Ok(StateHostInstalled)
}

pub fn state_host() -> Option<Arc<dyn StateReadHost>> {
    HOST.lock().unwrap().clone()
}

#[cfg(test)]
mod tests;

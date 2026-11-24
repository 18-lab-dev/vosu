use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

struct Host;
impl StateReadHost for Host {
    fn subscribe(
        &self,
        _: &str,
        _: SessionTarget,
        _: Vec<Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError> {
        Err(SurfaceError::UnknownReader)
    }
    fn read(&self, _: &str, _: SessionTarget, _: &Selector) -> Result<StateEntry, SurfaceError> {
        Err(SurfaceError::UnknownReader)
    }
}

#[test]
fn state_host_install_is_exclusive_and_drop_uninstalls() {
    let host: Arc<dyn StateReadHost> = Arc::new(Host);
    let installed = install_state_host(host.clone()).unwrap();
    assert!(Arc::ptr_eq(&state_host().unwrap(), &host));
    assert!(install_state_host(Arc::new(Host)).is_err());
    drop(installed);
    assert!(state_host().is_none());
    drop(install_state_host(host).unwrap());
    assert!(state_host().is_none());
}

struct Release(Arc<AtomicBool>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn surface_subscription_drop_releases_registration() {
    let released = Arc::new(AtomicBool::new(false));
    let (_, updates) = watch::channel(Arc::new(SurfaceUpdate {
        session: None,
        revision: 0,
        values: Vec::new(),
    }));
    let subscription = SurfaceSubscription::new(updates, Release(released.clone()));
    assert!(!released.load(Ordering::Acquire));
    drop(subscription);
    assert!(released.load(Ordering::Acquire));
}

//! Opaque, issuer-bound management provenance. Issuers stay in core transport.
use crate::listener::ListenerRole;
use std::sync::Arc;

#[derive(Clone)]
pub struct ManagementProvenance(Arc<()>);

/// Host implementation seam, never passed to bundled providers. A provider
/// can create its own issuer, but its proofs do not validate against core's.
#[derive(Default)]
pub struct ManagementAuthority(Arc<()>);
impl ManagementAuthority {
    pub fn authorize(
        &self,
        listener: ListenerRole,
        supplied_csrf: Option<&str>,
        expected_csrf: &str,
    ) -> Option<ManagementProvenance> {
        if listener != ListenerRole::Dashboard
            || expected_csrf.is_empty()
            || supplied_csrf != Some(expected_csrf)
        {
            return None;
        }
        Some(ManagementProvenance(self.0.clone()))
    }
    pub fn recognizes(&self, proof: &ManagementProvenance) -> bool {
        Arc::ptr_eq(&self.0, &proof.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_dashboard_csrf_mints_local_provenance() {
        let core = ManagementAuthority::default();
        let foreign = ManagementAuthority::default();
        assert!(
            core.authorize(ListenerRole::Overlay, Some("secret"), "secret")
                .is_none()
        );
        assert!(
            core.authorize(ListenerRole::Dashboard, Some("forwarded"), "secret")
                .is_none()
        );
        assert!(
            core.authorize(ListenerRole::Dashboard, None, "secret")
                .is_none()
        );
        let proof = core
            .authorize(ListenerRole::Dashboard, Some("secret"), "secret")
            .unwrap();
        assert!(core.recognizes(&proof));
        assert!(!foreign.recognizes(&proof));
    }
}

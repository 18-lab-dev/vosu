//! Instance-bound authority combining the independent state and route surfaces.
use crate::routes::{RegistrationError, RegistrationLease, RouteDescriptor, RouteProvider};
use crate::state::{
    CapturedSelection, IdentifiedRoster, SessionTarget, SurfaceError, SurfaceSubscription,
};
use std::sync::Arc;
use vosu_plugin_api::Selector;

/// Core binds this object to one live bundled instance. No caller supplies an id.
pub trait BundledRequester:
    crate::files::FilesRequester + crate::resources::ResourceRequester + Send + Sync + 'static
{
    fn capture(
        &self,
        target: SessionTarget,
        selectors: &[Selector],
    ) -> Result<CapturedSelection, SurfaceError>;
    fn capture_roster(&self, game: &str) -> Result<IdentifiedRoster, SurfaceError>;
    fn subscribe(
        &self,
        target: SessionTarget,
        selectors: Vec<Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError>;
    fn register_routes(
        &self,
        routes: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
    ) -> Result<RegistrationLease, RegistrationError>;
}

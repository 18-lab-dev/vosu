//! A non-owning public-state source, independent of HTTP and WebSockets.
use std::sync::Arc;

use super::{ApiError, ApiPlan, ApiSubscription, SessionTarget};
use crate::state::{LaneBits, StateValue};

/// Full selected-value snapshot, never a delta: skipping watch revisions cannot
/// lose updates to distinct keys. `session` changes reset an active baseline,
/// even if all selected values compare equal. `ended` is terminal and sticky.
#[derive(Clone, Debug)]
pub struct ApiUpdate {
    pub session: Option<String>,
    /// Source notification sequence, independent of session-state revisions.
    /// The transport hub owns its separate wire sequence.
    pub revision: u64,
    /// Changes on every active rebinding, even if a slow consumer skips a
    /// switch away and back to the same session. Compare this for baseline resets.
    pub binding: u64,
    pub values: Vec<Option<StateValue>>,
    pub lanes: LaneBits,
    pub ended: bool,
    pub error: Option<ApiError>,
}

pub(crate) trait ApiStateHost: Send + Sync {
    fn sessions(&self) -> Result<Vec<StateValue>, ApiError>;
    fn session(&self, target: &SessionTarget) -> Result<StateValue, ApiError>;
    fn read(&self, target: &SessionTarget, plan: &ApiPlan) -> Result<ApiUpdate, ApiError>;
    fn subscribe(
        &self,
        target: Option<SessionTarget>,
        plan: ApiPlan,
    ) -> Result<ApiSubscription, ApiError>;
}

#[derive(Clone)]
pub struct ApiStateSource(pub(crate) Arc<dyn ApiStateHost>);
impl ApiStateSource {
    /// Identified summaries only, exactly the `vosu.client` public shape.
    pub fn sessions(&self) -> Result<Vec<StateValue>, ApiError> {
        self.0.sessions()
    }
    pub fn session(&self, target: &SessionTarget) -> Result<StateValue, ApiError> {
        self.0.session(target)
    }
    /// Select currently committed values; this never creates demand.
    pub fn read(&self, target: &SessionTarget, plan: &ApiPlan) -> Result<ApiUpdate, ApiError> {
        self.0.read(target, plan)
    }
    pub fn subscribe(
        &self,
        target: SessionTarget,
        plan: ApiPlan,
    ) -> Result<ApiSubscription, ApiError> {
        self.0.subscribe(Some(target), plan)
    }
    pub fn subscribe_sessions(&self, plan: ApiPlan) -> Result<ApiSubscription, ApiError> {
        self.0.subscribe(None, plan)
    }
}

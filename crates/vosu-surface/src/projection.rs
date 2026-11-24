//! Typed adapter projections consumed by the transport-independent hub.
//! Wire filter and byte types are parameters so this boundary has no wire-crate dependency.
use crate::state::CapturedSelection;
use std::sync::Arc;
use vosu_plugin_api::Selector;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionError {
    NotReady,
    Failed(String),
}
impl std::fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => f.write_str("state is not ready"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for ProjectionError {}

pub trait ProjectionView<F, B>: Send + Sync + 'static {
    /// Derived revision captured with this immutable view, not a later worker
    /// observation. Zero denotes projections with no derived inputs.
    fn revision(&self) -> u64 {
        0
    }
    fn encode(&self, filters: &[F]) -> Result<B, String>;
}

pub trait Projection<F, B>: Send + Sync + 'static {
    /// Aggregate images may also contain another wire schema's facts. Ignore
    /// only fields this projection never decodes or emits.
    fn observes_field(&self, _entry: &vosu_plugin_api::StateEntry, _field: &str) -> bool {
        true
    }
    /// Complete read selection. One-shot reads never create demand.
    fn selectors(&self) -> Vec<Selector>;
    /// Sampling selection for precisely this client's wire plan.
    fn demand_selectors(&self, filters: &[F]) -> Vec<Selector>;
    fn decode(
        &self,
        capture: CapturedSelection,
    ) -> Result<Arc<dyn ProjectionView<F, B>>, ProjectionError>;
    /// Additional immutable inputs (for example completed resource calculations).
    fn revision(&self) -> u64 {
        0
    }
    fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }
    /// Retained derived work for the currently selected inputs is still in
    /// flight. A one-shot reader may wait a bounded time on `changes` for it.
    fn pending(&self) -> bool {
        false
    }
    /// Optional owned work retained only by a live consumer of these fields.
    fn retain(&self, _filters: &[F]) -> Option<Box<dyn Send + Sync>> {
        None
    }
}

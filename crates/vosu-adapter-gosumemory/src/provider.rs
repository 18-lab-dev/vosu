//! Unmounted gosuMemory projection and selector-derived sampling demand.
use bytes::Bytes;
use std::sync::{Arc, OnceLock};
use vosu_adapter_support::filtered::{Filter, SharedDocument, selects_path};
use vosu_plugin_api::Selector;
use vosu_surface::{
    projection::{Projection, ProjectionError, ProjectionView},
    state::CapturedSelection,
};

pub struct Provider;
impl Projection<Filter, Bytes> for Provider {
    fn observes_field(&self, entry: &vosu_plugin_api::StateEntry, field: &str) -> bool {
        entry.ns != "vosu.osu"
            || entry.pointer != "/tourney"
            || !matches!(field, "keysOverlay" | "preciseUnstableRate")
    }
    fn selectors(&self) -> Vec<Selector> {
        crate::view::selectors()
    }
    fn demand_selectors(&self, filters: &[Filter]) -> Vec<Selector> {
        self.selectors()
            .into_iter()
            .filter(|selector| match selector.pointer.as_str() {
                "/keys/overlay" => selects_path(filters, &["gameplay", "keyOverlay"]),
                "/play/hitErrors/all" => {
                    selects_path(filters, &["gameplay", "hits", "hitErrorArray"])
                }
                "/tourney" | "/tourneyAvailability" => selects_path(filters, &["tourney"]),
                _ => true,
            })
            .collect()
    }
    fn decode(
        &self,
        capture: CapturedSelection,
    ) -> Result<Arc<dyn ProjectionView<Filter, Bytes>>, ProjectionError> {
        crate::view::View::decode(capture)
            .map(|view| {
                Arc::new(Snapshot {
                    view,
                    response: OnceLock::new(),
                }) as Arc<dyn ProjectionView<Filter, Bytes>>
            })
            .map_err(|error| match error {
                crate::Error::NotReady => ProjectionError::NotReady,
                other => ProjectionError::Failed(other.to_string()),
            })
    }
}
struct Snapshot {
    view: crate::view::View,
    response: OnceLock<Result<crate::model::Response, String>>,
}
impl ProjectionView<Filter, Bytes> for Snapshot {
    fn encode(&self, filters: &[Filter]) -> Result<Bytes, String> {
        if filters.is_empty() {
            crate::encode(&self.view).map_err(|e| e.to_string())
        } else {
            // Cold DTO assembly happens once for this immutable generation,
            // never once per filtered client or distinct filter plan.
            let response = self
                .response
                .get_or_init(|| crate::build(&self.view).map_err(|e| e.to_string()));
            SharedDocument::new(response.as_ref().map_err(Clone::clone)?)
                .encode(filters)
                .map_err(|e| e.to_string())
        }
    }
}

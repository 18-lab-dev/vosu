//! Unmounted upstream providers. The hub owns capture/encoding caches and leases.
use bytes::Bytes;
use std::sync::Arc;
use vosu_adapter_support::filtered::{Filter, selects_path};
use vosu_plugin_api::Selector;
use vosu_surface::{
    projection::{Projection, ProjectionError, ProjectionView},
    state::CapturedSelection,
};

pub struct Regular;
pub struct Precise;
impl Projection<Filter, Bytes> for Regular {
    fn observes_field(&self, entry: &vosu_plugin_api::StateEntry, field: &str) -> bool {
        entry.ns != "vosu.osu"
            || entry.pointer != "/tourney"
            || !matches!(field, "keysOverlay" | "preciseUnstableRate")
    }
    fn selectors(&self) -> Vec<Selector> {
        crate::regular::selectors()
    }
    fn demand_selectors(&self, filters: &[Filter]) -> Vec<Selector> {
        self.selectors()
            .into_iter()
            .filter(|selector| regular_demand(selector, filters))
            .collect()
    }
    fn decode(
        &self,
        capture: CapturedSelection,
    ) -> Result<Arc<dyn ProjectionView<Filter, Bytes>>, ProjectionError> {
        crate::regular::View::from_capture(&capture)
            .map(|view| Arc::new(view) as Arc<dyn ProjectionView<Filter, Bytes>>)
            .map_err(projection_error)
    }
}
impl ProjectionView<Filter, Bytes> for crate::regular::View {
    fn encode(&self, filters: &[Filter]) -> Result<Bytes, String> {
        if filters.is_empty() {
            crate::regular::encode(self).map_err(|e| e.to_string())
        } else {
            crate::regular::filtered(self)
                .encode(filters)
                .map_err(|e| e.to_string())
        }
    }
}
impl Projection<Filter, Bytes> for Precise {
    fn observes_field(&self, entry: &vosu_plugin_api::StateEntry, field: &str) -> bool {
        entry.ns != "vosu.osu"
            || entry.pointer != "/tourney"
            || matches!(
                field,
                "clients"
                    | "ipcId"
                    | "keysOverlay"
                    | "play"
                    | "hitErrors"
                    | "all"
                    | "k1"
                    | "k2"
                    | "m1"
                    | "m2"
                    | "isPressed"
                    | "count"
            )
    }
    fn selectors(&self) -> Vec<Selector> {
        crate::precise::selectors()
    }
    fn demand_selectors(&self, filters: &[Filter]) -> Vec<Selector> {
        self.selectors()
            .into_iter()
            .filter(|selector| match selector.pointer.as_str() {
                "/keys/overlay" => selects_path(filters, &["keys"]),
                "/play/hitErrors/all" => selects_path(filters, &["hitErrors"]),
                "/tourney" => selects_path(filters, &["tourney"]),
                _ => true,
            })
            .collect()
    }
    fn decode(
        &self,
        capture: CapturedSelection,
    ) -> Result<Arc<dyn ProjectionView<Filter, Bytes>>, ProjectionError> {
        crate::precise::View::from_capture(&capture)
            .map(|view| Arc::new(view) as Arc<dyn ProjectionView<Filter, Bytes>>)
            .map_err(projection_error)
    }
}
impl ProjectionView<Filter, Bytes> for crate::precise::View {
    fn encode(&self, filters: &[Filter]) -> Result<Bytes, String> {
        if filters.is_empty() {
            crate::precise::encode(self).map_err(|e| e.to_string())
        } else {
            crate::precise::filtered(self)
                .encode(filters)
                .map_err(|e| e.to_string())
        }
    }
}

fn projection_error(error: crate::Error) -> ProjectionError {
    match error {
        crate::Error::NotReady => ProjectionError::NotReady,
        other => ProjectionError::Failed(other.to_string()),
    }
}

fn regular_demand(selector: &Selector, filters: &[Filter]) -> bool {
    if selector.ns == "vosu.skin" {
        return selects_path(filters, &["settings", "skin", "name"]);
    }
    match selector.pointer.as_str() {
        "/session/paused" => selects_path(filters, &["game", "paused"]),
        "/session/playTime" => selects_path(filters, &["session", "playTime"]),
        "/session/server" => selects_path(filters, &["server"]),
        "/session/state" => selects_path(filters, &["state"]),
        "/internal/paths" => ["folders", "files", "directPath"]
            .iter()
            .any(|field| selects_path(filters, &[field])),
        "/tourneyAvailability" => selects_path(filters, &["tourney"]),
        "/result" => selects_path(filters, &["resultsScreen"]),
        "/play/hitErrors/all" => selects_path(filters, &["play", "hitErrorArray"]),
        "/play/accuracyPercent" => selects_path(filters, &["play", "accuracy"]),
        "/play/healthRaw" => selects_path(filters, &["play", "healthBar"]),
        "/play/judgements" | "/play/statistics" => selects_path(filters, &["play", "hits"]),
        pointer => match pointer.strip_prefix("/play/") {
            Some(field) => selects_path(filters, &["play", field]),
            None => selects_path(filters, &[pointer.trim_start_matches('/')]),
        },
    }
}

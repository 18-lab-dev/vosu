//! Concrete hosts linked into this executable; core owns their lifecycle.
use std::{ffi::OsString, sync::Arc, time::Duration};

use vosu_core::{
    app::StartupFlags,
    runtime::{AdapterRequesters, BundledHosts, HostRoutes, OverlayInputs, Projections},
    server::{routes::StateJsonProvider, service::OverlayComposition, websocket::WebSocketHub},
};
use vosu_osu_reader::discovery::{StartupArgs, StartupValue};
use vosu_surface::{listener::ListenerRole, requester::BundledRequester, tool_view::ToolHostViews};

mod adapters;
#[cfg(test)]
mod lifecycle_owner_tests;
mod overlay;
#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod tests;

pub(crate) struct Hosts;

impl BundledHosts for Hosts {
    fn startup_flags(&self, args: &[OsString]) -> StartupFlags {
        let args = StartupArgs::parse(args.iter().cloned());
        StartupFlags {
            update: args.get("update").and_then(StartupValue::as_boolean),
            onedrive_bypass: args.get("onedrive").and_then(StartupValue::as_boolean) == Some(true),
        }
    }

    fn poll_duration(&self, milliseconds: f64) -> Duration {
        vosu_osu::capture::poll_duration(milliseconds)
    }

    fn projections(&self, requesters: &AdapterRequesters) -> Projections {
        adapters::projections(requesters)
    }

    fn adapter_routes(
        &self,
        state: [Arc<dyn StateJsonProvider>; 4],
        feeds: [WebSocketHub; 4],
        upstream: Arc<dyn BundledRequester>,
    ) -> [HostRoutes; 3] {
        adapters::adapter_routes(state, feeds, upstream)
    }

    fn dashboard_routes(&self, version: &str) -> HostRoutes {
        vosu_dashboard::routes::Routes::new(version).descriptors()
    }

    fn mount_tool_host(
        &self,
        requester: Arc<dyn BundledRequester>,
        views: ToolHostViews,
        roles: &[ListenerRole],
    ) -> Result<Box<dyn Send + Sync>, String> {
        let host = vosu_tool_host::ToolHost::mount(requester, views)
            .map_err(|error| format!("tool host registration failed: {error:?}"))?;
        host.mount_design_system(roles)
            .map_err(|error| format!("design-system registration failed: {error:?}"))?;
        Ok(Box::new(host))
    }

    fn overlay(&self, inputs: OverlayInputs) -> Arc<dyn OverlayComposition> {
        Arc::new(overlay::Overlay::new(inputs))
    }
}

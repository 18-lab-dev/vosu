//! `vosu.toolhost`: web-only tool hosting (A7.1/A7.2). Serves each installed
//! tool's static UI, declared art and password-filtered `_settings`, the v2
//! bridge and the shared design-system assets, entirely from the core-pushed
//! `ToolHostView`. Launch codes, sessions, action authority, tool state and
//! settings writes are core's; this crate never mints or checks a session.
mod assets;
mod plugin;
mod routes;
#[cfg(test)]
mod tests;

pub use plugin::vosu_plugin_vtable;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use vosu_surface::{
    listener::ListenerRole,
    requester::BundledRequester,
    routes::{RegistrationError, RegistrationLease, RouteDescriptor, RoutePath, RouteProvider},
    tool_view::ToolHostViews,
};

pub const MANIFEST: &str = include_str!("../manifest.toml");

struct Shared {
    requester: Arc<dyn BundledRequester>,
    views: ToolHostViews,
    provider: Arc<routes::Routes>,
    leases: Mutex<Leases>,
}
#[derive(Default)]
struct Leases {
    fixed: Option<RegistrationLease>,
    design_system: Option<RegistrationLease>,
    /// One `/tools/<id>` prefix registration per presented tool.
    tools: BTreeMap<String, RegistrationLease>,
    /// Last registration refusal, kept for diagnostics and tests.
    refused: Option<(String, RegistrationError)>,
}

/// The mounted tool host. Dropping it unregisters every route.
pub struct ToolHost {
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

fn descriptor(listener: ListenerRole, method: &str, path: RoutePath) -> RouteDescriptor {
    RouteDescriptor {
        listener,
        method: method.into(),
        path,
        websocket: false,
    }
}

impl ToolHost {
    /// Registers the exact overlay routes and one prefix per presented tool,
    /// then follows every newer view. Must be called inside a Tokio runtime.
    pub fn mount(
        requester: Arc<dyn BundledRequester>,
        views: ToolHostViews,
    ) -> Result<Self, RegistrationError> {
        let provider = Arc::new(routes::Routes {
            views: views.clone(),
        });
        let fixed = [routes::BRIDGE, routes::THEME, routes::DESIGN]
            .into_iter()
            .map(|path| {
                (
                    descriptor(ListenerRole::Overlay, "GET", RoutePath::Exact(path.into())),
                    provider.clone() as Arc<dyn RouteProvider>,
                )
            })
            .collect();
        let fixed = requester.register_routes(fixed)?;
        let shared = Arc::new(Shared {
            requester,
            views,
            provider,
            leases: Mutex::new(Leases {
                fixed: Some(fixed),
                ..Leases::default()
            }),
        });
        shared.reconcile();
        let follower = Arc::clone(&shared);
        let mut changes = shared.views.clone();
        let task = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                follower.reconcile();
            }
        });
        Ok(Self { shared, task })
    }

    /// Takes over the design-system set (`/assets/<name>` and `/favicon.ico`)
    /// on the given listener roles: both, or only the overlay role while the
    /// dashboard is off (A7.3). Core reserves and serves none of these paths.
    pub fn mount_design_system(&self, listeners: &[ListenerRole]) -> Result<(), RegistrationError> {
        let mut leases = self.shared.leases.lock().unwrap();
        if leases.design_system.is_some() {
            return Ok(());
        }
        let routes = listeners
            .iter()
            .copied()
            .flat_map(|listener| {
                assets::DESIGN_SYSTEM.iter().map(move |(path, _)| {
                    descriptor(listener, "GET", RoutePath::Exact((*path).into()))
                })
            })
            .map(|route| {
                (
                    route,
                    self.shared.provider.clone() as Arc<dyn RouteProvider>,
                )
            })
            .collect();
        leases.design_system = Some(self.shared.requester.register_routes(routes)?);
        Ok(())
    }

    /// The most recent per-tool registration refusal (collision or capacity).
    pub fn last_refusal(&self) -> Option<(String, RegistrationError)> {
        self.shared.leases.lock().unwrap().refused.clone()
    }
}

impl Drop for ToolHost {
    fn drop(&mut self) {
        self.task.abort();
        let mut leases = self.shared.leases.lock().unwrap();
        let released = std::mem::take(&mut *leases);
        drop(leases);
        // Unregistration re-enters core; never under this host's lock.
        drop(released);
    }
}

impl Shared {
    /// Registers `/tools/<id>` (GET and HEAD, prefix) for every newly
    /// presented tool and drops the registration of every tool that left the
    /// view. Unchanged tools keep their registration.
    fn reconcile(&self) {
        let ids: Vec<String> = self
            .views
            .borrow()
            .tools
            .iter()
            .map(|tool| tool.id.clone())
            .collect();
        let mut released = Vec::new();
        let mut leases = self.leases.lock().unwrap();
        if leases.fixed.is_none() {
            return;
        }
        let stale: Vec<String> = leases
            .tools
            .keys()
            .filter(|id| !ids.contains(id))
            .cloned()
            .collect();
        for id in stale {
            released.extend(leases.tools.remove(&id));
        }
        for id in ids {
            if leases.tools.contains_key(&id) || !route_segment(&id) {
                continue;
            }
            let path = format!("/tools/{id}");
            let routes = ["GET", "HEAD"]
                .into_iter()
                .map(|method| {
                    (
                        descriptor(
                            ListenerRole::Overlay,
                            method,
                            RoutePath::Prefix(path.clone()),
                        ),
                        self.provider.clone() as Arc<dyn RouteProvider>,
                    )
                })
                .collect();
            match self.requester.register_routes(routes) {
                Ok(lease) => {
                    leases.tools.insert(id, lease);
                }
                Err(error) => leases.refused = Some((id, error)),
            }
        }
        drop(leases);
        drop(released);
    }
}

/// Tool ids core presents are already validated; this only keeps a prefix
/// from ever spanning more than one literal segment.
fn route_segment(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('_')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

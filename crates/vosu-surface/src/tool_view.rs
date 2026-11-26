//! A7.1 `ToolHostView`: the one immutable, latest-only presentation view core
//! pushes to `vosu.toolhost`. It carries presentation data and read-only
//! package leases only: no pathname, session, launch code, credential or
//! status reason, and no `vosu-sys` type.
use crate::files::RootLease;
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::watch;

/// One generation of the installed web tools core decided to present.
pub struct ToolHostView {
    /// Strictly increasing for every published view.
    pub generation: u64,
    /// The bridge's `dashboardUrl`; `None` while the dashboard is off.
    pub dashboard_origin: Option<String>,
    /// Ordered by id; legacy `tool.toml`-only packages are never listed.
    pub tools: Vec<Arc<ToolPresentation>>,
}

pub struct ToolPresentation {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Enabled, trusted and resolvable: UI, entry and `_settings` may be served.
    /// Declared art is served in every state.
    pub servable: bool,
    /// SDK-validated package-relative `[ui].root`.
    pub ui_root: String,
    /// SDK-validated `[ui].entry`, relative to `ui_root`.
    pub ui_entry: String,
    /// Declared package-relative art paths.
    pub icon: Option<String>,
    pub banner: Option<String>,
    /// Effective settings values as one JSON object with every `password`
    /// setting omitted; `None` when core could not read the stored values.
    pub settings: Option<Bytes>,
    /// Read-only lease on the package directory. A lease from a replaced or
    /// removed package stops opening files.
    pub root: RootLease,
}

/// Core keeps the sender; the bundled tool host receives the latest view.
pub type ToolHostViewSender = watch::Sender<Arc<ToolHostView>>;
pub type ToolHostViews = watch::Receiver<Arc<ToolHostView>>;

/// An empty generation-0 view and its channel.
pub fn channel(dashboard_origin: Option<String>) -> (ToolHostViewSender, ToolHostViews) {
    watch::channel(Arc::new(ToolHostView {
        generation: 0,
        dashboard_origin,
        tools: Vec::new(),
    }))
}

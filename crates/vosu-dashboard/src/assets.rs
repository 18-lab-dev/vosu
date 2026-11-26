//! The dashboard-only asset set (A7 asset inventory), embedded once here and
//! served only on the dashboard listener. `assets/server/` keeps the files and
//! their `assets/SHA256SUMS` rows; the design-system set, the bridge and
//! `report.html` have other owners.

macro_rules! asset {
    ($name:literal, $content_type:expr) => {
        Asset {
            name: $name,
            content_type: $content_type,
            bytes: include_bytes!(concat!("../../../assets/server/", $name)),
        }
    };
}

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";

pub(crate) struct Asset {
    pub(crate) name: &'static str,
    pub(crate) content_type: &'static str,
    pub(crate) bytes: &'static [u8],
}

/// Served as `GET /assets/<name>` on the dashboard listener, nowhere else.
pub(crate) static ASSETS: &[Asset] = &[
    asset!("homepage.css", CSS),
    asset!("homepage-controls.css", CSS),
    asset!("homepage.js", JS),
    asset!("homepage-settings.js", JS),
    asset!("dashboard-catalog.css", CSS),
    asset!("dashboard-catalog.js", JS),
    asset!("dashboard-catalog-browse.js", JS),
    asset!("dashboard-catalog-flow.js", JS),
    asset!("dashboard-catalog-sources.js", JS),
    asset!("dashboard-pages.js", JS),
    asset!("dashboard-notices.css", CSS),
    asset!("dashboard-notices.js", JS),
    asset!("dashboard-plugins.css", CSS),
    asset!("dashboard-plugins.js", JS),
    asset!("dashboard-status.js", JS),
    asset!("dashboard-tools.js", JS),
    asset!("dashboard-update.js", JS),
    asset!("update-notice.css", CSS),
];

/// The page shell template; never served as an asset.
pub(crate) const HOMEPAGE: &str = include_str!("../../../assets/server/homepage.html");

pub(crate) fn find(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.name == name)
}

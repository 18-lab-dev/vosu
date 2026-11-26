//! The tool host's embedded files (A7 asset inventory): the v2 bridge, the
//! theme/design pages and the shared design-system set. Each file is embedded
//! once here; served bytes are exactly the reviewed `assets/server/` bytes.
use bytes::Bytes;

macro_rules! asset {
    ($name:literal) => {
        include_bytes!(concat!("../../../assets/server/", $name))
    };
}

pub(crate) const BRIDGE: &str = include_str!("../../../assets/server/tool-bridge.js");
pub(crate) const DESIGN: &[u8] = asset!("design.html");
/// One source for the dashboard and tools: the tokens, then the shared
/// components (docs/tools/design.md).
pub(crate) const THEME: &str = concat!(
    include_str!("../../../assets/server/tokens.css"),
    "\n",
    include_str!("../../../assets/server/system.css"),
);

/// The design-system set served under `/assets/<name>` on both listener
/// roles, plus `/favicon.ico`.
pub(crate) const DESIGN_SYSTEM: &[(&str, &[u8])] = &[
    ("/assets/tokens.css", asset!("tokens.css")),
    ("/assets/system.css", asset!("system.css")),
    ("/assets/theme-init.js", asset!("theme-init.js")),
    (
        "/assets/barlow-condensed-latin-500-normal.woff2",
        asset!("barlow-condensed-latin-500-normal.woff2"),
    ),
    (
        "/assets/barlow-condensed-latin-600-normal.woff2",
        asset!("barlow-condensed-latin-600-normal.woff2"),
    ),
    (
        "/assets/barlow-condensed-latin-700-normal.woff2",
        asset!("barlow-condensed-latin-700-normal.woff2"),
    ),
    (
        "/assets/barlow-latin-400-normal.woff2",
        asset!("barlow-latin-400-normal.woff2"),
    ),
    (
        "/assets/barlow-latin-500-normal.woff2",
        asset!("barlow-latin-500-normal.woff2"),
    ),
    (
        "/assets/barlow-latin-600-normal.woff2",
        asset!("barlow-latin-600-normal.woff2"),
    ),
    (
        "/assets/ibm-plex-mono-latin-400-normal.woff2",
        asset!("ibm-plex-mono-latin-400-normal.woff2"),
    ),
    (
        "/assets/ibm-plex-mono-latin-500-normal.woff2",
        asset!("ibm-plex-mono-latin-500-normal.woff2"),
    ),
    ("/assets/fonts-OFL.txt", asset!("fonts-OFL.txt")),
    ("/favicon.ico", asset!("favicon.ico")),
];

pub(crate) fn design_system(path: &str) -> Option<Bytes> {
    DESIGN_SYSTEM
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| Bytes::from_static(bytes))
}

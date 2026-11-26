//! The tool host's embedded files (A7 asset inventory): the v2 bridge, the
//! theme/design pages and the shared design-system set. Each file is embedded
//! once here; served bytes are exactly the reviewed `assets/server/` bytes.
use bytes::{Bytes, BytesMut};
use std::sync::LazyLock;

macro_rules! asset {
    ($name:literal) => {
        include_bytes!(concat!("../../../assets/server/", $name))
    };
}

pub(crate) static BRIDGE: &str = include_str!("../../../assets/server/tool-bridge.js");
pub(crate) static DESIGN: &[u8] = asset!("design.html");
static TOKENS: &[u8] = asset!("tokens.css");
static SYSTEM: &[u8] = asset!("system.css");

/// Assemble the existing combined response once without embedding a second
/// copy of the two sheets already served individually under `/assets`.
pub(crate) static THEME: LazyLock<Bytes> = LazyLock::new(|| {
    let mut theme = BytesMut::with_capacity(TOKENS.len() + 1 + SYSTEM.len());
    theme.extend_from_slice(TOKENS);
    theme.extend_from_slice(b"\n");
    theme.extend_from_slice(SYSTEM);
    theme.freeze()
});

/// The design-system set served under `/assets/<name>` on both listener
/// roles, plus `/favicon.ico`.
pub(crate) static DESIGN_SYSTEM: &[(&str, &[u8])] = &[
    ("/assets/tokens.css", TOKENS),
    ("/assets/system.css", SYSTEM),
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

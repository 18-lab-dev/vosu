//! Pinned upstream v2 projections over authorized coherent surface captures.
#![forbid(unsafe_op_in_unsafe_fn)]

pub mod calculate;
pub mod dto;
pub mod files;
mod paths;
mod plugin;
pub mod precise;
pub use plugin::vosu_plugin_vtable;
pub mod provider;
pub mod regular;
mod source;
pub mod view;
mod wire;

pub const ID: &str = "vosu.adapter.upstream";
pub const MANIFEST: &str = include_str!("../manifest.toml");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotReady,
    InvalidSource(String),
    MissingTourneyScore,
    Surface(vosu_surface::state::SurfaceError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => f.write_str("state is not ready"),
            Self::InvalidSource(reason) => f.write_str(reason),
            Self::MissingTourneyScore => f.write_str("lazer spectator score is unavailable"),
            Self::Surface(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
impl From<vosu_surface::state::SurfaceError> for Error {
    fn from(error: vosu_surface::state::SurfaceError) -> Self {
        Self::Surface(error)
    }
}

#[cfg(test)]
mod tests;

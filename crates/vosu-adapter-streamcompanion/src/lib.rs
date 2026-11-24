//! Bus-backed streamCompanion projection; production mounting belongs to the bundled composition root.
#![forbid(unsafe_code)]
mod builder;
pub mod chart;
pub mod derived;
pub mod dto;
mod helpers;
pub mod provider;
pub mod view;
pub use builder::{build, encode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    NotReady,
    Source(String),
    Serialization(String),
}
impl Error {
    pub(crate) fn serialization(reason: String) -> Self {
        Self::Serialization(reason)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => f.write_str("state is not ready"),
            Self::Source(e) | Self::Serialization(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for Error {}
pub const MANIFEST: &str = include_str!("../manifest.toml");
#[cfg(test)]
mod tests;

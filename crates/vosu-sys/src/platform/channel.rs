//! Executable-version policy, independent of this crate's version or build profile.
use std::path::PathBuf;

/// Storage channel selected by the root executable's semver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildChannel {
    Release,
    Prerelease,
}

impl BuildChannel {
    pub fn from_version(version: &str) -> Result<Self, semver::Error> {
        let version = semver::Version::parse(version)?;
        Ok(if version.pre.is_empty() {
            Self::Release
        } else {
            Self::Prerelease
        })
    }

    /// Applies the storage channel to an already resolved normal root.
    pub fn root(self, root: PathBuf) -> PathBuf {
        match self {
            Self::Release => root,
            Self::Prerelease => root.join("prerelease"),
        }
    }
}

#[cfg(test)]
mod tests;

use super::Failure;
use crate::catalog::sources::SourceIdentity;
use semver::Version;
use vosu_plugin_api::{
    catalog::CatalogEntry,
    manifest::{HostSupport, PackageKind, PackageManifest, Runtime},
};

/// Only enabled, successfully fetched, contextually validated sources belong here.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    pub kind: PackageKind,
    pub entry: &'a CatalogEntry,
    pub source: &'a SourceIdentity,
    pub source_order: usize,
    pub catalog_order: usize,
    pub trusted: bool,
}
#[derive(Debug, Clone, Copy)]
pub struct Installed<'a> {
    pub manifest: &'a PackageManifest,
}
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub kind: PackageKind,
    pub id: &'a str,
    pub source: &'a SourceIdentity,
}
pub struct Snapshot<'a> {
    pub candidates: &'a [Candidate<'a>],
    /// All kept consumers, including tools and overlays. A requested identity
    /// replaces its installed counterpart explicitly; no implicit upgrades.
    pub installed: &'a [Installed<'a>],
    pub builtins: &'a super::BuiltinProviders,
    pub application: &'a Version,
    pub target: &'a str,
    pub max_package_bytes: u64,
}
impl Candidate<'_> {
    pub(super) fn rank(&self) -> (bool, usize, usize) {
        (
            !self.source.is_official(),
            self.source_order,
            self.catalog_order,
        )
    }
    pub(super) fn eligibility(&self, snapshot: &Snapshot<'_>) -> Result<(), Failure> {
        let support = self
            .entry
            .support(self.kind, snapshot.application, snapshot.target)
            .map_err(|_| Failure::InvalidInput)?;
        if support.host != HostSupport::Supported {
            return Err(Failure::Host(self.entry.id.clone()));
        }
        if !support.target {
            return Err(Failure::Target(self.entry.id.clone()));
        }
        let target = if self.kind == PackageKind::Plugin {
            snapshot.target
        } else {
            "any"
        };
        if self.entry.packages[target].size > snapshot.max_package_bytes {
            return Err(Failure::Size(self.entry.id.clone()));
        }
        if self.kind == PackageKind::Plugin
            && self.entry.runtime == Some(Runtime::InProcess)
            && !self.trusted
            && !self.source.is_official()
        {
            return Err(Failure::Runtime(self.entry.id.clone()));
        }
        Ok(())
    }
}

//! A tool package's `manifest.toml` (`kind = "tool"`), accepted only through
//! the SDK validator. There is no `tool.toml` parser and no fallback: a
//! package with only `tool.toml` is legacy (`needs-2.0-update`, A7.2).
use super::capabilities::Requirement;
use vosu_plugin_api::manifest::{PackageManifest, ToolManifest};

#[derive(Clone, Debug)]
pub(super) struct ToolPackage {
    pub tool: ToolManifest,
    pub requires: Vec<Requirement>,
    pub optional: Vec<Requirement>,
    package: PackageManifest,
}

impl ToolPackage {
    pub fn parse(text: &str) -> Result<Self, String> {
        let package = PackageManifest::parse(text).map_err(|error| error.to_string())?;
        if package
            .host_support(crate::application_version())
            .map_err(|error| error.to_string())?
            != vosu_plugin_api::manifest::HostSupport::Supported
        {
            return Err("manifest.toml host range does not match this daemon".into());
        }
        let PackageManifest::Tool(tool) = &package else {
            return Err("manifest.toml requires kind='tool'".into());
        };
        let requirements = |capabilities: &[vosu_plugin_api::manifest::Capability]| {
            capabilities
                .iter()
                .map(Requirement::from_capability)
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            requires: requirements(&tool.shared.requires)?,
            optional: requirements(&tool.shared.optional)?,
            tool: tool.clone(),
            package,
        })
    }

    pub(super) fn registration(&self, source: &str) -> vosu_core::plugin::PackageRegistration {
        vosu_core::plugin::PackageRegistration::new(&self.package, source)
    }

    pub(super) fn validate_context(
        &self,
        snapshot: &vosu_core::plugin::ValidationSnapshot,
        source: &str,
    ) -> Result<(), String> {
        match snapshot
            .validate(&self.package, crate::application_version(), source, false)
            .map_err(|error| error.to_string())?
        {
            vosu_plugin_api::manifest::HostSupport::Supported => Ok(()),
            vosu_plugin_api::manifest::HostSupport::Unsupported => {
                Err("manifest.toml host range does not match this daemon".into())
            }
        }
    }
}

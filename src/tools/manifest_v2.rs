//! Validated SDK tool metadata converted only for legacy host presentation.
//! `tool.toml` remains a separately named legacy protocol input until M7.
use super::{
    capabilities::Requirement,
    manifest::{Api, Manifest, Permissions, Ui},
};
use vosu_plugin_api::manifest::PackageManifest;

#[derive(Clone, Debug)]
pub(super) struct ManifestV2 {
    pub requires: Vec<Requirement>,
    pub optional: Vec<Requirement>,
    package: PackageManifest,
}

impl ManifestV2 {
    pub fn parse(text: &str) -> Result<(Manifest, Self), String> {
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
        let requires = tool
            .shared
            .requires
            .iter()
            .map(Requirement::from_capability)
            .collect::<Result<_, _>>()?;
        let optional = tool
            .shared
            .optional
            .iter()
            .map(Requirement::from_capability)
            .collect::<Result<_, _>>()?;
        let normalized = Manifest {
            manifest_version: 1,
            id: tool.shared.id.clone(),
            name: tool.shared.name.clone(),
            version: tool.shared.version.clone(),
            description: tool.shared.description.clone().unwrap_or_default(),
            author: tool.shared.author.clone().unwrap_or_default(),
            homepage: tool.shared.homepage.clone(),
            icon: tool.shared.icon.clone(),
            banner: tool.shared.banner.clone(),
            ui: Some(Ui {
                root: tool.ui.root.clone(),
                entry: tool.ui.entry.clone(),
            }),
            service: None,
            cli: None,
            api: Api::default(),
            requires: Vec::new(),
            permissions: Permissions::default(),
        };
        Ok((
            normalized,
            Self {
                requires,
                optional,
                package,
            },
        ))
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

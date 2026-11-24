//! Transitional web-only v2 tool ingestion; invalid v2 never falls back.
use std::collections::HashSet;

use serde::Deserialize;

use super::{
    capabilities::Requirement,
    manifest::{Api, Manifest, Permissions, Ui, valid_id},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestV2 {
    manifest_version: u32,
    kind: String,
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    homepage: Option<String>,
    icon: Option<String>,
    banner: Option<String>,
    #[serde(deserialize_with = "super::capabilities::version_requirement")]
    host: semver::VersionReq,
    #[serde(default)]
    replaces: Vec<String>,
    #[serde(default)]
    provides: Vec<toml::Value>,
    ui: Ui,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    #[serde(default)]
    pub optional: Vec<Requirement>,
}

impl ManifestV2 {
    pub fn parse(text: &str) -> Result<(Manifest, Self), String> {
        let manifest: Self = toml::from_str(text).map_err(|e| format!("manifest.toml: {e}"))?;
        if manifest.manifest_version != 1 || manifest.kind != "tool" {
            return Err("manifest.toml requires manifest_version=1 and kind='tool'".into());
        }
        if !manifest
            .host
            .matches(&semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap())
        {
            return Err("manifest.toml host range does not match this daemon".into());
        }
        if !manifest.provides.is_empty() {
            return Err("tools cannot provide capabilities".into());
        }
        let mut seen = HashSet::new();
        for former in &manifest.replaces {
            if !valid_id(former) || former == &manifest.id || !seen.insert(former) {
                return Err("invalid or duplicate replaces id".into());
            }
        }
        seen.clear();
        for requirement in manifest.requires.iter().chain(&manifest.optional) {
            requirement.validate()?;
            if !seen.insert(&requirement.id) {
                return Err("duplicate capability in requires/optional".into());
            }
        }
        // Reuse identity, asset and UI validation without translating capabilities
        // into legacy dependency or filesystem permissions.
        let normalized = Manifest {
            manifest_version: 1,
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            author: manifest.author.clone(),
            homepage: manifest.homepage.clone(),
            icon: manifest.icon.clone(),
            banner: manifest.banner.clone(),
            ui: Some(manifest.ui.clone()),
            service: None,
            cli: None,
            api: Api::default(),
            requires: Vec::new(),
            permissions: Permissions::default(),
        };
        normalized.validate()?;
        Ok((normalized, manifest))
    }
}

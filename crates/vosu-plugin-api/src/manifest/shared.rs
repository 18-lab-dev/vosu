//! Shared package fields and stable validation diagnostics.
use super::{Capability, relative};
use serde::Deserialize;
use std::{collections::HashSet, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub rule: &'static str,
    pub pointer: String,
    /// TOML byte range when the deserializer supplies source context.
    pub source_span: Option<std::ops::Range<usize>>,
}
impl ValidationError {
    pub fn new(rule: &'static str, pointer: impl Into<String>) -> Self {
        Self {
            rule,
            pointer: pointer.into(),
            source_span: None,
        }
    }
}
impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.rule, self.pointer)
    }
}
impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SharedManifest {
    pub manifest_version: u32,
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub host: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub icon: Option<String>,
    pub banner: Option<String>,
    #[serde(default)]
    pub replaces: Vec<String>,
    #[serde(default)]
    pub provides: Vec<Capability>,
    #[serde(default)]
    pub requires: Vec<Capability>,
    #[serde(default)]
    pub optional: Vec<Capability>,
}

pub(super) fn unique<'a>(
    values: impl IntoIterator<Item = &'a str>,
    pointer: &str,
) -> Result<(), ValidationError> {
    let mut seen = HashSet::new();
    for (i, value) in values.into_iter().enumerate() {
        if !seen.insert(value) {
            return Err(ValidationError::new(
                "manifest.duplicate",
                format!("{pointer}/{i}"),
            ));
        }
    }
    Ok(())
}

pub(super) fn metadata(
    homepage: Option<&str>,
    icon: Option<&str>,
    banner: Option<&str>,
) -> Result<(), ValidationError> {
    if let Some(value) = homepage {
        let url = url::Url::parse(value)
            .map_err(|_| ValidationError::new("manifest.homepage", "/homepage"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(ValidationError::new("manifest.homepage", "/homepage"));
        }
    }
    for (name, path) in [("icon", icon), ("banner", banner)] {
        if path.is_some_and(|p| !relative(p)) {
            return Err(ValidationError::new("manifest.path", format!("/{name}")));
        }
    }
    Ok(())
}

impl SharedManifest {
    pub(super) fn validate(&self, kind: &str) -> Result<(), ValidationError> {
        for (valid, rule, pointer) in [
            (self.kind == kind, "manifest.kind", "/kind"),
            (
                self.manifest_version == 1,
                "manifest.format",
                "/manifest_version",
            ),
            (super::valid_kind_segment(&self.id), "manifest.id", "/id"),
            (!self.name.trim().is_empty(), "manifest.name", "/name"),
            (
                semver::Version::parse(&self.version).is_ok(),
                "manifest.version",
                "/version",
            ),
            (
                semver::VersionReq::parse(&self.host).is_ok(),
                "manifest.host",
                "/host",
            ),
            (
                self.provides.is_empty(),
                "manifest.provides-kind",
                "/provides",
            ),
        ] {
            if !valid {
                return Err(ValidationError::new(rule, pointer));
            }
        }
        metadata(
            self.homepage.as_deref(),
            self.icon.as_deref(),
            self.banner.as_deref(),
        )?;
        validate_replaces(&self.id, &self.replaces, false)?;
        super::requirements::validate_lists(kind, &self.requires, &self.optional)
    }
}

pub(super) fn validate_replaces(
    id: &str,
    replaces: &[String],
    plugin: bool,
) -> Result<(), ValidationError> {
    unique(replaces.iter().map(String::as_str), "/replaces")?;
    for (i, former) in replaces.iter().enumerate() {
        if former == id
            || !(if plugin {
                super::valid_plugin_id(former)
            } else {
                super::valid_kind_segment(former)
            })
        {
            return Err(ValidationError::new(
                "manifest.replaces",
                format!("/replaces/{i}"),
            ));
        }
    }
    Ok(())
}

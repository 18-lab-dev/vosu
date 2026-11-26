//! Catalog wire model; array ownership is validated independently of deserialization.
use crate::manifest::schema::LaneRecord;
use crate::manifest::{ActionDeclaration, Capability, MatchTable, ResourceKind, Runtime};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub catalog_version: u32,
    pub name: String,
    pub published: String,
    #[serde(default)]
    pub plugins: Vec<CatalogEntry>,
    #[serde(default)]
    pub tools: Vec<CatalogEntry>,
    #[serde(default)]
    pub overlays: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub host: String,
    pub description: Option<String>,
    pub author: Option<String>,
    pub homepage: Option<String>,
    pub icon_url: Option<String>,
    pub banner_url: Option<String>,
    pub preview: Option<String>,
    pub resolution: Option<[u32; 2]>,
    #[serde(default)]
    pub replaces: Vec<String>,
    #[serde(default)]
    pub provides: Vec<CatalogProvide>,
    #[serde(default)]
    pub requires: Vec<Capability>,
    #[serde(default)]
    pub optional: Vec<Capability>,
    pub runtime: Option<Runtime>,
    pub linux_user_namespaces: Option<bool>,
    pub matcher: Option<MatchTable>,
    pub actions: Option<Vec<ActionDeclaration>>,
    pub resource_kinds: Option<Vec<ResourceKind>>,
    pub covers: Option<Vec<String>>,
    pub packages: BTreeMap<String, Package>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogProvide {
    pub id: String,
    pub version: String,
    pub schema_version: Option<String>,
    pub lanes: Option<Vec<LaneRecord>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

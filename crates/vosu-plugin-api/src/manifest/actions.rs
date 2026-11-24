//! Action declarations and exact capability correspondence (catalog § actions).

use std::collections::HashSet;

use serde::Deserialize;

use super::{ManifestError, PluginManifest, relative, valid_segment};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionSessionMode {
    Required,
    Optional,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDeclaration {
    pub name: String,
    pub version: String,
    pub title: String,
    pub args_schema: String,
    pub result_schema: String,
    pub session: ActionSessionMode,
    #[serde(default)]
    pub cli: bool,
}

pub(super) fn validate(manifest: &PluginManifest) -> Result<(), ManifestError> {
    let fail = |reason: &str| ManifestError::Action(reason.to_owned());
    let mut names = HashSet::new();
    for action in &manifest.actions {
        if !action.name.split('.').all(|s| valid_segment(s, 32, false)) {
            return Err(fail("invalid action name"));
        }
        if !names.insert(action.name.as_str()) {
            return Err(fail("duplicate action name"));
        }
        if action.title.trim().is_empty() {
            return Err(fail("empty action title"));
        }
        let version =
            semver::Version::parse(&action.version).map_err(|_| fail("invalid action version"))?;
        if !relative(&action.args_schema) || !relative(&action.result_schema) {
            return Err(fail("action schema must be a confined relative path"));
        }
        let id = format!("action:{}.{}", manifest.id, action.name);
        let mut provides = manifest.provides.iter().filter(|p| p.id == id);
        let Some(provided) = provides.next() else {
            return Err(fail("action has no matching provide"));
        };
        if provides.next().is_some() {
            return Err(fail("duplicate action provide"));
        }
        if semver::Version::parse(&provided.version).ok().as_ref() != Some(&version) {
            return Err(fail("action/provide versions differ"));
        }
    }
    for provided in &manifest.provides {
        if let Some(id) = provided.id.strip_prefix("action:") {
            let prefix = format!("{}.", manifest.id);
            if !id
                .strip_prefix(&prefix)
                .is_some_and(|name| names.contains(name))
            {
                return Err(fail("action provide has no declaration"));
            }
        }
    }
    Ok(())
}

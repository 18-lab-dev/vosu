use std::path::{Component, Path};

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub homepage: Option<String>,
    pub icon: Option<String>,
    pub ui: Option<Ui>,
    pub service: Option<Service>,
    #[serde(default)]
    pub api: Api,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    #[serde(default)]
    pub permissions: Permissions,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub root: String,
    #[serde(default = "entry")]
    pub entry: String,
}
fn entry() -> String {
    "index.html".into()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub command: Vec<String>,
    #[serde(default = "startup")]
    pub startup: String,
}
fn startup() -> String {
    "on-demand".into()
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Api {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub id: String,
    pub version: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    #[serde(default)]
    pub host: Vec<String>,
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && (id.as_bytes()[0].is_ascii_lowercase() || id.as_bytes()[0].is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0', ':'])
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
pub fn prefix_matches(prefix: &str, path: &str) -> bool {
    prefix == "/"
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Self = toml::from_str(text).map_err(|e| format!("tool.toml: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.manifest_version != 1 {
            return Err("manifest_version must be 1".into());
        }
        if !valid_id(&self.id) {
            return Err("id must match ^[a-z0-9][a-z0-9-]{0,31}$".into());
        }
        if self.name.trim().is_empty() {
            return Err("name must not be empty".into());
        }
        Version::parse(&self.version).map_err(|e| format!("version: {e}"))?;
        if self.ui.is_none() && self.service.is_none() {
            return Err("at least one of [ui] or [service] is required".into());
        }
        if let Some(homepage) = &self.homepage {
            let url = url::Url::parse(homepage).map_err(|e| format!("homepage: {e}"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err("homepage must be an http(s) URL".into());
            }
        }
        if let Some(icon) = &self.icon
            && (!relative(icon)
                || !matches!(
                    Path::new(icon).extension().and_then(|v| v.to_str()),
                    Some("svg" | "png")
                ))
        {
            return Err("icon must be a relative svg/png path".into());
        }
        if let Some(ui) = &self.ui
            && (!relative(&ui.root) || !relative(&ui.entry))
        {
            return Err("ui.root and ui.entry must be confined relative paths".into());
        }
        if let Some(service) = &self.service {
            if service.command.is_empty()
                || service.command.iter().any(|v| v.contains('\0'))
                || service.command[0].is_empty()
            {
                return Err("service.command must be non-empty argv without NUL".into());
            }
            if !matches!(service.startup.as_str(), "always" | "on-demand") {
                return Err("service.startup must be always or on-demand".into());
            }
            if !relative(&service.command[0]) {
                return Err(
                    "service.command[0] must be a confined relative path or bare executable name"
                        .into(),
                );
            }
        }
        for (kind, prefixes) in [("read", &self.api.read), ("write", &self.api.write)] {
            for prefix in prefixes {
                if !prefix.starts_with('/')
                    || prefix.contains(['?', '#', '%', '\\', '\0'])
                    || (prefix != "/" && (prefix.ends_with('/') || !relative(&prefix[1..])))
                {
                    return Err(format!("api.{kind}: invalid segment prefix {prefix:?}"));
                }
            }
        }
        if self.service.is_none() && (!self.api.read.is_empty() || !self.api.write.is_empty()) {
            return Err("[api] requires [service]".into());
        }
        let mut seen = std::collections::HashSet::new();
        for dependency in &self.requires {
            if !valid_id(&dependency.id) || !seen.insert(&dependency.id) {
                return Err(format!(
                    "requires: invalid or duplicate id {:?}",
                    dependency.id
                ));
            }
            VersionReq::parse(&dependency.version)
                .map_err(|e| format!("requires.{} version: {e}", dependency.id))?;
        }
        for permission in &self.permissions.host {
            if !matches!(permission.as_str(), "context" | "songs:write") {
                return Err(format!(
                    "permissions.host: unknown permission {permission:?}"
                ));
            }
        }
        Ok(())
    }
    pub fn access(&self, method: &str, path: &str) -> Option<bool> {
        if matches!(method, "GET" | "HEAD") && self.api.read.iter().any(|p| prefix_matches(p, path))
        {
            Some(false)
        } else if self.api.write.iter().any(|p| prefix_matches(p, path)) {
            Some(true)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_and_prefixes() {
        for id in ["a", "0", "skill-tracker"] {
            assert!(valid_id(id));
        }
        for id in ["", "_host", "A", "a/b", "abcdefghijklmnopqrstuvwxyz1234567"] {
            assert!(!valid_id(id));
        }
        assert!(prefix_matches("/v1/map", "/v1/map/abc"));
        assert!(!prefix_matches("/v1/map", "/v1/maps"));
    }
    #[test]
    fn validation() {
        let base =
            "manifest_version=1\nid='demo'\nname='Demo'\nversion='1.0.0'\n[ui]\nroot='web'\n";
        assert!(Manifest::parse(base).is_ok());
        for (from, to) in [
            ("manifest_version=1", "manifest_version=2"),
            ("version='1.0.0'", "version='latest'"),
            ("root='web'", "root='../web'"),
        ] {
            assert!(Manifest::parse(&base.replace(from, to)).is_err());
        }
    }
    #[test]
    fn rejects_unsafe_prefixes_permissions_dependencies_and_commands() {
        let base = "manifest_version=1\nid='demo'\nname='Demo'\nversion='1.0.0'\n[service]\ncommand=['bin/demo']\n";
        assert_eq!(
            Manifest::parse(base).unwrap().service.unwrap().startup,
            "on-demand"
        );
        for (suffix, expected) in [
            ("[api]\nread=['/v1/maps/']", "segment prefix"),
            ("[api]\nread=['/v1/../write']", "segment prefix"),
            ("[api]\nwrite=['/v1/%2e%2e']", "segment prefix"),
            ("[permissions]\nhost=['arbitrary']", "unknown permission"),
            ("[[requires]]\nid='valid'\nversion='not-semver'", "version"),
            (
                "[[requires]]\nid='_host'\nversion='*'",
                "invalid or duplicate",
            ),
        ] {
            assert!(
                Manifest::parse(&format!("{base}{suffix}"))
                    .unwrap_err()
                    .contains(expected)
            );
        }
        assert!(
            Manifest::parse(&base.replace("['bin/demo']", "[]"))
                .unwrap_err()
                .contains("command")
        );
        assert!(
            Manifest::parse(&base.replace("bin/demo", "../demo"))
                .unwrap_err()
                .contains("confined")
        );
        assert!(
            Manifest::parse(&base.replace("name='Demo'", "name=''"))
                .unwrap_err()
                .contains("name")
        );
    }
}

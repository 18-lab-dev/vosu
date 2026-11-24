//! Publication authority compiled once from a validated plugin manifest.
use std::sync::Arc;

use vosu_plugin_api::{ManifestError, PluginManifest, field_to_pointer};

use super::{CORE_NAMESPACES, Pointer, PointerError};

#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error("invalid publisher manifest: {0}")]
    Manifest(ManifestError),
    #[error(transparent)]
    Pointer(#[from] PointerError),
}

#[derive(Clone, Debug)]
pub struct PublisherGrant {
    pub(super) plugin_id: Arc<str>,
    own: bool,
    covers: Vec<(Arc<str>, Pointer)>,
    core_kinds: Vec<Arc<str>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permit {
    Own,
    Covered,
    Uncovered,
    ClientNamespace,
    Reserved,
    Foreign,
}

impl PublisherGrant {
    pub(crate) fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn from_manifest(m: &PluginManifest) -> Result<Self, GrantError> {
        m.validate().map_err(GrantError::Manifest)?;
        let mut covers = Vec::new();
        for id in &m.covers {
            for ns in CORE_NAMESPACES {
                if let Some(pointer) = field_to_pointer(ns, id) {
                    covers.push((Arc::from(ns), Pointer::parse(&pointer)?));
                }
            }
        }
        Ok(Self {
            plugin_id: Arc::from(m.id.as_str()),
            own: m
                .provides
                .iter()
                .any(|c| field_to_pointer(&m.id, &c.id).is_some()),
            covers,
            core_kinds: m
                .resource_kinds
                .iter()
                .filter(|k| {
                    matches!(
                        k.kind.as_str(),
                        "file" | "audio" | "background" | "set" | "skin"
                    )
                })
                .map(|k| Arc::from(k.kind.as_str()))
                .collect(),
        })
    }

    pub fn permits(&self, ns: &str, p: &Pointer) -> Permit {
        if ns == "vosu.client" {
            return Permit::ClientNamespace;
        }
        if ns == "vosu.sessions" {
            return Permit::Reserved;
        }
        if ns == self.plugin_id.as_ref() && self.own {
            return Permit::Own;
        }
        if !CORE_NAMESPACES.contains(&ns) {
            return Permit::Foreign;
        }
        let tokens = p.tokens();
        if ns == "vosu.chart" && tokens.first().is_some_and(|t| t.as_ref() == "resources") {
            return if tokens.len() == 1 {
                if self.core_kinds.iter().any(|k| k.as_ref() != "skin") {
                    Permit::Covered
                } else {
                    Permit::Uncovered
                }
            } else if self
                .core_kinds
                .iter()
                .any(|k| k.as_ref() != "skin" && k.as_ref() == tokens[1].as_ref())
            {
                Permit::Covered
            } else {
                Permit::Uncovered
            };
        }
        if ns == "vosu.skin" && tokens.first().is_some_and(|t| t.as_ref() == "tree") {
            return if self.core_kinds.iter().any(|k| k.as_ref() == "skin") {
                Permit::Covered
            } else {
                Permit::Uncovered
            };
        }
        if self
            .covers
            .iter()
            .any(|(n, covered)| n.as_ref() == ns && covered.is_prefix_of(p))
        {
            Permit::Covered
        } else {
            Permit::Uncovered
        }
    }

    pub fn covered_core_namespaces(&self) -> Vec<Arc<str>> {
        CORE_NAMESPACES
            .into_iter()
            .filter(|ns| *ns != "vosu.client")
            .filter(|ns| {
                self.covers.iter().any(|(n, _)| n.as_ref() == *ns)
                    || (*ns == "vosu.chart" && self.core_kinds.iter().any(|k| k.as_ref() != "skin"))
                    || (*ns == "vosu.skin" && self.core_kinds.iter().any(|k| k.as_ref() == "skin"))
            })
            .map(Arc::from)
            .collect()
    }
}

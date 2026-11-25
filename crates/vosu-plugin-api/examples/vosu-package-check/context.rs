//! The registry-context file: the verifier's fixed, explicit stand-in for
//! everything `ValidationContext` takes from a running host's registry
//! (`docs/v2/catalog-v2.md` "Package checker"). Format version 1:
//!
//! ```json
//! {
//!   "format": 1,
//!   "official_sources": ["18-lab-dev/vosu-tools"],
//!   "registrations": [
//!     {"kind": "plugin", "id": "vosu.osu", "source": "bundled",
//!      "official": true, "builtin": true, "replaces": []}
//!   ],
//!   "namespace_revisions": [
//!     {"namespace": "vosu.osu", "version": "1.3.0", "provenance": "trusted-bundled"}
//!   ],
//!   "common_defs_schema": { "$defs": { } }
//! }
//! ```
//!
//! `registrations` is the complete installed/bundled/other-source set the
//! catalog is validated against; `namespace_revisions` the authoritative
//! namespace schema versions; `common_defs_schema` the host release's
//! `common-defs.schema.json` document. Unknown keys are rejected.

use std::path::Path;

use semver::Version;
use serde::Deserialize;
use vosu_plugin_api::manifest::{NamespaceRevision, PackageKind, Registration, SchemaProvenance};

/// Upper bound on the context document.
const MAX_CONTEXT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryContext {
    format: u32,
    official_sources: Vec<String>,
    registrations: Vec<RegistrationRecord>,
    namespace_revisions: Vec<RevisionRecord>,
    common_defs_schema: serde_json::Value,
    #[serde(skip)]
    common_defs: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrationRecord {
    kind: KindRecord,
    id: String,
    source: String,
    official: bool,
    builtin: bool,
    #[serde(default)]
    replaces: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum KindRecord {
    Plugin,
    Tool,
    Overlay,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionRecord {
    namespace: String,
    version: String,
    provenance: ProvenanceRecord,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProvenanceRecord {
    ThirdParty,
    Official,
    TrustedBundled,
}

impl RegistryContext {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = crate::assets::read_bounded(path, MAX_CONTEXT_BYTES)
            .map_err(|e| format!("registry context {}: {e}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut context: Self =
            serde_json::from_str(text).map_err(|e| format!("registry context: {e}"))?;
        if context.format != 1 {
            return Err(format!(
                "registry context: unsupported format {}; this checker reads format 1",
                context.format
            ));
        }
        if context
            .official_sources
            .iter()
            .any(|source| source.is_empty() || source.trim() != source)
        {
            return Err("registry context: official_sources holds a non-exact identity".into());
        }
        // The compiler resolves `common-defs.schema.json#/$defs/...` against
        // this object, exactly as the host does with its embedded copy.
        context.common_defs = match context.common_defs_schema.get("$defs") {
            Some(defs @ serde_json::Value::Object(_)) => defs.clone(),
            _ => {
                return Err("registry context: common_defs_schema has no \"$defs\" object".into());
            }
        };
        Ok(context)
    }

    /// Exact identity match only: no aliases, prefixes or case folding.
    pub fn is_official(&self, source: &str) -> bool {
        self.official_sources.iter().any(|known| known == source)
    }

    pub fn registrations(&self) -> Vec<Registration<'_>> {
        self.registrations
            .iter()
            .map(|record| Registration {
                kind: match record.kind {
                    KindRecord::Plugin => PackageKind::Plugin,
                    KindRecord::Tool => PackageKind::Tool,
                    KindRecord::Overlay => PackageKind::Overlay,
                },
                id: &record.id,
                source: &record.source,
                official: record.official,
                builtin: record.builtin,
                replaces: &record.replaces,
            })
            .collect()
    }

    pub fn namespace_revisions(&self) -> Result<Vec<NamespaceRevision<'_>>, String> {
        self.namespace_revisions
            .iter()
            .map(|record| {
                Ok(NamespaceRevision {
                    namespace: &record.namespace,
                    version: Version::parse(&record.version).map_err(|_| {
                        format!(
                            "registry context: namespace {} version {:?} is not semver",
                            record.namespace, record.version
                        )
                    })?,
                    provenance: match record.provenance {
                        ProvenanceRecord::ThirdParty => SchemaProvenance::ThirdParty,
                        ProvenanceRecord::Official => SchemaProvenance::Official,
                        ProvenanceRecord::TrustedBundled => SchemaProvenance::TrustedBundled,
                    },
                })
            })
            .collect()
    }

    pub fn common_defs(&self) -> &serde_json::Value {
        &self.common_defs
    }
}

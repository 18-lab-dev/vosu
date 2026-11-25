//! Pure D7 namespace compiler. No filesystem, network, runtime evaluator or
//! registration authority lives here. Q1–Q3 profile: state-schema Sampling lanes.
mod compiler;
mod lanes;
#[cfg(test)]
mod tests;
pub use lanes::{DemandBits, LaneRecord, demand, validate_records};
use serde_json::Value;

pub const MAX_BYTES: usize = 262_144;
pub const MAX_DEPTH: usize = 32;
pub const MAX_NODE_VISITS: usize = 50_000;

/// Supplied by the host after authenticating source provenance, never inferred
/// from a document's title, $id or declared package identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaProvenance {
    Untrusted,
    TrustedOfficial,
}

/// Asset-backed compiler token. Callers may inspect but cannot manufacture or
/// mutate successful compilation evidence.
///
/// ```compile_fail
/// use vosu_plugin_api::manifest::schema::CompiledSchema;
/// let forged = CompiledSchema {
///     namespace: "acme.plugin".into(),
///     version: "1.0.0".parse().unwrap(),
///     lanes: vec![], required_top_level: vec![], hidden: vec![],
/// };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledSchema {
    namespace: String,
    version: semver::Version,
    lanes: Vec<LaneRecord>,
    required_top_level: Vec<String>,
    hidden: Vec<String>,
}

impl CompiledSchema {
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    pub fn version(&self) -> &semver::Version {
        &self.version
    }
    pub fn lanes(&self) -> &[LaneRecord] {
        &self.lanes
    }
    pub fn required_top_level(&self) -> &[String] {
        &self.required_top_level
    }
    pub fn hidden(&self) -> &[String] {
        &self.hidden
    }

    /// Consume validated evidence when transferring ownership into a runtime
    /// registry. This cannot construct or modify another compiler token.
    pub fn into_parts(
        self,
    ) -> (
        String,
        semver::Version,
        Vec<LaneRecord>,
        Vec<String>,
        Vec<String>,
    ) {
        (
            self.namespace,
            self.version,
            self.lanes,
            self.required_top_level,
            self.hidden,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    #[error("D7.bytes: schema exceeds 262144 bytes")]
    TooLarge,
    #[error("D7.depth: schema exceeds 32 structural levels")]
    TooDeep,
    #[error("D7.work: schema exceeds 50000 work nodes")]
    TooManyNodes,
    #[error("D7.ref: forbidden reference {0:?}")]
    ForeignRef(String),
    #[error("D7.ref: unresolved reference {0:?}")]
    UnresolvedRef(String),
    #[error("D7.ref: cyclic reference {0:?}")]
    CyclicRef(String),
    #[error("D7.visibility: forbidden annotation at {at}")]
    VisibilityNotAllowed { namespace: String, at: String },
    #[error("D7.title: title {title:?} differs from namespace {provides:?}")]
    TitleMismatch { title: String, provides: String },
    #[error("D7.version: schema version differs from owning provide")]
    VersionMismatch,
    #[error("D7.lane: invalid annotation at {0}")]
    InvalidLane(String),
    #[error("D7.structure: unsupported lane-bearing construct at {0}")]
    UnsupportedLane(String),
    #[error("D7.pointer: invalid pointer {0:?}")]
    InvalidPointer(String),
    #[error("D7.mirror: records must be sorted and exactly deduplicated")]
    NoncanonicalRecords,
    #[error("D7.json: {0}")]
    Json(String),
}

pub fn compile(
    namespace: &str,
    expected_version: &semver::Version,
    provenance: SchemaProvenance,
    text: &str,
    common_defs: &Value,
) -> Result<CompiledSchema, SchemaError> {
    compiler::compile(namespace, expected_version, provenance, text, common_defs)
}

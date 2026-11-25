//! Immutable installed provenance and explicit approval of one native artifact.
use super::sources::SourceIdentity;
use serde::{Deserialize, Serialize, Serializer};
use vosu_plugin_api::manifest::Runtime;

/// Construct only from SDK-validated, hash-verified package metadata. A correct
/// hash establishes integrity, not source trust. Never populate from a saved tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub id: String,
    pub source: SourceIdentity,
    pub version: String,
    pub sha256: [u8; 32],
    #[serde(serialize_with = "serialize_runtime")]
    pub runtime: Runtime,
}

fn serialize_runtime<S: Serializer>(runtime: &Runtime, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(match runtime {
        Runtime::InProcess => "in-process",
        Runtime::Process => "process",
    })
}

/// A user confirmation, never inferred from source trust or package integrity.
/// Installed-record publication must persist its removal on revocation before
/// another load/restart can observe the record (I0's operation/journal authority).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    artifact: Artifact,
}
impl Approval {
    /// Called only after explicit confirmation of this exact native plan add.
    pub fn grant(artifact: &Artifact) -> Self {
        Self {
            artifact: artifact.clone(),
        }
    }
    pub fn matches(&self, artifact: &Artifact) -> bool {
        self.artifact == *artifact
    }
}

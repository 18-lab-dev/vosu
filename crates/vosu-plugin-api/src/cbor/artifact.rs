//! Invocation-owned artifacts, CBOR (`docs/v2/plugin-abi.md` §6.13, AF.12).
//!
//! An artifact is a protected or sessionless output owned by one live
//! `tool`/`cli` invocation and served only to that job's credential. It is
//! never a resource reference and never names a host path. `ARTIFACT_OPEN`
//! is answered with an ordinary `VOSU_OP_RESOURCE_RESOLVED`
//! ([`crate::ResourceResolved::Path`] against a plugin-wide root,
//! [`crate::ResourceResolved::Stream`], or an error).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

use crate::cbor::common::ErrorInfo;
use crate::limits::VOSU_ARTIFACT_NAME_MAX_BYTES;

/// `VOSU_OP_ARTIFACT_DECLARE` (plugin -> host, nonzero `correlation_id`):
/// declares one immutable artifact for the still-running `invocation`.
/// `key` is the plugin's opaque lookup key, echoed back unchanged in
/// `ARTIFACT_OPEN`/`ARTIFACT_RELEASE`; `name` is one safe file-name
/// component ([`is_artifact_name`]) whose extension fixes the served type;
/// `size` is the exact byte length every open must reproduce.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ArtifactDeclare {
    pub invocation: u64,
    pub key: CborValue,
    pub name: String,
    pub content_id: CborValue,
    pub size: u64,
}

/// `VOSU_OP_ARTIFACT_DECLARED` (host -> plugin, reply correlated by
/// `correlation_id`): the opaque artifact id, or a refusal whose `code` is
/// `invocation_finished`, `caller_not_supported`, `too_many_artifacts` or
/// `invalid_name`. A refusal does not end the invocation. Both variants deny
/// unknown fields so a reply mixing the two shapes is rejected rather than
/// read as whichever matches first.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ArtifactDeclared {
    Artifact { artifact: String },
    Error { error: ErrorInfo },
}

/// `VOSU_OP_ARTIFACT_OPEN` (host -> plugin, correlated): one download of
/// `artifact`, whose declared `key` is echoed back. Answered with a
/// `VOSU_OP_RESOURCE_RESOLVED`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ArtifactOpen {
    pub artifact: String,
    pub key: CborValue,
}

/// `VOSU_OP_ARTIFACT_RELEASE` (host -> plugin, no reply): `artifact` left
/// core's registry and every download lease already received
/// `STREAM_CLOSE`; the plugin may delete what backs it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ArtifactRelease {
    pub artifact: String,
    pub key: CborValue,
}

/// Whether `name` is a valid artifact name: one nonempty file-name
/// component of at most [`VOSU_ARTIFACT_NAME_MAX_BYTES`] UTF-8 bytes, with no
/// `/`, `\`, NUL or other control character, and not `.` or `..`. Core
/// refuses any other name with `invalid_name`.
pub fn is_artifact_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= VOSU_ARTIFACT_NAME_MAX_BYTES as usize
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|character| matches!(character, '/' | '\\') || character.is_control())
}

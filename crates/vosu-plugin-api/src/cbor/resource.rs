//! Declared file roots and resource resolution, CBOR
//! (`docs/v2/plugin-abi.md` §6.5).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

use crate::cbor::common::ErrorInfo;

/// `VOSU_VALUE_RESOURCE_REF`/`VOSU_VALUE_TREE_REF`'s trailing CBOR
/// (embedded in a flat `PUBLISH_FIELD`/`PUBLISH_BATCH` entry's `data`, not
/// an opcode of its own): §6.5 "Minting a reference's id". `key` and
/// `content_id` are the plugin's own opaque CBOR values, echoed back
/// unchanged in a later resolve request.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ResourceRef {
    pub kind: String,
    pub key: CborValue,
    pub name: String,
    pub content_id: CborValue,
}

/// `VOSU_OP_ROOT_DECLARE` (plugin -> host, no reply). `handle: None`
/// declares a plugin-wide root; a non-`None` handle scopes it to one
/// session.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RootDeclare {
    #[serde(default)]
    pub handle: Option<u64>,
    pub name: String,
    pub path: String,
}

/// `VOSU_OP_RESOURCE_RESOLVE` (host -> plugin).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ResourceResolve {
    pub handle: u64,
    pub kind: String,
    pub key: CborValue,
}

/// `VOSU_OP_TREE_LIST` (host -> plugin): list a tree's index.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TreeList {
    pub handle: u64,
    pub kind: String,
    pub key: CborValue,
}

/// `VOSU_OP_TREE_LOOKUP` (host -> plugin): resolve one file within a tree
/// by its relative path, using the plugin's own lookup rules.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TreeLookup {
    pub handle: u64,
    pub kind: String,
    pub key: CborValue,
    pub path: String,
}

/// One `TREE_LIST` index entry. An entry with unknown content hashing
/// omits `hash` entirely rather than sending it `null`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TreeEntry {
    pub path: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// The single reply opcode for `RESOURCE_RESOLVE`, `TREE_LIST`, and
/// `TREE_LOOKUP` (plugin -> host, reply correlated by `correlation_id`):
/// the host's own pending-request table disambiguates which request each
/// reply answers, so this one opcode carries one of four shapes.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ResourceResolved {
    /// a declared root plus a relative path inside it
    Path {
        root: String,
        path: String,
    },
    /// a `vosu_resource_id` naming a bounded byte stream of the given
    /// total size, served through `STREAM_READ`/`STREAM_CHUNK`
    Stream {
        stream: u64,
        size: u64,
    },
    /// `TREE_LIST`'s reply shape
    Tree {
        entries: Vec<TreeEntry>,
        truncated: bool,
        revision: String,
    },
    Error {
        error: ErrorInfo,
    },
}

/// `VOSU_OP_STREAM_READ` (host -> plugin): `len` is bounded by
/// `VOSU_STREAM_CHUNK_MAX_BYTES`; ranges may be requested in any order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct StreamRead {
    pub stream: u64,
    pub offset: u64,
    pub len: u32,
}

/// `VOSU_OP_STREAM_CHUNK` (plugin -> host, reply correlated by
/// `correlation_id`): fewer bytes than requested, including zero, signals
/// end of stream.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum StreamChunk {
    Data { data: crate::cbor::codec::CborBytes },
    Error { error: ErrorInfo },
}

/// `VOSU_OP_STREAM_CLOSE` (host -> plugin, no reply): core no longer needs
/// this stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct StreamClose {
    pub stream: u64,
}

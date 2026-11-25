//! Runtime namespace registry and demand conversion.
//!
//! The SDK's `manifest::schema` compiler owns the entire D7 structural,
//! reference, lane and visibility profile. Core embeds schema bytes and owns
//! registration lifetime, grants and atomic replacement; it never walks a
//! namespace document independently or validates published values at runtime.
//! See state-schema.md Sampling lanes for the approved Q1–Q3 profile.

mod embedded;
mod lanes;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

pub use lanes::LaneBits;
use serde_json::Value;

use crate::state::Pointer;

/// The five best-effort common namespaces any game plugin may fill
/// (`docs/v2/state-schema.md`, ADR 0020).
pub const CORE_NAMESPACES: [&str; 5] = [
    "vosu.client",
    "vosu.status",
    "vosu.chart",
    "vosu.play",
    "vosu.skin",
];

/// The fan-out event namespace non-owning plugins subscribe to
/// (abi:158-160); never a namespace a schema document itself describes, so
/// it is reserved and cannot be `register`ed.
pub const SESSIONS_NAMESPACE: &str = "vosu.sessions";

/// Who the registry attributes a loaded namespace schema to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaOwner {
    Core,
    Plugin(Arc<str>),
}

/// One namespace's compiled schema: lane table and `x-visibility` subtrees
/// extracted by the walker, not the raw JSON Schema (which is never
/// re-walked after load).
#[derive(Debug)]
pub struct NamespaceSchema {
    pub namespace: Arc<str>,
    pub version: semver::Version,
    pub owner: SchemaOwner,
    pub required_top_level: Vec<Arc<str>>,
    lanes: Vec<vosu_plugin_api::manifest::schema::LaneRecord>,
    /// `x-visibility` pointers, e.g. `[Pointer::parse("/internal")]` for
    /// `vosu.osu`; empty for every other namespace in this freeze.
    hidden: Vec<Pointer>,
}

impl NamespaceSchema {
    /// Union of every annotated path's lanes that overlaps `tokens` (MJ).
    fn lanes_for(&self, tokens: &[&str]) -> LaneBits {
        let bits = vosu_plugin_api::manifest::schema::demand(&self.lanes, tokens);
        LaneBits {
            regular_always: bits.regular_always,
            precise_always: bits.precise_always,
            regular_gameplay: bits.regular_gameplay,
            precise_gameplay: bits.precise_gameplay,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("schema text exceeds the 256 KiB bound (D7)")]
    TooLarge,
    #[error("schema structure exceeds the 32-level JSON nesting bound (D7)")]
    TooDeep,
    #[error("schema expands to more structural nodes than the bound allows (D7)")]
    TooManyNodes,
    #[error("$ref {0:?} is outside the schema itself and common-defs (D7)")]
    ForeignRef(String),
    #[error("$ref {0:?} does not resolve to a known $defs entry")]
    UnresolvedRef(String),
    #[error("$ref {0:?} forms a cycle")]
    CyclicRef(String),
    #[error("x-visibility is not allowed on {namespace}{at} (D2, ss:297-311)")]
    VisibilityNotAllowed { namespace: String, at: String },
    #[error("schema title {title:?} does not match the provides namespace {provides:?}")]
    TitleMismatch { title: String, provides: String },
    #[error("{0:?} is a reserved namespace")]
    ReservedNamespace(String),
    #[error("{0:?} is already registered to a different owner")]
    NamespaceConflict(String),
    #[error("invalid x-sampling-lane: {0}")]
    InvalidLane(String),
    #[error("invalid schema JSON: {0}")]
    Json(String),
    #[error(transparent)]
    Compile(vosu_plugin_api::manifest::schema::SchemaError),
}

impl From<vosu_plugin_api::manifest::schema::SchemaError> for SchemaError {
    fn from(error: vosu_plugin_api::manifest::schema::SchemaError) -> Self {
        use vosu_plugin_api::manifest::schema::SchemaError as E;
        match error {
            E::TooLarge => Self::TooLarge,
            E::TooDeep => Self::TooDeep,
            E::TooManyNodes => Self::TooManyNodes,
            E::ForeignRef(r) => Self::ForeignRef(r),
            E::UnresolvedRef(r) => Self::UnresolvedRef(r),
            E::CyclicRef(r) => Self::CyclicRef(r),
            E::VisibilityNotAllowed { namespace, at } => {
                Self::VisibilityNotAllowed { namespace, at }
            }
            E::TitleMismatch { title, provides } => Self::TitleMismatch { title, provides },
            E::InvalidLane(at) => Self::InvalidLane(at),
            E::Json(message) => Self::Json(message),
            other => Self::Compile(other),
        }
    }
}

/// Loaded core namespace schemas plus each live plugin's own namespace.
pub struct SchemaRegistry {
    namespaces: HashMap<Arc<str>, Arc<NamespaceSchema>>,
    /// `common-defs.schema.json`'s `$defs` object, kept parsed once for
    /// every `$ref` resolution across every `load` call.
    common_defs: Value,
}

impl SchemaRegistry {
    /// Core namespace schemas only. Plugin schemas enter through their
    /// bundled or development plugin registrations, never implicitly here.
    pub fn with_core() -> Result<Self, SchemaError> {
        let common_doc: Value = serde_json::from_str(embedded::COMMON_DEFS)
            .map_err(|e| SchemaError::Json(e.to_string()))?;
        let common_defs = common_doc.get("$defs").cloned().unwrap_or(Value::Null);
        let mut registry = Self {
            namespaces: HashMap::new(),
            common_defs,
        };

        for (namespace, text) in [
            ("vosu.client", embedded::VOSU_CLIENT),
            ("vosu.status", embedded::VOSU_STATUS),
            ("vosu.chart", embedded::VOSU_CHART),
            ("vosu.play", embedded::VOSU_PLAY),
            ("vosu.skin", embedded::VOSU_SKIN),
        ] {
            registry.load(namespace, SchemaOwner::Core, text)?;
        }
        Ok(registry)
    }

    /// Registers a plugin-provided namespace schema (development plugins,
    /// O2): `provider` is the owning plugin id; `provides_id` is the
    /// namespace this text's `title` must equal verbatim (cat:73-76). A
    /// core namespace or `vosu.sessions` id is refused, and so is a
    /// `provides_id` already registered to a different owner — otherwise a
    /// second plugin (or the same plugin under another id) could silently
    /// replace another provider's namespace. Re-registering under the same
    /// owner (a plugin reloading its own schema) is allowed.
    ///
    /// Unversioned fixtures only; production must supply revision and provenance.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn register(
        &mut self,
        provider: &str,
        provides_id: &str,
        text: &str,
    ) -> Result<Arc<NamespaceSchema>, SchemaError> {
        if CORE_NAMESPACES.contains(&provides_id) || provides_id == SESSIONS_NAMESPACE {
            return Err(SchemaError::ReservedNamespace(provides_id.to_string()));
        }
        let owner = SchemaOwner::Plugin(Arc::from(provider));
        if let Some(existing) = self.namespaces.get(provides_id)
            && existing.owner != owner
        {
            return Err(SchemaError::NamespaceConflict(provides_id.to_string()));
        }
        self.load(provides_id, owner, text)
    }

    /// Registers bytes against the owning provide revision and authenticated
    /// source provenance. No privilege is derived from `provider` or `title`.
    pub fn register_versioned(
        &mut self,
        provider: &str,
        provides_id: &str,
        expected_version: &semver::Version,
        provenance: vosu_plugin_api::manifest::schema::SchemaProvenance,
        text: &str,
    ) -> Result<Arc<NamespaceSchema>, SchemaError> {
        if CORE_NAMESPACES.contains(&provides_id) || provides_id == SESSIONS_NAMESPACE {
            return Err(SchemaError::ReservedNamespace(provides_id.to_string()));
        }
        let owner = SchemaOwner::Plugin(Arc::from(provider));
        if let Some(existing) = self.namespaces.get(provides_id)
            && existing.owner != owner
        {
            return Err(SchemaError::NamespaceConflict(provides_id.to_string()));
        }
        self.load_versioned(provides_id, owner, Some(expected_version), provenance, text)
    }

    /// Drops every namespace schema owned by `provider` (a development
    /// plugin unloading or reloading).
    pub fn unregister_provider(&mut self, provider: &str) {
        self.namespaces
            .retain(|_, schema| schema.owner != SchemaOwner::Plugin(Arc::from(provider)));
    }

    pub fn get(&self, ns: &str) -> Option<&Arc<NamespaceSchema>> {
        self.namespaces.get(ns)
    }

    /// Namespaces currently available to merged-document API subscriptions.
    pub fn namespaces(&self) -> impl Iterator<Item = &str> {
        self.namespaces.keys().map(AsRef::as_ref)
    }

    /// Union of every lane annotation in `ns` that overlaps `tokens` (MJ).
    /// An unknown namespace has no demand.
    pub fn lanes(&self, ns: &str, tokens: &[&str]) -> LaneBits {
        self.namespaces
            .get(ns)
            .map(|schema| schema.lanes_for(tokens))
            .unwrap_or_default()
    }

    /// `x-visibility` pointers for `ns` (`[Pointer::parse("/internal")]` for
    /// `vosu.osu`, empty elsewhere).
    pub fn hidden_subtrees(&self, ns: &str) -> &[Pointer] {
        self.namespaces
            .get(ns)
            .map(|schema| schema.hidden.as_slice())
            .unwrap_or(&[])
    }

    fn load(
        &mut self,
        namespace: &str,
        owner: SchemaOwner,
        text: &str,
    ) -> Result<Arc<NamespaceSchema>, SchemaError> {
        let provenance = vosu_plugin_api::manifest::schema::SchemaProvenance::Untrusted;
        self.load_versioned(namespace, owner, None, provenance, text)
    }

    fn load_versioned(
        &mut self,
        namespace: &str,
        owner: SchemaOwner,
        expected_version: Option<&semver::Version>,
        provenance: vosu_plugin_api::manifest::schema::SchemaProvenance,
        text: &str,
    ) -> Result<Arc<NamespaceSchema>, SchemaError> {
        let version;
        let expected_version = if let Some(expected) = expected_version {
            expected
        } else {
            let root: Value =
                serde_json::from_str(text).map_err(|e| SchemaError::Json(e.to_string()))?;
            version = root
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| SchemaError::Json("missing /version".into()))?
                .parse()
                .map_err(|e: semver::Error| SchemaError::Json(e.to_string()))?;
            &version
        };
        let walked = vosu_plugin_api::manifest::schema::compile(
            namespace,
            expected_version,
            provenance,
            text,
            &self.common_defs,
        )?;
        let (_, version, lanes, required_top_level, hidden) = walked.into_parts();
        let schema = Arc::new(NamespaceSchema {
            namespace: Arc::from(namespace),
            version,
            owner,
            required_top_level: required_top_level.into_iter().map(Arc::from).collect(),
            lanes,
            hidden: hidden
                .iter()
                .map(|pointer| Pointer::parse(pointer).expect("compiler returns valid pointers"))
                .collect(),
        });
        self.namespaces
            .insert(Arc::from(namespace), Arc::clone(&schema));
        Ok(schema)
    }
}

//! Core schema registry, lane walker, and `x-visibility` rule (S6, S7, S12;
//! plan:574-579, D7, D13; ss:297-311, ss:386-442).
//!
//! Embeds `docs/v2/schemas/*.schema.json` with `include_str!`
//! ([`embedded`]) — the schema documents are the single source of truth;
//! this crate never re-validates a publish against them at runtime with a
//! general JSON Schema evaluator (D13: no `jsonschema` production
//! dependency, dev-only per X1). It instead compiles, at load time, the two
//! things a namespace schema needs: the per-field sampling-lane table (the
//! hand-written walker, [`walker`], generalizing `V2SubscriptionHub`'s fixed
//! classifier, `src/server/v2_subscription.rs:267-326`) and the
//! `x-visibility: "plugins"` carve-out ([`visibility`]), accepted only at
//! `vosu.osu`'s top-level `/internal` key.

mod embedded;
mod lanes;
#[cfg(test)]
mod tests;
mod visibility;
mod walker;

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
    lanes: Vec<(Vec<Arc<str>>, LaneBits)>,
    /// `x-visibility` pointers, e.g. `[Pointer::parse("/internal")]` for
    /// `vosu.osu`; empty for every other namespace in this freeze.
    hidden: Vec<Pointer>,
}

impl NamespaceSchema {
    /// Union of every annotated path's lanes that overlaps `tokens` (MJ).
    fn lanes_for(&self, tokens: &[&str]) -> LaneBits {
        self.lanes
            .iter()
            .filter(|(path, _)| lanes::paths_overlap(path, tokens))
            .fold(LaneBits::default(), |acc, (_, bits)| acc.union(*bits))
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
}

/// Loaded namespace schemas: the 5 core namespaces and `vosu.osu` at
/// startup ([`SchemaRegistry::with_core`]), plus any development plugin's
/// own namespace schema registered later ([`SchemaRegistry::register`]).
pub struct SchemaRegistry {
    namespaces: HashMap<Arc<str>, Arc<NamespaceSchema>>,
    /// `common-defs.schema.json`'s `$defs` object, kept parsed once for
    /// every `$ref` resolution across every `load` call.
    common_defs: Value,
}

impl SchemaRegistry {
    /// The common-defs `$defs` map plus the 5 core namespaces and
    /// `vosu.osu`, every one embedded (`include_str!`) from
    /// `docs/v2/schemas`.
    ///
    /// **Deviation (S6/O6):** the plan routes `vosu.osu`'s schema through
    /// `vosu-osu`'s own `BundledPlugin.namespace_schemas`, registered as
    /// `SchemaOwner::Plugin("vosu.osu")`, not loaded here as `Core`. That
    /// wiring (O6) does not exist yet, so this bring-up fixture loads
    /// `vosu.osu` as `Core` instead — the only way its schema enters the
    /// registry today. `register`'s conflict check (below) and the
    /// owner-gated `x-visibility` carve-out (`visibility::owner_eligible`)
    /// both treat `Core` and `Plugin("vosu.osu")` as equally eligible, so
    /// this stand-in cannot be silently replaced or unregistered by an
    /// unrelated plugin id; O6 can later switch this to a `register` call
    /// without changing either check.
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
        registry.load("vosu.osu", SchemaOwner::Core, embedded::VOSU_OSU)?;
        Ok(registry)
    }

    /// Registers a plugin-provided namespace schema (development plugins,
    /// O2): `provider` is the owning plugin id; `provides_id` is the
    /// namespace this text's `title` must equal verbatim (cat:73-76). A
    /// core namespace or `vosu.sessions` id is refused, and so is a
    /// `provides_id` already registered to a different owner — otherwise a
    /// second plugin (or the same plugin under another id) could silently
    /// replace another provider's namespace, including the `vosu.osu`
    /// `with_core` loads (finding 3). Re-registering under the *same*
    /// owner (a plugin reloading its own schema) is allowed.
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

    /// Drops every namespace schema owned by `provider` (a development
    /// plugin unloading or reloading).
    pub fn unregister_provider(&mut self, provider: &str) {
        self.namespaces
            .retain(|_, schema| schema.owner != SchemaOwner::Plugin(Arc::from(provider)));
    }

    pub fn get(&self, ns: &str) -> Option<&Arc<NamespaceSchema>> {
        self.namespaces.get(ns)
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
        let walked = walker::walk(namespace, &owner, text, &self.common_defs)?;
        let schema = Arc::new(NamespaceSchema {
            namespace: Arc::from(namespace),
            version: walked.version,
            owner,
            required_top_level: walked.required_top_level,
            lanes: walked.lanes,
            hidden: walked.hidden,
        });
        self.namespaces
            .insert(Arc::from(namespace), Arc::clone(&schema));
        Ok(schema)
    }
}

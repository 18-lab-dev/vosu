//! The M2 state bus (`docs/v2/implementation-plan.md` M2, ADR 0002): a
//! latest-only per-session namespace bus fed by `VOSU_OP_PUBLISH_FIELD`/
//! `PUBLISH_BATCH`.
//!
//! K1 landed the schema side: the embedded-schema registry, the
//! hand-written lane walker, and the `x-visibility` carve-out ([`schema`]).
//! K2 adds the value tree ([`value`]), pointers ([`pointer`]), per-session
//! documents ([`document`]), and the D2 projector ([`projector`]). The
//! commit path, subscriptions, and `DEMAND_CHANGED` delivery are later
//! units (K3 onward) and are not implemented here.

mod document;
mod pointer;
mod projector;
pub mod schema;
mod value;

#[cfg(test)]
mod tests;

pub use document::{SessionKey, SessionState};
pub use pointer::{Pointer, PointerError};
pub use projector::{
    Audience, InternalGrant, Redacted, project_document, project_namespace, project_select,
};
pub use schema::{
    CORE_NAMESPACES, LaneBits, NamespaceSchema, SESSIONS_NAMESPACE, SchemaError, SchemaOwner,
    SchemaRegistry,
};
pub use value::{StateObject, StateValue, ValueError};

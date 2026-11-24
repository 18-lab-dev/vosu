//! The M2 state bus (`docs/v2/implementation-plan.md` M2, ADR 0002): a
//! latest-only per-session namespace bus fed by `VOSU_OP_PUBLISH_FIELD`/
//! `PUBLISH_BATCH`.
//!
//! This unit (K1) lands only the schema side: the embedded-schema registry,
//! the hand-written lane walker, and the `x-visibility` carve-out
//! ([`schema`]). The value tree, pointers, per-session documents, the D2
//! projector, subscriptions, and `DEMAND_CHANGED` delivery are later units
//! (K2 onward) and are not implemented here.

pub mod schema;

pub use schema::{
    CORE_NAMESPACES, LaneBits, NamespaceSchema, SESSIONS_NAMESPACE, SchemaError, SchemaOwner,
    SchemaRegistry,
};

//! The M2 state bus (`docs/v2/implementation-plan.md` M2, ADR 0002): a
//! latest-only per-session namespace bus fed by `VOSU_OP_PUBLISH_FIELD`/
//! `PUBLISH_BATCH`.
//!
//! K1 landed the schema side: the embedded-schema registry, the
//! hand-written lane walker, and the `x-visibility` carve-out ([`schema`]).
//! K2 adds the value tree ([`value`]), pointers ([`pointer`]), per-session
//! documents ([`document`]), and the D2 projector ([`projector`]). The
//! commit path ([`StateStore`]) validates each whole batch before mutation.
//! Subscriptions and demand delivery are separate layers.

mod cbor;
mod client;
mod commit;
mod demand;
mod document;
mod ecma_number;
mod grant;
mod pointer;
mod projector;
mod runtime;
pub mod schema;
mod store;
mod subscriptions;
mod update;
mod value;

#[cfg(test)]
mod tests;

pub use client::ClientFacts;
pub use commit::{CommitError, CommitOutcome};
pub use demand::DemandSnapshot;
pub use document::{SessionKey, SessionState};
pub use grant::{GrantError, Permit, PublisherGrant};
pub use pointer::{Pointer, PointerError};
pub use projector::{
    Audience, InternalGrant, Redacted, project_document, project_namespace, project_select,
};
pub use runtime::StateRuntime;
pub use schema::{
    CORE_NAMESPACES, LaneBits, NamespaceSchema, SESSIONS_NAMESPACE, SchemaError, SchemaOwner,
    SchemaRegistry,
};
pub use store::StateStore;
pub use subscriptions::{
    MAX_NAMESPACE_BYTES, MAX_SELECTORS, MAX_SUBSCRIPTIONS, MAX_SUBSCRIPTIONS_PER_SUBSCRIBER,
    SubscribeError, SubscriberId, SubscriptionRegistry,
};
pub use update::{PendingUpdate, UpdateError, split_update};
pub use value::{StateObject, StateValue, ValueError};

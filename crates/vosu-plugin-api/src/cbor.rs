//! CBOR-encoded message payloads (`docs/v2/plugin-abi.md` §3, §6.2-§6.12,
//! D13) and the `ciborium` codec functions they all share.
//!
//! Every message type here round-trips through [`codec::encode`]/
//! [`codec::decode`], matching the exact map shape the spec defines:
//!
//! - a required field uses the spec's own JSON key spelling (`#[derive]`'s
//!   default field-name rendering already matches every key in this ABI,
//!   since the spec itself uses `snake_case`);
//! - a field the spec marks `|null` (always present, nullable) is
//!   `Option<T>` with `#[serde(default)]` so a decoder accepts either an
//!   explicit `null` or the key missing entirely, and an encoder always
//!   emits the key (`null` when absent) — the spec treats both wire forms
//!   as equivalent (§6.12 "an absent field reports value: null");
//! - a field the spec marks `|omitted`, with omission having its own
//!   defined meaning distinct from an explicit falsy value, is skipped on
//!   encode and defaulted on decode instead (`SESSION_IDENTIFY`'s
//!   `focus_priority`, `TREE_LIST`'s per-entry `hash`).
//!
//! No message here rejects an unrecognized map key: additive,
//! backward-compatible growth is this ABI's own minor-version story (§1),
//! so every struct accepts and ignores one exactly as a newer peer's extra
//! `struct_size` field is ignored by an older reader. A field that is
//! present but the wrong CBOR type, or an enum-typed string outside its
//! defined literals, is rejected (`CborError::Decode`): spec ambiguity
//! trade-off recorded in this crate's M1 report, not in this doc comment.

pub mod action;
pub mod codec;
pub mod common;
pub mod log;
pub mod read_plan;
pub mod resource;
pub mod scan;
pub mod session;
pub mod settings;
pub mod state;
pub mod timer;
pub use ciborium::Value as CborValue;
pub use codec::{CborBytes, CborError, decode, encode};

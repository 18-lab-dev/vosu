//! Flat binary payload structs for the hot-path opcodes
//! (`docs/v2/plugin-abi.md` §3, `include/vosu_plugin.h:232-446`).
//!
//! Every variable-length field in a `*_header` struct is an `{offset, len}`
//! pair naming a byte range within that same payload buffer, never a
//! pointer. The exact byte layout of every struct below is asserted against
//! `docs/v2/plugin-abi.md`'s §3 table by `tests/layout.rs`.

pub mod demand;
pub mod pointer;
pub mod publish;
pub mod read_plan;

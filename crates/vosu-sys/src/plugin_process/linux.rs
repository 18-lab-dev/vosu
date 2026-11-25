//! Linux objects behind one ring direction: a sealed anonymous `memfd`
//! mapping and an `eventfd` doorbell (`docs/v2/plugin-abi.md` §7.2).

mod objects;

pub use objects::Doorbell;
pub(super) use objects::Mapping;
pub(super) use objects::{adopt_mapping, create_mapping};

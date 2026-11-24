//! Publish-time identities and bounded membership; never opens resource bytes.
//!
//! Composition acquires the state transaction lock before the resource registry
//! lock, prepares and commits both while held, then notifies after unlocking.
//! Identity/registry methods never call plugins; byte/tree service composition
//! lives in `resource_io` to preserve the one-way dependency boundary.
mod id;
mod index;
pub(crate) mod media;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeIndexError {
    InvalidReply,
}
pub(crate) mod path;
mod registry;
mod secret;
pub mod tree;
mod types;

pub use id::{MAX_RESOURCE_ID_BYTES, ReferenceIdentity, ResourceId};
pub use index::{TreeIndex, TreeIndexEntry};
pub use path::validate as validate_tree_path;
pub use registry::{PreparedReferenceBindings, ResourceRegistry};
pub use secret::ResourceSecret;
pub use types::{BindingOwner, ReferenceClass, RegistryError, RegistryLimits, RegistryStats};

#[cfg(test)]
mod tests;

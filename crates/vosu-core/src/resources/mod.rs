//! Publish-time identities and bounded membership; never opens resource bytes.
//!
//! Composition acquires the state transaction lock before the resource registry
//! lock, prepares and commits both while held, then notifies after unlocking.
//! No method here calls plugins, acquires other locks, or starts jobs.
mod id;
mod registry;
mod secret;
mod types;

pub use id::{MAX_RESOURCE_ID_BYTES, ReferenceIdentity, ResourceId};
pub use registry::{PreparedReferenceBindings, ResourceRegistry};
pub use secret::ResourceSecret;
pub use types::{BindingOwner, ReferenceClass, RegistryError, RegistryLimits, RegistryStats};

#[cfg(test)]
mod tests;

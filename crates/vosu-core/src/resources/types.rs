use std::sync::Arc;

/// Resource and tree identities occupy disjoint authenticated domains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceClass {
    Resource,
    Tree,
}

impl ReferenceClass {
    pub(super) fn tag(self) -> &'static str {
        match self {
            Self::Resource => "r",
            Self::Tree => "t",
        }
    }
}

/// One state leaf binding, independent of the public token identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingOwner {
    pub instance: u64,
    pub session: u64,
    pub namespace: Arc<str>,
    pub pointer: Arc<str>,
}

impl BindingOwner {
    pub(super) fn bytes(&self) -> usize {
        size_of::<Self>() + self.namespace.len() + self.pointer.len()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegistryLimits {
    pub records: usize,
    pub metadata_bytes: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            records: 4096,
            metadata_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RegistryStats {
    pub records: usize,
    pub bindings: usize,
    pub metadata_bytes: usize,
    pub evictions: u64,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RegistryError {
    #[error("resource identity is invalid or exceeds its budget")]
    InvalidReference,
    #[error("resource reference cannot fit in the registry")]
    Capacity,
    #[error("prepared reference bindings are stale")]
    StalePreparation,
    #[error("resource token is invalid or its plugin is unknown")]
    NotFound,
    #[error("resource token is authentic but no longer current")]
    Gone,
}

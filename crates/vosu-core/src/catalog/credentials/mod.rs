//! Exact-source write-only credentials, injected with the channel config root.
//! S0 owns cross-store journal/recovery; the credential store alone owns secret
//! persistence and its mutation generation. No credentials cross surface/ABI.
mod store;
mod types;

pub use store::CredentialStore;
pub use types::{
    CredentialAvailability, CredentialError, CredentialInput, CredentialPresence,
    MAX_DOCUMENT_BYTES, MAX_ENTRIES, MAX_FIELD_BYTES, MAX_MUTATION_BYTES,
};

#[cfg(test)]
mod tests;

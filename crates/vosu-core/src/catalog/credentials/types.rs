use serde::Deserialize;

pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
pub const MAX_ENTRIES: usize = 32;
pub const MAX_FIELD_BYTES: usize = 8 * 1024;
pub const MAX_MUTATION_BYTES: usize = 32 * 1024;

/// Write-only input. Intentionally implements neither Debug nor Serialize.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum CredentialInput {
    Token { token: String },
    Basic { username: String, password: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CredentialPresence {
    pub has_credential: bool,
    pub availability: CredentialAvailability,
    pub generation: u64,
}

/// Fixed diagnostics never contain parser, filesystem or secret payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("invalid credential input")]
    InvalidInput,
    #[error("unknown catalog source")]
    UnknownSource,
    #[error("catalog credentials unavailable; explicit replacement required")]
    Unavailable,
    #[error("cannot publish catalog credentials")]
    Persistence,
    #[error("catalog credential generation exhausted")]
    GenerationExhausted,
}

impl CredentialInput {
    /// Decode a bounded mutation without returning serde's payload-bearing errors.
    pub fn from_body(body: &[u8]) -> Result<Self, CredentialError> {
        if body.len() > MAX_MUTATION_BYTES {
            return Err(CredentialError::InvalidInput);
        }
        let input: Self = serde_json::from_slice(body).map_err(|_| CredentialError::InvalidInput)?;
        input.validate()?;
        Ok(input)
    }

    pub(super) fn validate(&self) -> Result<(), CredentialError> {
        let valid = match self {
            Self::Token { token } => field(token) && token.bytes().all(|b| (0x21..=0x7e).contains(&b)),
            Self::Basic { username, password } => field(username) && !username.contains(':') && field(password),
        };
        if valid { Ok(()) } else { Err(CredentialError::InvalidInput) }
    }

    pub(super) fn matches_source(&self, source: &str) -> bool {
        // Membership comes exclusively from S0's validated source identities.
        let static_source = source.starts_with("https://") && source.ends_with("/catalog-v2.json");
        matches!(self, Self::Basic { .. }) == static_source
    }
}

pub(super) fn field(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_FIELD_BYTES && !value.chars().any(char::is_control)
}

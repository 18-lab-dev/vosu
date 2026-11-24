//! Bounded forge protocols and core-only, generation-scoped catalog retrieval.
pub mod gitea;
pub mod github;
pub mod gitlab;
mod service;
pub mod static_source;
use super::{
    credentials::CredentialInput,
    sources::{DetectedSource, Forge, SourceLocation},
};
use serde::{Deserialize, Deserializer};
pub use service::{CatalogFetchService, FetchContext};
use url::Url;
use vosu_net::streaming::{MetadataAccept, Origin, RequestCredential, RequestPurpose};

pub const MAX_METADATA_BYTES: usize = 1024 * 1024;
pub const MAX_ASSETS: usize = 1024;
pub const MAX_METADATA_TEXT: usize = 8192;
const CATALOG_NAME: &str = "catalog-v2.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    #[error("source unavailable; check source spelling, access and credential settings")]
    Unavailable,
    #[error("source credential required or rejected")]
    CredentialRejected,
    #[error("credential may be required; source existence unconfirmed")]
    CredentialRequired,
    #[error("could not detect forge type")]
    UndetectedForge,
    #[error("source credentials unavailable")]
    CredentialsUnavailable,
    #[error("invalid release metadata")]
    InvalidMetadata,
    #[error("release must contain exactly one catalog-v2.json asset")]
    CatalogAsset,
    #[error("unsafe catalog asset origin")]
    AssetOrigin,
    #[error("credential kind does not match source")]
    CredentialKind,
    #[error("catalog fetch cancelled")]
    Cancelled,
}
/// Never expose upstream bodies, URLs or presigned parameters in diagnostics.
fn status(status: u16) -> Result<(), FetchError> {
    match status {
        200 => Ok(()),
        401 => Err(FetchError::CredentialRequired),
        403 => Err(FetchError::CredentialRejected),
        _ => Err(FetchError::Unavailable),
    }
}
fn text<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    if value.len() > MAX_METADATA_TEXT || value.chars().any(char::is_control) {
        return Err(serde::de::Error::custom("invalid metadata text"));
    }
    Ok(value)
}
fn assets<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Bounded<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Bounded<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded release assets")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element()? {
                if values.len() == MAX_ASSETS {
                    return Err(serde::de::Error::custom("too many assets"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    d.deserialize_seq(Bounded(std::marker::PhantomData))
}
fn metadata<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, FetchError> {
    if body.len() > MAX_METADATA_BYTES {
        return Err(FetchError::InvalidMetadata);
    }
    serde_json::from_slice(body).map_err(|_| FetchError::InvalidMetadata)
}
fn select<'a>(
    items: impl Iterator<Item = (&'a str, &'a str)>,
    source: &DetectedSource,
) -> Result<Url, FetchError> {
    let mut matches = items.filter(|(name, _)| *name == CATALOG_NAME);
    let (_, value) = matches.next().ok_or(FetchError::CatalogAsset)?;
    if matches.next().is_some() {
        return Err(FetchError::CatalogAsset);
    }
    validate_asset(value, source)
}
fn validate_asset(value: &str, source: &DetectedSource) -> Result<Url, FetchError> {
    let url = Url::parse(value).map_err(|_| FetchError::AssetOrigin)?;
    let authority = value
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""));
    if authority.is_none_or(|a| a.contains('@'))
        || url.fragment().is_some()
        || Origin::from_url(&url).ok().as_ref() != Some(source.credential_origin())
    {
        return Err(FetchError::AssetOrigin);
    }
    Ok(url)
}
fn endpoint(origin: &Url, components: &[&str]) -> Result<Url, FetchError> {
    // URL path builders discard dot components; never silently select another tag.
    if components
        .iter()
        .any(|component| matches!(*component, "." | ".."))
    {
        return Err(FetchError::InvalidMetadata);
    }
    let mut url = origin.clone();
    url.set_query(None);
    url.set_fragment(None);
    url.path_segments_mut()
        .map_err(|_| FetchError::InvalidMetadata)?
        .clear()
        .extend(components.iter().copied());
    Ok(url)
}
fn repository(source: &DetectedSource) -> Result<(&Url, &str), FetchError> {
    match &source.parsed.location {
        SourceLocation::Repository {
            origin,
            project_path,
        } => Ok((origin, project_path)),
        _ => Err(FetchError::InvalidMetadata),
    }
}
fn credential(
    forge: Forge,
    input: Option<&CredentialInput>,
) -> Result<Option<RequestCredential>, FetchError> {
    input
        .map(|input| match (forge, input) {
            (Forge::Static, CredentialInput::Basic { username, password }) => {
                RequestCredential::basic(username, password).map_err(|_| FetchError::CredentialKind)
            }
            (Forge::Gitea, CredentialInput::Token { token }) => {
                RequestCredential::token(token).map_err(|_| FetchError::CredentialKind)
            }
            (Forge::GitHub | Forge::GitLab, CredentialInput::Token { token }) => {
                RequestCredential::bearer(token).map_err(|_| FetchError::CredentialKind)
            }
            _ => Err(FetchError::CredentialKind),
        })
        .transpose()
}
fn metadata_purpose(forge: Forge) -> RequestPurpose {
    RequestPurpose::SourceMetadata(if forge == Forge::GitHub {
        MetadataAccept::GitHub
    } else {
        MetadataAccept::Json
    })
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialGuidance {
    pub url: Url,
    pub scope: &'static str,
}
pub fn credential_guidance(source: &DetectedSource) -> Result<CredentialGuidance, FetchError> {
    let (url, scope) = match source.forge {
        Forge::GitHub => (
            Url::parse("https://github.com/settings/personal-access-tokens/new")
                .expect("fixed URL"),
            "Fine-grained token: selected repository only, Contents read-only. Classic tokens require broader repo scope; prefer fine-grained.",
        ),
        Forge::GitLab => (
            endpoint(
                repository(source)?.0,
                &["-", "user_settings", "personal_access_tokens"],
            )?,
            "read_api only; grant access only to the required project.",
        ),
        Forge::Gitea => (
            endpoint(repository(source)?.0, &["user", "settings", "applications"])?,
            "read:repository only, restricted to the required repository where supported.",
        ),
        Forge::Static => (
            match &source.parsed.location {
                SourceLocation::Static { url } => url
                    .origin()
                    .ascii_serialization()
                    .parse()
                    .map_err(|_| FetchError::InvalidMetadata)?,
                _ => return Err(FetchError::InvalidMetadata),
            },
            "HTTP Basic account with read-only catalog access; package and artwork requests never authenticate.",
        ),
    };
    Ok(CredentialGuidance { url, scope })
}
#[cfg(test)]
mod tests;

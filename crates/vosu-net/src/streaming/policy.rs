//! Request-local catalog authority. No ambient client headers or credentials.
use super::{NetworkError, Url};
use reqwest::header::HeaderValue;

const MAX_FIELD: usize = 8 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    host: String,
    port: u16,
}
impl Origin {
    pub fn parse(value: &str) -> Result<Self, NetworkError> {
        Self::from_url(&Url::parse(value)?)
    }

    pub fn from_url(url: &Url) -> Result<Self, NetworkError> {
        validate_url(url)?;
        Ok(Self {
            host: url
                .host_str()
                .ok_or(NetworkError::InvalidPolicy)?
                .to_owned(),
            port: url
                .port_or_known_default()
                .ok_or(NetworkError::InvalidPolicy)?,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataAccept {
    GitHub,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestPurpose {
    Probe,
    SourceMetadata(MetadataAccept),
    CatalogAsset,
    Package,
    Artwork,
    Updater,
    Counter,
}

#[derive(Clone, Debug)]
pub struct SourcePolicy {
    configured_origin: Origin,
    credential_origin: Origin,
    private_network: bool,
}
impl SourcePolicy {
    /// Repository endpoints derive their authority; there is no API-host override.
    /// GitHub shorthand callers use the corresponding https://github.com URL.
    pub fn repository(url: &Url, private_network: bool) -> Result<Self, NetworkError> {
        let configured_origin = Origin::from_url(url)?;
        let credential_origin = if configured_origin == Origin::parse("https://github.com")? {
            Origin::parse("https://api.github.com")?
        } else {
            configured_origin.clone()
        };
        Ok(Self {
            configured_origin,
            credential_origin,
            private_network,
        })
    }

    /// Static sources retain their literal origin, including GitHub-hosted URLs.
    pub fn static_source(url: &Url, private_network: bool) -> Result<Self, NetworkError> {
        let configured_origin = Origin::from_url(url)?;
        Ok(Self {
            credential_origin: configured_origin.clone(),
            configured_origin,
            private_network,
        })
    }

    pub fn credential_origin(&self) -> &Origin {
        &self.credential_origin
    }
}

#[derive(Clone, Debug)]
pub struct RequestPolicy {
    pub(super) purpose: RequestPurpose,
    pub(super) source: Option<SourcePolicy>,
}
impl RequestPolicy {
    pub fn public(purpose: RequestPurpose) -> Self {
        Self {
            purpose,
            source: None,
        }
    }

    pub fn source(purpose: RequestPurpose, source: SourcePolicy) -> Result<Self, NetworkError> {
        if !matches!(
            purpose,
            RequestPurpose::Probe
                | RequestPurpose::SourceMetadata(_)
                | RequestPurpose::CatalogAsset
        ) {
            return Err(NetworkError::InvalidPolicy);
        }
        Ok(Self {
            purpose,
            source: Some(source),
        })
    }

    pub(super) fn validate_initial(&self, url: &Url) -> Result<(), NetworkError> {
        validate_url(url)?;
        if self.purpose == RequestPurpose::Probe
            && let Some(source) = &self.source
            && Origin::from_url(url)? != source.configured_origin
        {
            return Err(NetworkError::InvalidPolicy);
        }
        Ok(())
    }

    pub(super) fn permits_private(&self, url: &Url) -> bool {
        self.source.as_ref().is_some_and(|source| {
            source.private_network && url.host_str() == Some(source.configured_origin.host())
        })
    }

    pub(super) fn credential_eligible(&self, url: &Url) -> bool {
        matches!(
            self.purpose,
            RequestPurpose::SourceMetadata(_) | RequestPurpose::CatalogAsset
        ) && self.source.as_ref().is_some_and(|source| {
            Origin::from_url(url).is_ok_and(|origin| origin == source.credential_origin)
        })
    }
}

/// Sensitive request-only material. Intentionally neither Debug nor Serialize.
pub struct RequestCredential(HeaderValue);
impl RequestCredential {
    pub fn bearer(token: &str) -> Result<Self, NetworkError> {
        Self::token_header("Bearer", token)
    }
    pub fn token(token: &str) -> Result<Self, NetworkError> {
        Self::token_header("token", token)
    }

    fn token_header(scheme: &str, token: &str) -> Result<Self, NetworkError> {
        validate_field(token)?;
        Self::header(format!("{scheme} {token}"))
    }

    pub fn basic(username: &str, password: &str) -> Result<Self, NetworkError> {
        validate_field(username)?;
        validate_field(password)?;
        if username.contains(':') {
            return Err(NetworkError::InvalidCredential);
        }
        let pair = format!("{username}:{password}");
        Self::header(format!(
            "Basic {}",
            base64::display::Base64Display::new(
                pair.as_bytes(),
                &base64::engine::general_purpose::STANDARD,
            )
        ))
    }

    fn header(value: String) -> Result<Self, NetworkError> {
        let mut value =
            HeaderValue::from_str(&value).map_err(|_| NetworkError::InvalidCredential)?;
        value.set_sensitive(true);
        Ok(Self(value))
    }

    pub(super) fn value(&self) -> HeaderValue {
        self.0.clone()
    }
}

fn validate_field(value: &str) -> Result<(), NetworkError> {
    if value.len() > MAX_FIELD || value.chars().any(char::is_control) {
        return Err(NetworkError::InvalidCredential);
    }
    Ok(())
}

pub(super) fn validate_url(url: &Url) -> Result<(), NetworkError> {
    if url.scheme() != "https" {
        return Err(NetworkError::Scheme);
    }
    reject_userinfo(url)
}

pub(super) fn reject_userinfo(url: &Url) -> Result<(), NetworkError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(NetworkError::InvalidPolicy);
    }
    Ok(())
}

/// Redirect state is monotonic: once stripped it can never become eligible again.
pub(super) struct CredentialChain<'a> {
    credential: Option<&'a RequestCredential>,
}
impl<'a> CredentialChain<'a> {
    pub(super) fn new(
        policy: &RequestPolicy,
        initial: &Url,
        credential: Option<&'a RequestCredential>,
    ) -> Self {
        Self {
            credential: credential.filter(|_| policy.credential_eligible(initial)),
        }
    }
    pub(super) fn redirect(&mut self, current: &Url, next: &Url) {
        if Origin::from_url(current).ok() != Origin::from_url(next).ok() {
            self.credential = None;
        }
    }
    pub(super) fn header(&self) -> Option<HeaderValue> {
        self.credential.map(RequestCredential::value)
    }
}

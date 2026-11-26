//! Exact source identities are authority keys; parsed fetch conveniences are not aliases.
use serde::{Deserialize, Serialize};
use url::Url;

pub const MAX_SOURCES: usize = 32;
pub const MAX_SOURCE_BYTES: usize = 8192;
pub const OFFICIAL_SOURCES: [&str; 3] = [
    "18-lab-dev/vosu-tools",
    "18-lab-dev/vosulay",
    "18-lab-dev/vosu-overlays",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceIdentity(String);
impl SourceIdentity {
    pub fn parse(value: impl Into<String>) -> Result<ParsedSource, SourceParseError> {
        parse(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_official(&self) -> bool {
        self.is_allowlisted() || self.is_official_fixture()
    }

    /// Production allowlist only; the fixture switch never changes disk settings.
    pub(crate) fn is_allowlisted(&self) -> bool {
        OFFICIAL_SOURCES.contains(&self.as_str())
    }

    pub(crate) fn is_official_fixture(&self) -> bool {
        #[cfg(all(feature = "test-harness", debug_assertions))]
        if let Some(fixture) = OFFICIAL_FIXTURE.as_ref() {
            return self == fixture;
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSource {
    pub identity: SourceIdentity,
    pub location: SourceLocation,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceLocation {
    Static {
        url: Url,
    },
    GitHub {
        owner: String,
        repo: String,
        tag: Option<String>,
    },
    Repository {
        origin: Url,
        project_path: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
    Gitea,
    Static,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid catalog source")]
pub struct SourceParseError;

fn component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}
fn parse(value: String) -> Result<ParsedSource, SourceParseError> {
    if value.is_empty()
        || value.len() > MAX_SOURCE_BYTES
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(SourceParseError);
    }
    let location = if value.contains("://") {
        let url = Url::parse(&value).map_err(|_| SourceParseError)?;
        // Reject even empty userinfo and URL-parser path normalization conveniences.
        let authority = value
            .split_once("://")
            .ok_or(SourceParseError)?
            .1
            .split(['/', '?', '#'])
            .next()
            .ok_or(SourceParseError)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || authority.contains('@')
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || value.contains('\\')
        {
            return Err(SourceParseError);
        }
        if url.path().ends_with("/catalog-v2.json") {
            SourceLocation::Static { url }
        } else {
            let raw_path = value
                .split_once("://")
                .ok_or(SourceParseError)?
                .1
                .split_once('/')
                .map_or("", |(_, path)| path);
            let path = raw_path.strip_suffix('/').unwrap_or(raw_path);
            let path = path.strip_suffix(".git").unwrap_or(path);
            if path.split('/').count() < 2 || !path.split('/').all(component) {
                return Err(SourceParseError);
            }
            let mut origin = url;
            origin.set_path("/");
            SourceLocation::Repository {
                origin,
                project_path: path.to_owned(),
            }
        }
    } else {
        let (repository, tag) = value
            .split_once('@')
            .map_or((value.as_str(), None), |(repository, tag)| {
                (repository, Some(tag))
            });
        let (owner, repo) = repository.split_once('/').ok_or(SourceParseError)?;
        if !component(owner)
            || !component(repo)
            || tag.is_some_and(|tag| tag.is_empty() || tag.contains('@'))
        {
            return Err(SourceParseError);
        }
        SourceLocation::GitHub {
            owner: owner.into(),
            repo: repo.into(),
            tag: tag.map(str::to_owned),
        }
    };
    Ok(ParsedSource {
        identity: SourceIdentity(value),
        location,
    })
}

#[cfg(any(test, feature = "test-harness"))]
pub(crate) const HARNESS_HOST: &str = "catalog.invalid";

#[cfg(feature = "test-harness")]
pub(crate) fn harness_socket() -> Option<std::net::SocketAddr> {
    std::env::var("VOSU_CATALOG_TEST_SOCKET")
        .ok()?
        .parse::<std::net::SocketAddr>()
        .ok()
        .filter(|socket| socket.ip().is_loopback() && socket.port() != 0)
}

#[cfg(all(feature = "test-harness", debug_assertions))]
static OFFICIAL_FIXTURE: std::sync::LazyLock<Option<SourceIdentity>> =
    std::sync::LazyLock::new(|| {
        let source = fixture_source(
            std::env::var("VOSU_TEST_OFFICIAL_CATALOG").ok()?,
            harness_socket()?,
            cfg!(debug_assertions),
        )?;
        tracing::info!(
            source = source.as_str(),
            trust = "official-fixture",
            fixture = true,
            "enabled test catalog authority"
        );
        Some(source)
    });

#[cfg(any(test, all(feature = "test-harness", debug_assertions)))]
fn fixture_source(
    value: String,
    socket: std::net::SocketAddr,
    debug_build: bool,
) -> Option<SourceIdentity> {
    if !(debug_build && cfg!(feature = "test-harness") && socket.ip().is_loopback())
        || socket.port() == 0
    {
        return None;
    }
    let parsed = SourceIdentity::parse(value).ok()?;
    // Match the transport's exact scheme and host spelling as well as its port.
    let authority = format!("https://{HARNESS_HOST}:{}/", socket.port());
    match parsed.location {
        SourceLocation::Static { url }
            if url.port_or_known_default() == Some(socket.port())
                && parsed.identity.as_str().starts_with(&authority) =>
        {
            Some(parsed.identity)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_catalog_switch_requires_debug_harness_and_the_exact_loopback_authority() {
        let source = "https://catalog.invalid:8111/fixtures/catalog-v2.json";
        let loopback = "127.0.0.1:8111".parse().unwrap();
        assert!(fixture_source(source.into(), loopback, false).is_none());
        let selected = fixture_source(source.into(), loopback, true);
        if cfg!(feature = "test-harness") {
            assert_eq!(selected.unwrap().as_str(), source);
        } else {
            assert!(selected.is_none());
        }
        for bad in [
            "18-lab-dev/vosu-tools",
            "https://catalog.invalid:8112/fixtures/catalog-v2.json",
            "https://CATALOG.INVALID:8111/fixtures/catalog-v2.json",
            "https://catalog.invalid:8111/fixtures/project",
            "https://outside.example:8111/fixtures/catalog-v2.json",
        ] {
            assert!(
                fixture_source(bad.into(), loopback, true).is_none(),
                "{bad}"
            );
        }
        assert!(fixture_source(source.into(), "192.0.2.1:8111".parse().unwrap(), true).is_none());
    }
    #[test]
    fn sources_parse_all_forms_without_authority_aliases() {
        for source in [
            "18-lab-dev/vosu-tools",
            "18-lab-dev/vosu-tools@v2/rc+1",
            "https://github.com/18-lab-dev/vosu-tools.git/",
            "https://gitlab.com/group/nested/project",
            "https://host.example/a/catalog-v2.json",
        ] {
            let parsed = SourceIdentity::parse(source).unwrap();
            assert_eq!(parsed.identity.as_str(), source);
            assert_eq!(parsed.identity.is_official(), source == OFFICIAL_SOURCES[0]);
        }
        let parsed = SourceIdentity::parse("https://gitlab.com/group/nested/project.git/").unwrap();
        assert!(
            matches!(parsed.location, SourceLocation::Repository { project_path, .. } if project_path == "group/nested/project")
        );
    }
    #[test]
    fn sources_reject_unsafe_and_ambiguous_forms() {
        for source in [
            "http://host/a/b",
            "https://u:p@host/a/b",
            "https://@host/a/b",
            "https://host/a/b?",
            "https://host/a/b#",
            "https://host/a/../b",
            "https://host/a/b//",
            "a/b/c",
            "a/b@",
            "a/b@@tag",
            " a/b",
            "https://host/a/%2f",
        ] {
            assert!(SourceIdentity::parse(source).is_err(), "{source}");
        }
    }
    #[test]
    fn sources_static_suffix_is_exact() {
        assert!(matches!(
            SourceIdentity::parse("https://host/a/catalog-v2.json")
                .unwrap()
                .location,
            SourceLocation::Static { .. }
        ));
        for source in [
            "https://host/a/catalog-v2.json/",
            "https://host/a/Catalog-v2.json",
            "https://host/a/catalog-v2.json.git",
        ] {
            assert!(matches!(
                SourceIdentity::parse(source).unwrap().location,
                SourceLocation::Repository { .. }
            ));
        }
    }
}

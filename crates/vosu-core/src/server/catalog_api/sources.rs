//! Sources, write-only credentials, dedicated trust/private-network flags,
//! explicit refresh and the validated entry listing.
use super::*;
use crate::catalog::{
    credentials::{CredentialAvailability, CredentialInput},
    runtime::RevocationReport,
    sources::{SourceIdentity, SourceInput, SourceLocation, SourceRecord},
};
use serde::Deserialize;
use serde_json::{Value, json};
use vosu_plugin_api::manifest::{HostSupport, PackageKind, Runtime};

/// A6b.4: replacement carries exact source strings, enabled flags and their
/// order (array position) only. Any other field, `trusted` and
/// `private_network` included, is a 400 before anything changes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    sources: Vec<SourceInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustBody {
    trusted: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateBody {
    allowed: bool,
}

pub(super) fn kind_name(kind: PackageKind) -> &'static str {
    match kind {
        PackageKind::Plugin => "plugin",
        PackageKind::Tool => "tool",
        PackageKind::Overlay => "overlay",
    }
}

fn form(identity: &SourceIdentity) -> &'static str {
    match SourceIdentity::parse(identity.as_str()).map(|parsed| parsed.location) {
        Ok(SourceLocation::Static { .. }) => "static",
        Ok(SourceLocation::GitHub { .. }) => "github",
        Ok(SourceLocation::Repository { .. }) => "repository",
        Err(_) => "invalid",
    }
}

fn view(inner: &Inner) -> Result<Value, ApiError> {
    let fetch = inner.runtime().fetch().ok_or_else(ApiError::disabled)?;
    let (generation, records) = fetch.sources().snapshot()?;
    let snapshot = fetch.sources().candidate_snapshot().ok().flatten();
    let sources: Vec<Value> = records
        .iter()
        .map(|record| {
            let presence = fetch.credential_presence(record.identity.as_str());
            let fetched = snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .sources()
                    .iter()
                    .find(|source| source.source().identity == record.identity)
                    .map(|source| source.catalog().map(|catalog| catalog.entries().count()))
            });
            let (status, message, entries) = match fetched {
                _ if !record.enabled => ("disabled", None, 0),
                Some(Ok(entries)) => ("ok", None, entries),
                // Static, redacted S0 failure text; never an upstream body.
                Some(Err(failure)) => ("failed", Some(failure.to_string()), 0),
                None => ("pending", None, 0),
            };
            let form = form(&record.identity);
            let fixture = record.identity.is_official_fixture();
            let view = json!({
                "source": record.identity,
                "enabled": record.enabled,
                "order": record.order,
                "official": record.identity.is_official(),
                "trusted": record.trusted || fixture,
                "private_network": record.private_network,
                "has_credential": presence.has_credential,
                "credential_available":
                    presence.availability == CredentialAvailability::Available,
                "form": form,
                "credential_kind": if form == "static" { "basic" } else { "token" },
                "status": status,
                "message": message,
                "entries": entries,
            });
            super::mark_fixture(view, fixture)
        })
        .collect();
    Ok(json!({
        "generation": generation.0,
        "fetched": snapshot.is_some(),
        "sources": sources,
    }))
}

fn revocation(report: &RevocationReport) -> Value {
    json!({
        "approvals_cleared": report.approvals_cleared,
        "pending_restart": report.pending_restart,
        "stop_process": report.stop_process,
    })
}

pub(super) fn list(inner: &Inner) -> ApiResult {
    Ok(json(StatusCode::OK, &view(inner)?))
}

pub(super) async fn replace(inner: &Inner, body: &[u8]) -> ApiResult {
    let Replacement { sources } = parse(body)?;
    let report = inner.runtime().replace_sources(sources).await?;
    let mut value = view(inner)?;
    value["revocation"] = revocation(&report);
    Ok(json(StatusCode::OK, &value))
}

/// The exact identity a decoded segment names, or 404: a string the parser
/// refuses can never be a configured source.
fn identity(source: &str) -> Result<SourceIdentity, ApiError> {
    SourceIdentity::parse(source)
        .map(|parsed| parsed.identity)
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "catalog source not found"))
}

fn configured(inner: &Inner, source: &str) -> Result<SourceRecord, ApiError> {
    let fetch = inner.runtime().fetch().ok_or_else(ApiError::disabled)?;
    let (_, records) = fetch.sources().snapshot()?;
    records
        .into_iter()
        .find(|record| record.identity.as_str() == source)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "catalog source not found"))
}

/// GET reports presence only; POST/DELETE answer 204 with no body. The
/// decoded segment is the verbatim credential key: `a%2Fb` is `a/b`, a raw
/// `a/b` never reaches here, and `a%252Fb` is the distinct string `a%2Fb`.
pub(super) async fn credential(
    inner: &Inner,
    method: &Method,
    source: &str,
    body: &[u8],
) -> ApiResult {
    let record = configured(inner, source)?;
    let runtime = inner.runtime();
    if *method == Method::GET {
        let fetch = runtime.fetch().ok_or_else(ApiError::disabled)?;
        let presence = fetch.credential_presence(record.identity.as_str());
        return Ok(json(
            StatusCode::OK,
            &json!({
                "has_credential": presence.has_credential,
                "available": presence.availability == CredentialAvailability::Available,
            }),
        ));
    }
    if *method == Method::DELETE {
        runtime.delete_credential(record.identity.as_str()).await?;
        return Ok(no_content());
    }
    let input = CredentialInput::from_body(body).map_err(CatalogError::from)?;
    runtime
        .set_credential(record.identity.as_str(), input)
        .await?;
    Ok(no_content())
}

/// The dashboard shows the exact normative warning immediately before every
/// false→true request; the backend cannot attest that, accepts no
/// `warning_shown` claim (unknown fields are refused) and relies only on the
/// dispatch provenance/CSRF check that already ran.
pub(super) async fn trust(inner: &Inner, source: &str, body: &[u8]) -> ApiResult {
    let TrustBody { trusted } = parse(body)?;
    let report = inner
        .runtime()
        .set_trust(identity(source)?, trusted)
        .await?;
    let mut value = view(inner)?;
    value["revocation"] = revocation(&report);
    Ok(json(StatusCode::OK, &value))
}

pub(super) async fn private_network(inner: &Inner, source: &str, body: &[u8]) -> ApiResult {
    let PrivateBody { allowed } = parse(body)?;
    inner
        .runtime()
        .set_private_network(identity(source)?, allowed)
        .await?;
    Ok(json(StatusCode::OK, &view(inner)?))
}

pub(super) async fn refresh(inner: &Inner) -> ApiResult {
    inner
        .services
        .network
        .refresh(inner.runtime(), &inner.services.cancel)
        .await?;
    Ok(json(StatusCode::OK, &view(inner)?))
}

/// Every validated candidate of the current snapshot, shadowed alternatives
/// included, with the facts the install UI needs. No package URL or hash.
pub(super) fn entries(inner: &Inner) -> ApiResult {
    let runtime = inner.runtime();
    let fetch = runtime.fetch().ok_or_else(ApiError::disabled)?;
    let snapshot = fetch.sources().candidate_snapshot()?;
    let limit = (inner.services.package_limit)();
    let installed = runtime
        .installed()
        .map(|store| store.snapshot().records)
        .unwrap_or_default();
    let target = runtime.host_facts().target.as_str();
    let entries: Vec<Value> = snapshot
        .iter()
        .flat_map(|snapshot| snapshot.candidates())
        .map(|candidate| {
            let entry = candidate.entry;
            let package_target = if candidate.kind == PackageKind::Plugin {
                target
            } else {
                "any"
            };
            let size = entry
                .packages
                .get(package_target)
                .map(|package| package.size);
            let record = installed
                .iter()
                .find(|record| record.package_kind() == candidate.kind && record.id == entry.id);
            let fixture = candidate.source.identity.is_official_fixture();
            let view = json!({
                "kind": kind_name(candidate.kind),
                "id": entry.id,
                "name": entry.name,
                "version": entry.version,
                "description": entry.description,
                "author": entry.author,
                "homepage": entry.homepage,
                "icon_url": entry.icon_url,
                "banner_url": entry.banner_url,
                "preview": entry.preview,
                "resolution": entry.resolution,
                "source": candidate.source.identity,
                "official": candidate.source.identity.is_official(),
                "source_trusted": candidate.source.trusted || fixture,
                "runtime": entry.runtime.map(|runtime| match runtime {
                    Runtime::Process => "process",
                    Runtime::InProcess => "in-process",
                }),
                "game": entry.matcher.is_some(),
                "requires": entry.requires.iter().map(|r| &r.id).collect::<Vec<_>>(),
                "host_supported": candidate.support.host == HostSupport::Supported,
                "target_supported": candidate.support.target,
                "too_large": match (size, limit) {
                    (Some(size), Some(limit)) => size > limit,
                    _ => true,
                },
                "installed": record.map(|record| super::mark_fixture(json!({
                    "version": record.version,
                    "source": record.source,
                    "installed_for": record.installed_for,
                }), record.fixture)),
            });
            super::mark_fixture(view, fixture)
        })
        .collect();
    Ok(json(
        StatusCode::OK,
        &json!({"fetched": snapshot.is_some(), "entries": entries}),
    ))
}

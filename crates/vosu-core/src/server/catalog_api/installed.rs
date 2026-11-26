//! Installed catalog packages, U0 uninstall preview/start/promote and the
//! catalog status (disabled stores, pending restart, last load reports).
use super::*;
use crate::catalog::{
    install::{InstalledKind, LoadStatus},
    installed::UninstallRequest,
    sources::SourceIdentity,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    kind: InstalledKind,
    id: String,
    source: SourceIdentity,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UninstallBody {
    kind: InstalledKind,
    id: String,
    source: SourceIdentity,
    /// Required: the user explicitly keeps or removes the target's data.
    remove_data: bool,
}

fn uninstaller(inner: &Inner) -> Result<&Arc<UninstallService>, ApiError> {
    inner
        .services
        .uninstall
        .as_ref()
        .ok_or_else(ApiError::disabled)
}

fn load_status(status: &LoadStatus) -> (&'static str, Option<String>) {
    match status {
        LoadStatus::Loaded => ("loaded", None),
        LoadStatus::WebRescanned => ("web-rescanned", None),
        LoadStatus::RequiresM5ProcessRuntime => (
            "requires-process-runtime",
            Some("installed; the process plugin runtime is not available in this build".into()),
        ),
        LoadStatus::Refused(error) => ("refused", Some(error.to_string())),
        LoadStatus::Failed(error) => ("failed", Some(error.clone())),
    }
}

/// `GET /api/catalog/status`.
pub(super) fn status(inner: &Inner) -> ApiResult {
    let runtime = inner.runtime();
    let status = runtime.status();
    let reports: Vec<Value> = runtime
        .executor()
        .map(|executor| executor.load_reports())
        .unwrap_or_default()
        .iter()
        .map(|report| {
            let (status, detail) = load_status(&report.status);
            json!({"kind": report.kind, "id": report.id, "status": status, "detail": detail})
        })
        .collect();
    Ok(json(
        StatusCode::OK,
        &json!({
            "sources_available": status.sources.is_none(),
            "installed_available": status.installed.is_none(),
            "disabled": status.sources.iter().map(ToString::to_string)
                .chain(status.installed.iter().map(ToString::to_string))
                .collect::<Vec<_>>(),
            "pending_restart": runtime.pending_restart(),
            "restart_required": (inner.services.restart_required)(),
            "load_reports": reports,
            "installed_generation": runtime.installed_generation(),
        }),
    ))
}

/// `GET /api/catalog/installed`: committed records, never artifact hashes.
pub(super) fn list(inner: &Inner) -> ApiResult {
    let store = inner.runtime().installed().ok_or_else(ApiError::disabled)?;
    let document = store.snapshot();
    let packages: Vec<Value> = document
        .records
        .iter()
        .map(|record| {
            json!({
                "kind": record.kind,
                "id": record.id,
                "version": record.version,
                "source": record.source,
                "official": record.source.is_official(),
                "installed_for": record.installed_for,
                "runtime": record.artifact.as_ref().map(|artifact| match artifact.runtime {
                    vosu_plugin_api::manifest::Runtime::Process => "process",
                    vosu_plugin_api::manifest::Runtime::InProcess => "in-process",
                }),
                "approved": record.approval.is_some(),
            })
        })
        .collect();
    Ok(json(
        StatusCode::OK,
        &json!({"generation": document.generation, "packages": packages}),
    ))
}

/// Advisory closure review (no gate, no job): explicit target plus every
/// dependency-only plugin it leaves unreferenced, dropped optional grants,
/// or a 409 naming the dependents that still require it.
pub(super) async fn preview(inner: &Inner, body: &[u8]) -> ApiResult {
    let Target { kind, id, source } = parse(body)?;
    let plan = uninstaller(inner)?
        .preview(&UninstallRequest {
            kind,
            id,
            source,
            remove_data: false,
        })
        .await?;
    Ok(json(StatusCode::OK, &plan))
}

/// Starts one gated removal job; refusals return here with nothing changed.
pub(super) async fn uninstall(inner: &Inner, body: &[u8]) -> ApiResult {
    let UninstallBody {
        kind,
        id,
        source,
        remove_data,
    } = parse(body)?;
    let response = uninstaller(inner)?
        .start(
            inner.runtime().plans(),
            UninstallRequest {
                kind,
                id,
                source,
                remove_data,
            },
        )
        .await?;
    Ok(json(StatusCode::OK, &response))
}

/// Explicit request for an installed dependency: records-only promotion.
pub(super) async fn promote(inner: &Inner, body: &[u8]) -> ApiResult {
    let Target { kind, id, source } = parse(body)?;
    let promoted = uninstaller(inner)?
        .promote(inner.runtime().plans(), kind, &id, &source)
        .await?;
    inner.runtime().refresh_offers();
    Ok(json(StatusCode::OK, &json!({"promoted": promoted})))
}

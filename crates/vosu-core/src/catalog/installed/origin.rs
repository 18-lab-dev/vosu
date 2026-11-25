//! Requested/dependency origin. `installed_for` is persisted with each record
//! (I0); this is the only other writer: an explicit request for an installed
//! dependency promotes it to requested without downloading anything. Nothing
//! ever demotes a requested package, and no reference count is stored.
use super::UninstallError;
use crate::catalog::{
    install::{InstallLayout, InstalledDocument, InstalledFor, InstalledKind, publish_records},
    sources::SourceIdentity,
};

/// Publish the promoted records as one atomic replacement with the next
/// generation (no directory changes, so no journal). Returns `None` when the
/// package is already requested. The caller holds the catalog operation gate
/// and has already run install recovery.
pub(super) fn promote(
    layout: &InstallLayout,
    current: &InstalledDocument,
    kind: InstalledKind,
    id: &str,
    source: &SourceIdentity,
) -> Result<Option<InstalledDocument>, UninstallError> {
    let index = current
        .records
        .iter()
        .position(|r| r.kind == kind && r.id == id && r.source == *source)
        .ok_or_else(|| UninstallError::NotInstalled(id.to_owned()))?;
    if current.records[index].installed_for == InstalledFor::Requested {
        return Ok(None);
    }
    let mut document = current.clone();
    document.generation = current
        .generation
        .checked_add(1)
        .ok_or(crate::catalog::install::InstallError::RecordsInvalid)?;
    document.records[index].installed_for = InstalledFor::Requested;
    publish_records(layout, &document)?;
    Ok(Some(document))
}

//! A5.5 management of catalog-installed process packages. The catalog operation
//! gate serializes these actions with source mutation and directory replacement.
use super::*;
use crate::catalog::{
    install::{InstalledRecord, LoadStatus, RuntimeHandoff},
    trust::TrustTier,
};
use crate::plugin::process::control::StartupStage;
use vosu_plugin_api::manifest::PackageKind;

fn stale() -> Response {
    refusal(
        StatusCode::CONFLICT,
        "approval_stale",
        "the package changed since it was shown; review it again",
    )
}

fn not_applicable() -> Response {
    refusal(
        StatusCode::BAD_REQUEST,
        "approval_not_applicable",
        "this process plugin does not need explicit approval",
    )
}

fn not_saved(error: impl std::fmt::Display) -> Response {
    refusal(
        StatusCode::INTERNAL_SERVER_ERROR,
        "approval_not_saved",
        format!("the approval was not saved: {error}"),
    )
}

impl PluginManagement {
    pub(super) fn report_installed_start(&self, id: &str, result: &Result<(), HostError>) {
        if let Some((runtime, _)) = &self.catalog
            && let Some(executor) = runtime.executor()
        {
            let status = match result {
                Ok(()) => LoadStatus::Loaded,
                Err(HostError::ProcessStartupFailed(error))
                    if error.stage == StartupStage::Approval =>
                {
                    LoadStatus::ApprovalRequired
                }
                Err(error) => LoadStatus::Failed(error.to_string()),
            };
            executor.update_plugin_report(id, Some(status));
        }
    }

    pub(super) fn installed_approval_revocable(
        &self,
        plugin: &ManagedPlugin,
        digests: Option<Digests>,
    ) -> bool {
        let Some((runtime, _)) = &self.catalog else {
            return false;
        };
        self.installed_record(plugin).is_some_and(|record| {
            record.approval.is_some()
                && runtime
                    .process_requirement(&record)
                    .is_some_and(|requirement| {
                        requirement.needs_approval
                            || !digests
                                .is_some_and(|current| record.process_digests == Some(current))
                    })
        })
    }

    pub(super) fn installed_trust(&self, plugin: &ManagedPlugin) -> Option<&'static str> {
        if plugin.trust != Trust::Installed {
            return None;
        }
        let (runtime, _) = self.catalog.as_ref()?;
        let record = runtime.installed()?.plugin(&plugin.package.as_ref()?.id)?;
        Some(match runtime.process_requirement(&record)?.trust {
            TrustTier::Official => "official",
            TrustTier::Trusted => "trusted",
            TrustTier::User => "user",
        })
    }

    /// Revalidate changed manifest metadata before showing its disclosure or
    /// building a replacement template. A validated manifest is always the
    /// one bound by the digest passed to the gate.
    pub(super) fn updated_installed(&self, plugin: &ManagedPlugin) -> Option<LoadedPlugin> {
        let package = plugin.package.as_ref()?;
        let process = package.process.as_ref()?;
        let manifest = Digests::installed_manifest(process.root()).ok()?;
        if manifest.0 == *process.manifest_sha256() {
            return None;
        }
        let (runtime, handoff) = self.catalog.as_ref()?;
        let store = runtime.installed()?;
        let record = store.plugin(&package.id)?;
        let directory = store
            .layout()
            .destination(PackageKind::Plugin, &package.id)
            .ok()?;
        if record.provenance().as_ref() != process.installed_provenance() {
            return None;
        }
        handoff.validate_process(&record, &directory).ok()
    }

    fn installed_record(&self, plugin: &ManagedPlugin) -> Option<InstalledRecord> {
        let (runtime, _) = self.catalog.as_ref()?;
        let package = plugin.package.as_ref()?;
        let process = package.process.as_ref()?;
        let record = runtime.installed()?.plugin(&package.id)?;
        (record.provenance().as_ref() == process.installed_provenance()).then_some(record)
    }

    pub(super) fn approve_installed(&self, plugin: &ManagedPlugin, body: &[u8]) -> Response {
        let Some(record) = self.installed_record(plugin) else {
            return stale();
        };
        let Some((runtime, handoff)) = &self.catalog else {
            return stale();
        };
        let Some(requirement) = runtime.process_requirement(&record) else {
            return stale();
        };
        let requested = match approval_input(body) {
            Ok(digests) => digests,
            Err(message) => return invalid_approval_request(message),
        };
        let replacement = self.updated_installed(plugin);
        let process = match replacement.as_ref().map(LoadedPlugin::execution) {
            Some(Execution::Process(process)) => Some(process),
            _ => plugin
                .package
                .as_ref()
                .and_then(|package| package.process.as_ref()),
        };
        let Some(process) = process else {
            return stale();
        };
        let Ok(current) = Digests::of(process) else {
            return stale();
        };
        if current != requested || current.manifest_sha256.0 != *process.manifest_sha256() {
            return stale();
        }
        if !requirement.needs_approval && record.process_digests == Some(current) {
            return not_applicable();
        }
        let Some(store) = runtime.installed() else {
            return stale();
        };
        let active = self
            .host
            .process_status(plugin.instance)
            .is_some_and(|status| {
                matches!(
                    status.phase,
                    ProcessPhase::Starting | ProcessPhase::Running | ProcessPhase::Backoff { .. }
                )
            });
        if replacement.is_none() && record.matches_approval(current) && active {
            return respond(StatusCode::OK, self.view(plugin, Some(current)));
        }
        if let Err(error) = handoff.stop_process(&record.id) {
            return refusal(StatusCode::CONFLICT, "busy", error);
        }
        if let Err(error) = store.approve_process(&record.id, current) {
            if active {
                handoff.resume_process(&record.id);
            }
            return not_saved(error);
        }
        let updated;
        let (plugin, status) = if let Some(replacement) = replacement {
            handoff.remove_process(&record.id);
            let (managed, status) = handoff.start_process(&replacement);
            updated = managed;
            (updated.as_ref(), status)
        } else {
            let status = match self.host.restart_process_instance(plugin.instance) {
                Ok(()) => LoadStatus::Loaded,
                Err(HostError::ProcessStartupFailed(error))
                    if error.stage == StartupStage::Approval =>
                {
                    LoadStatus::ApprovalRequired
                }
                Err(error) => LoadStatus::Failed(error.to_string()),
            };
            (plugin, status)
        };
        runtime.refresh_offers();
        if let Some(executor) = runtime.executor() {
            executor.update_plugin_report(&record.id, Some(status));
        }
        respond(StatusCode::OK, self.view(plugin, Some(current)))
    }

    pub(super) fn revoke_installed(&self, plugin: &ManagedPlugin) -> Response {
        let Some(record) = self.installed_record(plugin) else {
            return stale();
        };
        let Some((runtime, handoff)) = &self.catalog else {
            return stale();
        };
        let Some(requirement) = runtime.process_requirement(&record) else {
            return stale();
        };
        if !requirement.needs_approval
            && (record.approval.is_none()
                || plugin
                    .package
                    .as_ref()
                    .and_then(|package| package.process.as_ref())
                    .and_then(|process| Digests::of(process).ok())
                    .is_some_and(|current| record.process_digests == Some(current)))
        {
            return not_applicable();
        }
        let Some(store) = runtime.installed() else {
            return stale();
        };
        if let Err(error) = store.clear_approvals(&[&record.id]) {
            return not_saved(error);
        }
        if let Err(error) = handoff.stop_process(&record.id) {
            return refusal(StatusCode::CONFLICT, "busy", error);
        }
        runtime.refresh_offers();
        if let Some(executor) = runtime.executor() {
            executor.update_plugin_report(&record.id, Some(LoadStatus::ApprovalRequired));
        }
        respond(StatusCode::OK, self.view(plugin, None))
    }
}

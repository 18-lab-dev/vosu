//! Pure catalog policy. No runtime execution, source writes, or UI attestation.
//!
//! W4 must serialize source mutation, installed approval revocation, plan
//! confirmation and final load under the same operation authority. Obtain a
//! fresh S0 snapshot at each boundary; do not retain a successful decision as
//! load authority. M5 executes Process only in a real child, never via dlopen in
//! the daemon. Runtime callbacks run after policy publication, outside locks.
use super::{
    approvals::{Approval, Artifact},
    sources::{SourceGeneration, SourceRecord},
};
use serde::{Deserialize, Serialize};
use vosu_plugin_api::manifest::Runtime;

/// H0 displays this immediately before EVERY false-to-true grant. No remembered
/// warning or caller-supplied attestation can replace dashboard provenance+CSRF.
pub const TRUST_WARNING: &str = "Plugins from this source can run in-process, with the same memory access as vosu itself. Only trust sources you control or know well.";

/// Dedicated route body, not a source-list input. The route must check dashboard
/// listener, same-origin provenance and CSRF before invoking S0's set_trust.
/// Core cannot attest what a human read; there is intentionally no warning flag.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustChange {
    pub trusted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustTier {
    Official,
    Trusted,
    User,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub trust: TrustTier,
    pub needs_approval: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("source is not trusted for in-process execution")]
    InProcessDenied,
    #[error("exact native artifact approval required")]
    ApprovalRequired,
    #[error("revocation pending restart")]
    RevocationPendingRestart,
    #[error("source policy changed")]
    StalePolicy,
}

/// Current S0 snapshot, shared by plan/confirm/load. Credential presence, private
/// permission and cached catalog trust labels are deliberately not arguments.
pub struct CurrentPolicy<'a> {
    pub generation: SourceGeneration,
    pub sources: &'a [SourceRecord],
}
impl CurrentPolicy<'_> {
    pub fn tier(&self, artifact: &Artifact) -> TrustTier {
        if artifact.source.is_official() {
            TrustTier::Official
        } else if self
            .sources
            .iter()
            .any(|source| source.identity == artifact.source && source.trusted)
        {
            TrustTier::Trusted
        } else {
            // Removed sources lose authority too; no frozen installed tier.
            TrustTier::User
        }
    }
    pub fn requirement(&self, artifact: &Artifact) -> Result<Requirement, PolicyError> {
        let trust = self.tier(artifact);
        if artifact.runtime == Runtime::InProcess && trust == TrustTier::User {
            return Err(PolicyError::InProcessDenied);
        }
        Ok(Requirement {
            trust,
            needs_approval: trust == TrustTier::User && artifact.runtime == Runtime::Process,
        })
    }
    /// P0 pins this generation and rejects stale plans rather than recomputing.
    pub fn check_generation(&self, pinned: SourceGeneration) -> Result<(), PolicyError> {
        if pinned == self.generation {
            Ok(())
        } else {
            Err(PolicyError::StalePolicy)
        }
    }
    /// Frozen W4 seam: call immediately before execution, under the shared
    /// operation gate, using verified installed metadata and current S0 policy.
    /// A return value is NOT a durable permit or a substitute for M5 confinement.
    pub fn authorize_load(&self, installed: &InstalledPolicy) -> Result<Requirement, PolicyError> {
        if installed.pending_restart {
            return Err(PolicyError::RevocationPendingRestart);
        }
        let requirement = self.requirement(&installed.artifact)?;
        if requirement.needs_approval
            && !installed
                .approval
                .as_ref()
                .is_some_and(|approval| approval.matches(&installed.artifact))
        {
            return Err(PolicyError::ApprovalRequired);
        }
        Ok(requirement)
    }
}

/// Effects for M5's execution backend, not a claim that policy stops code itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revocation {
    None,
    StopProcess,
    PendingRestart,
}
impl Revocation {
    pub fn status(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::StopProcess => Some("approval required"),
            Self::PendingRestart => Some("revocation pending restart"),
        }
    }
}

/// Per-installed-artifact policy state. Approval is persisted in I0's record;
/// pending_restart is daemon-lifetime state and clears ONLY on daemon restart,
/// never on source re-trust, plugin rescan, or repeated revocation.
#[derive(Debug, Clone)]
pub struct InstalledPolicy {
    artifact: Artifact,
    approval: Option<Approval>,
    pending_restart: bool,
}
impl InstalledPolicy {
    pub fn new(artifact: Artifact, approval: Option<Approval>) -> Self {
        Self {
            artifact,
            approval,
            pending_restart: false,
        }
    }
    /// Re-applies composition's daemon-lifetime marker to a fresh load-time
    /// policy. It can only add the refusal; `false` never clears a marker.
    pub fn with_pending_restart(mut self, pending: bool) -> Self {
        self.pending_restart |= pending;
        self
    }
    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }
    pub fn approval(&self) -> Option<&Approval> {
        self.approval.as_ref()
    }
    pub fn disabled_for_next_load(&self) -> bool {
        self.pending_restart
    }
    pub fn status(&self) -> Option<&'static str> {
        self.pending_restart.then_some("revocation pending restart")
    }
    /// Called for every installed artifact when S0 publishes a trust change or
    /// removes a source. Persist cleared approvals even for nonrunning plugins;
    /// otherwise an older approval could silently resurrect after untrust.
    pub fn source_changed(
        &mut self,
        previous: &CurrentPolicy<'_>,
        current: &CurrentPolicy<'_>,
        running: bool,
    ) -> Revocation {
        if previous.tier(&self.artifact) != TrustTier::User
            && current.tier(&self.artifact) == TrustTier::User
        {
            self.revoke(running)
        } else {
            Revocation::None
        }
    }
    /// Explicit per-artifact approval revocation. M5 must stop a running process
    /// before allowing a fresh approval/load; native in-process code stays in
    /// the address space, honestly pending user restart.
    pub fn revoke(&mut self, running: bool) -> Revocation {
        self.approval = None;
        if !running {
            return Revocation::None;
        }
        match self.artifact.runtime {
            Runtime::Process => Revocation::StopProcess,
            Runtime::InProcess => {
                self.pending_restart = true;
                Revocation::PendingRestart
            }
        }
    }
}

#[cfg(test)]
mod tests;

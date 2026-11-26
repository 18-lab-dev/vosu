//! Job-owned artifact declaration and download admission (AF.12).

use std::sync::Arc;
use std::time::Instant;

use super::{JobRegistry, Principal};
use crate::actions::artifacts::{
    ArtifactCell, ArtifactDeclaration, ArtifactDownload, ArtifactError, ArtifactRefusal,
    MAX_ARTIFACTS_PER_JOB, MAX_LIVE_ARTIFACTS, mint_id,
};

impl JobRegistry {
    /// A registry whose terminal jobs (and so their artifacts) are
    /// retained for `retention` instead of five minutes.
    #[cfg(any(test, feature = "test-harness"))]
    pub fn with_retention_for_tests(retention: std::time::Duration) -> Self {
        let registry = Self::new();
        registry.inner.lock().retention = retention;
        registry
    }

    /// Records one artifact for the still-running job `job`, returning its
    /// opaque id. Only the invocation's own sink names `job`; a job that
    /// already ended, or whose launch session was invalidated, refuses.
    /// Dropping a refused declaration drops its release notice unsent.
    pub fn declare_artifact(
        &self,
        job: &str,
        declaration: ArtifactDeclaration,
    ) -> Result<String, ArtifactRefusal> {
        let id = mint_id().ok_or(ArtifactRefusal::TooManyArtifacts)?;
        let mut state = self.inner.lock();
        state.cleanup(Instant::now());
        let live = state
            .entries
            .values()
            .flat_map(|entry| &entry.artifacts)
            .filter(|cell| !cell.is_released())
            .count();
        let entry = state
            .entries
            .get_mut(job)
            .ok_or(ArtifactRefusal::InvocationFinished)?;
        if entry.snapshot.state.is_terminal() || entry.revoked {
            return Err(ArtifactRefusal::InvocationFinished);
        }
        if matches!(entry.owner, Principal::Plugin(_)) {
            return Err(ArtifactRefusal::CallerNotSupported);
        }
        if entry.artifacts.len() == MAX_ARTIFACTS_PER_JOB || live >= MAX_LIVE_ARTIFACTS {
            return Err(ArtifactRefusal::TooManyArtifacts);
        }
        entry
            .artifacts
            .push(Arc::new(ArtifactCell::new(id.clone(), job, declaration)));
        Ok(id)
    }

    /// Admits one download of `artifact` of `job` for `principal`: the job
    /// must be visible to it (otherwise `UnknownJob`, as for the job route)
    /// and must have succeeded and listed the artifact. A released artifact
    /// (expiry wakes, revocation, cancel) or one whose provider instance is
    /// gone is `Released`. The returned download holds one slot of the
    /// per-artifact and global bounds until dropped.
    pub fn open_artifact(
        &self,
        principal: &Principal,
        job: &str,
        artifact: &str,
    ) -> Result<ArtifactDownload, ArtifactError> {
        let cell = {
            let mut state = self.inner.lock();
            state.cleanup(Instant::now());
            Self::listed(&state, principal, job, artifact)?
        };
        // Liveness asks the plugin host: never under the registry lock.
        let live = cell
            .host()
            .is_some_and(|host| host.provider_live(cell.provider()));
        let mut state = self.inner.lock();
        state.cleanup(Instant::now());
        let current = Self::listed(&state, principal, job, artifact)?;
        if !Arc::ptr_eq(&current, &cell) {
            return Err(ArtifactError::UnknownArtifact);
        }
        if !live {
            // Instance loss, disable or uninstall: nothing is sent to a
            // lost instance, and the artifact stays listed as released.
            let _ = cell.release(false);
            return Err(ArtifactError::Released);
        }
        cell.reserve(&self.inner.downloads)
    }

    fn listed(
        state: &super::RegistryState,
        principal: &Principal,
        job: &str,
        artifact: &str,
    ) -> Result<Arc<ArtifactCell>, ArtifactError> {
        let entry = state
            .entries
            .get(job)
            .filter(|entry| entry.visible_to(principal))
            .ok_or(ArtifactError::UnknownJob)?;
        if entry.snapshot.state != super::JobState::Succeeded {
            return Err(ArtifactError::UnknownArtifact);
        }
        entry
            .artifacts
            .iter()
            .find(|cell| cell.id == artifact)
            .cloned()
            .ok_or(ArtifactError::UnknownArtifact)
    }
}

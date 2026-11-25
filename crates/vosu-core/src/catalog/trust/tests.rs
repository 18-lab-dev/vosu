use super::*;
use crate::catalog::sources::{
    SourceIdentity, SourceInput, SourceLifecycle, SourceStore, SourceStoreError,
};
use std::sync::Arc;

fn artifact(source: &str, runtime: Runtime) -> Artifact {
    Artifact {
        id: "acme.reader".into(),
        source: SourceIdentity::parse(source).unwrap().identity,
        version: "2.3.4".into(),
        sha256: [0x41; 32],
        runtime,
    }
}
fn record(source: &str, trusted: bool) -> SourceRecord {
    SourceRecord {
        identity: SourceIdentity::parse(source).unwrap().identity,
        enabled: true,
        trusted,
        private_network: false,
        order: 0,
    }
}
fn policy(sources: &[SourceRecord]) -> CurrentPolicy<'_> {
    CurrentPolicy {
        generation: SourceGeneration(17),
        sources,
    }
}
const USER: &str = "acme/plugins";

#[test]
fn official_identity_is_exact_not_fetch_alias_or_stored_flag() {
    let official = artifact("18-lab-dev/vosu-tools", Runtime::InProcess);
    assert_eq!(policy(&[]).tier(&official), TrustTier::Official);
    assert!(policy(&[]).requirement(&official).is_ok());
    for alias in [
        "18-LAB-dev/vosu-tools",
        "18-lab-dev/vosu-tools@latest",
        "https://github.com/18-lab-dev/vosu-tools",
        "https://github.com/18-lab-dev/vosu-tools.git/",
    ] {
        let artifact = artifact(alias, Runtime::InProcess);
        assert_eq!(policy(&[]).tier(&artifact), TrustTier::User);
        assert_eq!(
            policy(&[]).requirement(&artifact),
            Err(PolicyError::InProcessDenied)
        );
        assert_eq!(
            policy(&[record(alias, true)]).tier(&artifact),
            TrustTier::Trusted
        );
    }
}

#[test]
fn private_permission_and_verified_hash_do_not_grant_trust() {
    let artifact = artifact(USER, Runtime::Process);
    let mut source = record(USER, false);
    source.private_network = true;
    assert_eq!(
        policy(&[source.clone()]).requirement(&artifact),
        Ok(Requirement {
            trust: TrustTier::User,
            needs_approval: true,
        })
    );
    source.trusted = true;
    assert_eq!(
        policy(&[source]).requirement(&artifact),
        Ok(Requirement {
            trust: TrustTier::Trusted,
            needs_approval: false,
        })
    );
}

#[test]
fn approval_cannot_override_in_process_refusal_or_change_runtime() {
    let artifact = artifact(USER, Runtime::InProcess);
    let installed = InstalledPolicy::new(artifact.clone(), Some(Approval::grant(&artifact)));
    assert_eq!(
        policy(&[]).authorize_load(&installed),
        Err(PolicyError::InProcessDenied)
    );
    assert_eq!(installed.artifact().runtime, Runtime::InProcess);
}

#[test]
fn process_user_needs_approval_but_trusted_and_official_do_not() {
    let artifact = artifact(USER, Runtime::Process);
    let installed = InstalledPolicy::new(artifact.clone(), None);
    assert_eq!(
        policy(&[]).authorize_load(&installed),
        Err(PolicyError::ApprovalRequired)
    );
    assert!(
        policy(&[record(USER, true)])
            .authorize_load(&installed)
            .is_ok()
    );
    let approved = InstalledPolicy::new(artifact.clone(), Some(Approval::grant(&artifact)));
    assert!(policy(&[]).authorize_load(&approved).is_ok());
    let official = InstalledPolicy::new(artifact_for_official(), None);
    assert!(policy(&[]).authorize_load(&official).is_ok());
}
fn artifact_for_official() -> Artifact {
    artifact("18-lab-dev/vosu-overlays", Runtime::Process)
}

#[test]
fn approval_binds_each_id_source_version_hash_and_runtime_dimension() {
    let approved = artifact(USER, Runtime::Process);
    let approval = Approval::grant(&approved);
    let mut alternatives = vec![approved.clone(); 5];
    alternatives[0].id = "acme.other".into();
    alternatives[1].source = SourceIdentity::parse("acme/other").unwrap().identity;
    alternatives[2].version = "2.3.5".into();
    alternatives[3].sha256 = [0x42; 32];
    alternatives[4].runtime = Runtime::InProcess;
    assert!(approval.matches(&approved));
    for changed in alternatives {
        assert!(!approval.matches(&changed));
        let installed = InstalledPolicy::new(changed.clone(), Some(approval.clone()));
        assert_eq!(
            policy(&[]).authorize_load(&installed),
            Err(if changed.runtime == Runtime::Process {
                PolicyError::ApprovalRequired
            } else {
                PolicyError::InProcessDenied
            })
        );
    }
}

#[test]
fn persisted_approval_roundtrips_without_becoming_an_id_only_grant() {
    let artifact = artifact(USER, Runtime::Process);
    let encoded = serde_json::to_vec(&Approval::grant(&artifact)).unwrap();
    let approval: Approval = serde_json::from_slice(&encoded).unwrap();
    assert!(approval.matches(&artifact));
    let mut changed = artifact;
    changed.sha256 = [0x43; 32];
    assert!(!approval.matches(&changed));
}

#[test]
fn untrust_stops_process_and_discards_even_an_older_explicit_approval() {
    let old = [record(USER, true)];
    let new = [record(USER, false)];
    let artifact = artifact(USER, Runtime::Process);
    let mut installed = InstalledPolicy::new(artifact.clone(), Some(Approval::grant(&artifact)));
    assert!(policy(&old).authorize_load(&installed).is_ok());
    let effect = installed.source_changed(&policy(&old), &policy(&new), true);
    assert_eq!(effect, Revocation::StopProcess);
    assert_eq!(effect.status(), Some("approval required"));
    assert!(installed.approval().is_none());
    assert_eq!(
        policy(&new).authorize_load(&installed),
        Err(PolicyError::ApprovalRequired)
    );
    let reapproved = InstalledPolicy::new(artifact.clone(), Some(Approval::grant(&artifact)));
    assert!(policy(&new).authorize_load(&reapproved).is_ok());
}

#[test]
fn untrust_nonrunning_process_also_clears_persisted_approval() {
    let old = [record(USER, true)];
    let artifact = artifact(USER, Runtime::Process);
    let mut installed = InstalledPolicy::new(artifact.clone(), Some(Approval::grant(&artifact)));
    assert_eq!(
        installed.source_changed(&policy(&old), &policy(&[]), false),
        Revocation::None
    );
    assert!(installed.approval().is_none());
    assert_eq!(
        policy(&[]).authorize_load(&installed),
        Err(PolicyError::ApprovalRequired)
    );
}

#[test]
fn in_process_untrust_is_disabled_pending_restart_not_unsafe_unload() {
    let old = [record(USER, true)];
    let artifact = artifact(USER, Runtime::InProcess);
    let mut installed = InstalledPolicy::new(artifact.clone(), None);
    assert!(policy(&old).authorize_load(&installed).is_ok());
    let effect = installed.source_changed(&policy(&old), &policy(&[]), true);
    assert_eq!(effect, Revocation::PendingRestart);
    assert_eq!(effect.status(), Some("revocation pending restart"));
    assert!(installed.disabled_for_next_load());
    assert_eq!(installed.status(), Some("revocation pending restart"));
    assert_eq!(
        policy(&[]).authorize_load(&installed),
        Err(PolicyError::RevocationPendingRestart)
    );
    assert_eq!(
        installed.source_changed(&policy(&[]), &policy(&old), true),
        Revocation::None
    );
    assert_eq!(
        policy(&old).authorize_load(&installed),
        Err(PolicyError::RevocationPendingRestart)
    );
    assert_eq!(installed.revoke(false), Revocation::None);
    assert!(installed.disabled_for_next_load());
    // A new daemon drops only the running-code marker, not current trust policy.
    let restarted = InstalledPolicy::new(artifact, None);
    assert_eq!(
        policy(&[]).authorize_load(&restarted),
        Err(PolicyError::InProcessDenied)
    );
    assert!(policy(&old).authorize_load(&restarted).is_ok());
}

#[test]
fn nonrunning_in_process_revocation_blocks_next_load_without_restart_claim() {
    let old = [record(USER, true)];
    let mut installed = InstalledPolicy::new(artifact(USER, Runtime::InProcess), None);
    assert_eq!(
        installed.source_changed(&policy(&old), &policy(&[]), false),
        Revocation::None
    );
    assert!(!installed.disabled_for_next_load());
    assert_eq!(installed.status(), None);
    assert_eq!(
        policy(&[]).authorize_load(&installed),
        Err(PolicyError::InProcessDenied)
    );
}

#[test]
fn explicit_process_approval_revocation_blocks_reload_and_restart() {
    let artifact = artifact(USER, Runtime::Process);
    let mut installed = InstalledPolicy::new(artifact.clone(), Some(Approval::grant(&artifact)));
    assert_eq!(installed.revoke(true), Revocation::StopProcess);
    assert_eq!(
        policy(&[]).authorize_load(&installed),
        Err(PolicyError::ApprovalRequired)
    );
    let restarted = InstalledPolicy::new(artifact, installed.approval().cloned());
    assert_eq!(
        policy(&[]).authorize_load(&restarted),
        Err(PolicyError::ApprovalRequired)
    );
}

#[test]
fn unrelated_source_change_does_not_revoke_approval() {
    let old = [record("other/plugins", true)];
    let artifact = artifact(USER, Runtime::Process);
    let mut installed = InstalledPolicy::new(artifact.clone(), Some(Approval::grant(&artifact)));
    assert_eq!(
        installed.source_changed(&policy(&old), &policy(&[]), true),
        Revocation::None
    );
    assert!(policy(&[]).authorize_load(&installed).is_ok());
}

#[test]
fn route_body_rejects_warning_attestation_and_source_policy_smuggling() {
    assert!(
        serde_json::from_str::<TrustChange>(r#"{"trusted":true}"#)
            .unwrap()
            .trusted
    );
    assert!(
        !serde_json::from_str::<TrustChange>(r#"{"trusted":false}"#)
            .unwrap()
            .trusted
    );
    for body in [
        r#"{"trusted":true,"warning_shown":true}"#,
        r#"{"trusted":true,"csrf_valid":true}"#,
        r#"{"trusted":true,"private_network":true}"#,
    ] {
        assert!(serde_json::from_str::<TrustChange>(body).is_err());
    }
    assert_eq!(
        TRUST_WARNING,
        "Plugins from this source can run in-process, with the same memory access as vosu itself. Only trust sources you control or know well."
    );
}

struct Lifecycle;
impl SourceLifecycle for Lifecycle {
    fn reconcile_sources(&self, _: &[String]) -> Result<(), SourceStoreError> {
        Ok(())
    }
}
#[test]
fn durable_source_trust_mutations_immediately_stale_plan_stamps_and_load_policy() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[SourceInput {
            source: USER.into(),
            enabled: true,
        }])
        .unwrap();
    let artifact = artifact(USER, Runtime::InProcess);
    let (untrusted_generation, untrusted_sources) = store.snapshot().unwrap();
    let before = CurrentPolicy {
        generation: untrusted_generation,
        sources: &untrusted_sources,
    };
    let installed = InstalledPolicy::new(artifact.clone(), None);
    assert_eq!(
        before.authorize_load(&installed),
        Err(PolicyError::InProcessDenied)
    );
    let trusted_generation = store.set_trust(&artifact.source, true).unwrap();
    let (_, trusted_sources) = store.snapshot().unwrap();
    let trusted = CurrentPolicy {
        generation: trusted_generation,
        sources: &trusted_sources,
    };
    assert_eq!(
        trusted.check_generation(untrusted_generation),
        Err(PolicyError::StalePolicy)
    );
    assert!(trusted.authorize_load(&installed).is_ok());
    let revoked_generation = store.set_trust(&artifact.source, false).unwrap();
    let (_, revoked_sources) = store.snapshot().unwrap();
    let revoked = CurrentPolicy {
        generation: revoked_generation,
        sources: &revoked_sources,
    };
    assert_eq!(
        revoked.check_generation(trusted_generation),
        Err(PolicyError::StalePolicy)
    );
    assert_eq!(
        revoked.authorize_load(&installed),
        Err(PolicyError::InProcessDenied)
    );
    assert!(revoked.check_generation(revoked_generation).is_ok());
    assert_eq!(
        store.set_trust(&artifact.source, false).unwrap(),
        revoked_generation
    );
}

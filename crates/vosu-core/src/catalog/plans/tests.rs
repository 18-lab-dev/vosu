use super::*;
use crate::catalog::{
    resolver::{BuiltinProviders, Candidate, Request, Snapshot},
    sources::{SourceGeneration, SourceIdentity},
    trust::CurrentPolicy,
};
use semver::Version;
use serde_json::json;
use std::sync::Mutex;
use std::{
    sync::{Arc, Barrier},
    time::Instant,
};
use vosu_plugin_api::{
    catalog::{Catalog, CatalogEntry},
    manifest::{PackageKind, PackageManifest},
};
const TARGET: &str = "x86_64-unknown-linux-gnu";
fn prerequisites() -> Prerequisites {
    Prerequisites {
        source_generation: 11,
        trust_generation: 12,
        private_generation: 13,
        credential_generation: 14,
        installed_generation: 15,
        limit_generation: 16,
        snapshot_fingerprint: [17; 32],
        application: "2.1.0".into(),
        target: TARGET.into(),
        max_package_bytes: 1_048_576,
    }
}
fn plugin(required: Option<&str>, optional: Option<&str>, size: u64) -> CatalogEntry {
    let cap = |id: &str| json!({"id":id,"version":"^1"});
    let document = json!({"catalog_version":2,"name":"Fixture","published":"2026-11-24T00:00:00Z",
        "plugins":[{"id":"acme.native","name":"Native","version":"1.2.3","host":"^2","runtime":"process",
        "provides":[{"id":"field:acme.native","version":"1.0.0","schema_version":"1.0.0","lanes":[]}],
        "requires":required.map(cap).into_iter().collect::<Vec<_>>(),
        "optional":optional.map(cap).into_iter().collect::<Vec<_>>(),
        "packages":{TARGET:{"url":"https://example.org/native.zip","sha256":"ab".repeat(32),"size":size}}}]});
    Catalog::parse(&document.to_string())
        .unwrap()
        .plugins
        .remove(0)
}
fn plan(entry: &CatalogEntry, bundled: bool) -> PinnedPlan {
    let source = SourceIdentity::parse("acme/catalog").unwrap().identity;
    let candidates = [Candidate {
        kind: PackageKind::Plugin,
        entry,
        source: &source,
        source_order: 0,
        catalog_order: 0,
        trusted: false,
    }];
    let bundled = if bundled {
        vec![PackageManifest::parse(include_str!("../../../../vosu-osu/manifest.toml")).unwrap()]
    } else {
        vec![]
    };
    let builtins = BuiltinProviders::from_registry(
        &crate::state::schema::SchemaRegistry::with_core().unwrap(),
        &bundled,
    );
    let application = Version::new(2, 1, 0);
    let snapshot = Snapshot {
        candidates: &candidates,
        installed: &[],
        builtins: &builtins,
        application: &application,
        target: TARGET,
        max_package_bytes: 1_048_576,
    };
    build(
        &snapshot,
        &[Request {
            kind: PackageKind::Plugin,
            id: "acme.native",
            source: &source,
        }],
        &CurrentPolicy {
            generation: SourceGeneration(11),
            sources: &[],
        },
        prerequisites(),
    )
}
#[derive(Default)]
struct Executor(Mutex<Vec<CatalogOperation>>);
impl CatalogExecutor for Executor {
    fn start(&self, operation: CatalogOperation) -> Result<(), String> {
        self.0.lock().unwrap().push(operation);
        Ok(())
    }
}
fn approvals() -> Vec<String> {
    vec!["acme.native".into()]
}
#[test]
fn exact_all_field_serialization_and_pinned_approval() {
    let registry = PlanRegistry::new();
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    let serialized = serde_json::to_value(&view).unwrap();
    assert_eq!(
        serialized,
        json!({"plan_id":view.plan_id,"adds":[{"id":"acme.native","kind":"plugin","source":"acme/catalog","version":"1.2.3","reason":"requested","native":true,"trust":"user","runtime":"process","needs_approval":true,"requires_internal_paths":false}],"errors":[]})
    );
    let executor = Executor::default();
    let response = registry
        .confirm(&view.plan_id, &approvals(), prerequisites, &executor)
        .unwrap();
    let operations = executor.0.lock().unwrap();
    let confirmed = operations[0].confirmed().unwrap();
    let artifact = confirmed.plan.adds()[0].artifact.as_ref().unwrap();
    assert!(confirmed.approvals[0].matches(artifact));
    assert_eq!(artifact.sha256, [0xab; 32]);
    let mut wrong = artifact.clone();
    wrong.source = SourceIdentity::parse("acme/other").unwrap().identity;
    assert!(!confirmed.approvals[0].matches(&wrong));
    wrong = artifact.clone();
    wrong.sha256 = [0xcd; 32];
    assert!(!confirmed.approvals[0].matches(&wrong));
    assert_eq!(confirmed.plan.adds()[0].package_limit_bytes, 1_048_576);
    assert_eq!(registry.job(&response.job_id).unwrap().status, "running");
}

#[test]
fn linux_user_namespace_request_is_disclosed_and_bound_to_the_plan() {
    let mut entry = plugin(None, None, 123);
    let baseline = plan(&entry, false);
    entry.linux_user_namespaces = Some(true);
    let opted_in = plan(&entry, false);
    assert_ne!(opted_in.fingerprint(), baseline.fingerprint());
    assert_eq!(
        serde_json::to_value(opted_in.view()).unwrap()["adds"][0]["linux_user_namespaces"],
        true
    );
    let artifact = opted_in.adds()[0].artifact.as_ref().unwrap();
    assert!(artifact.linux_user_namespaces);
    assert!(opted_in.view().adds[0].needs_approval);
}
#[test]
fn stale_each_prerequisite_rejects_without_dispatch() {
    for field in 0..10 {
        let registry = PlanRegistry::new();
        let view = registry
            .insert(plan(&plugin(None, None, 123), false))
            .unwrap();
        let executor = Executor::default();
        let result = registry.confirm(
            &view.plan_id,
            &approvals(),
            || {
                let mut p = prerequisites();
                match field {
                    0 => p.source_generation += 1,
                    1 => p.trust_generation += 1,
                    2 => p.private_generation += 1,
                    3 => p.credential_generation += 1,
                    4 => p.installed_generation += 1,
                    5 => p.limit_generation += 1,
                    6 => p.snapshot_fingerprint[0] ^= 1,
                    7 => p.application = "2.2.0".into(),
                    8 => p.target = "x86_64-pc-windows-msvc".into(),
                    _ => p.max_package_bytes += 1,
                }
                p
            },
            &executor,
        );
        assert_eq!(result, Err(PlanError::Stale), "field {field}");
        assert!(executor.0.lock().unwrap().is_empty());
    }
}
#[test]
fn concurrent_duplicate_confirm_starts_one_job() {
    let registry = PlanRegistry::new();
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    let executor = Arc::new(Executor::default());
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    registry.confirm(
                        &view.plan_id,
                        &approvals(),
                        prerequisites,
                        executor.as_ref(),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| **r == Err(PlanError::Consumed))
            .count(),
        1
    );
    assert_eq!(executor.0.lock().unwrap().len(), 1);
}
#[test]
fn capacity_and_expiry_are_lazy_and_bounded() {
    let registry = PlanRegistry::new();
    let start = Instant::now();
    let entry = plugin(None, None, 123);
    let mut first = String::new();
    for _ in 0..64 {
        first = registry
            .insert_at(plan(&entry, false), start)
            .unwrap()
            .plan_id;
    }
    assert_eq!(
        registry.insert_at(plan(&entry, false), start).unwrap_err(),
        PlanError::Busy
    );
    let executor = Executor::default();
    assert_eq!(
        registry.confirm_at(
            &first,
            &approvals(),
            prerequisites,
            &executor,
            start + PLAN_TTL
        ),
        Err(PlanError::NotFound)
    );
    assert!(
        registry
            .insert_at(plan(&entry, false), start + PLAN_TTL)
            .is_ok()
    );
    assert!(executor.0.lock().unwrap().is_empty());
}
#[test]
fn invalid_approvals_do_not_consume_plan() {
    let registry = PlanRegistry::new();
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    let executor = Executor::default();
    for ids in [
        vec![],
        vec!["wrong".into()],
        vec!["acme.native".into(), "acme.native".into()],
    ] {
        assert_eq!(
            registry.confirm(&view.plan_id, &ids, prerequisites, &executor),
            Err(PlanError::InvalidApprovals)
        );
    }
    assert!(
        registry
            .confirm(&view.plan_id, &approvals(), prerequisites, &executor)
            .is_ok()
    );
}
#[test]
fn d8_uses_state_authority_and_only_resolved_optional() {
    for (capability, bundled, expected) in [
        ("field:vosu.osu", true, false),
        ("field:vosu.osu.internal", true, true),
        ("field:vosu.osu.internal.paths.stable", true, true),
        ("field:vosu.osu.internality", true, false),
        ("field:vosu.osu.internal.paths", false, false),
    ] {
        let p = plan(&plugin(None, Some(capability), 123), bundled);
        assert!(p.view().errors.is_empty(), "{:?}", p.view().errors);
        assert_eq!(
            p.view().adds[0].requires_internal_paths,
            expected,
            "{capability}"
        );
    }
    assert!(
        plan(
            &plugin(Some("field:vosu.osu.internal.paths"), None, 123),
            true
        )
        .view()
        .adds[0]
            .requires_internal_paths
    );
}
#[test]
fn oversized_is_document_valid_but_an_error_plan() {
    let entry = plugin(None, None, 1_048_577);
    assert_eq!(entry.packages[TARGET].size, 1_048_577);
    let p = plan(&entry, false);
    assert!(p.adds().is_empty());
    assert!(p.view().adds.is_empty());
    assert!(p.view().errors[0].contains("local limit"));
    let registry = PlanRegistry::new();
    let view = registry.insert(p).unwrap();
    assert_eq!(
        registry.confirm(&view.plan_id, &[], prerequisites, &Executor::default()),
        Err(PlanError::ErrorPlan)
    );
}
#[test]
fn empty_resolution_is_an_error() {
    let builtins = BuiltinProviders::default();
    let application = Version::new(2, 1, 0);
    let snapshot = Snapshot {
        candidates: &[],
        installed: &[],
        builtins: &builtins,
        application: &application,
        target: TARGET,
        max_package_bytes: 1_048_576,
    };
    let p = build(
        &snapshot,
        &[],
        &CurrentPolicy {
            generation: SourceGeneration(11),
            sources: &[],
        },
        prerequisites(),
    );
    assert_eq!(p.view().errors, ["plan has no adds"]);
    assert!(p.adds().is_empty());
}
#[test]
fn operation_gate_and_job_envelope_release_on_finish_and_drop() {
    let registry = PlanRegistry::new();
    let executor = Executor::default();
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    let guard = registry.begin_operation().unwrap();
    assert_eq!(
        registry.confirm(&view.plan_id, &approvals(), prerequisites, &executor),
        Err(PlanError::Busy)
    );
    drop(guard);
    let job = registry
        .confirm(&view.plan_id, &approvals(), prerequisites, &executor)
        .unwrap();
    assert_eq!(registry.begin_operation().err(), Some(PlanError::Busy));
    let operation = executor.0.lock().unwrap().pop().unwrap();
    operation.progress("verifying", "acme.native");
    assert_eq!(registry.job(&job.job_id).unwrap().phase, "verifying");
    operation.finish(Ok(()));
    assert_eq!(
        serde_json::to_value(registry.job(&job.job_id).unwrap()).unwrap(),
        json!({"id":job.job_id,"status":"completed","phase":"finished","tool":"acme.native","error":null})
    );
    assert!(registry.begin_operation().is_ok());
}
#[test]
fn fingerprint_recheck_and_dropped_worker_fail_closed() {
    let registry = PlanRegistry::new();
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    {
        let mut state = registry.state.lock().unwrap();
        let record = state.plans.get_mut(&view.plan_id).unwrap();
        Arc::get_mut(&mut record.plan).unwrap().fingerprint[0] ^= 1;
    }
    let executor = Executor::default();
    assert_eq!(
        registry.confirm(&view.plan_id, &approvals(), prerequisites, &executor),
        Err(PlanError::Stale)
    );
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    let job = registry
        .confirm(&view.plan_id, &approvals(), prerequisites, &executor)
        .unwrap();
    drop(executor.0.lock().unwrap().pop().unwrap());
    assert_eq!(registry.job(&job.job_id).unwrap().status, "failed");
    assert!(registry.begin_operation().is_ok());
}

#[test]
fn rejected_executor_consumes_plan_and_releases_gate_with_failed_job() {
    struct Reject;
    impl CatalogExecutor for Reject {
        fn start(&self, _operation: CatalogOperation) -> Result<(), String> {
            Err("redacted dispatch failure".into())
        }
    }
    let registry = PlanRegistry::new();
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    assert_eq!(
        registry.confirm(&view.plan_id, &approvals(), prerequisites, &Reject),
        Err(PlanError::Executor)
    );
    assert_eq!(
        registry.confirm(&view.plan_id, &approvals(), prerequisites, &Reject),
        Err(PlanError::Consumed)
    );
    assert!(registry.begin_operation().is_ok());
    assert_eq!(
        registry
            .state
            .lock()
            .unwrap()
            .jobs
            .values()
            .next()
            .unwrap()
            .0
            .status,
        "failed"
    );
}

#[test]
fn native_trust_policy_never_downgrades_in_process_and_trusted_needs_no_approval() {
    use crate::catalog::{sources::SourceRecord, trust::TrustTier};
    use vosu_plugin_api::manifest::Runtime;
    let mut entry = plugin(None, None, 123);
    entry.runtime = Some(Runtime::InProcess);
    let denied = plan(&entry, false);
    assert!(denied.adds().is_empty());
    assert!(denied.view().errors[0].contains("in-process"));
    let source = SourceIdentity::parse("acme/catalog").unwrap().identity;
    let records = [SourceRecord {
        identity: source.clone(),
        enabled: true,
        trusted: true,
        private_network: false,
        order: 0,
    }];
    let builtins = BuiltinProviders::default();
    let application = Version::new(2, 1, 0);
    for runtime in [Runtime::Process, Runtime::InProcess] {
        entry.runtime = Some(runtime);
        let candidates = [Candidate {
            kind: PackageKind::Plugin,
            entry: &entry,
            source: &source,
            source_order: 0,
            catalog_order: 0,
            trusted: true,
        }];
        let snapshot = Snapshot {
            candidates: &candidates,
            installed: &[],
            builtins: &builtins,
            application: &application,
            target: TARGET,
            max_package_bytes: 1_048_576,
        };
        let p = build(
            &snapshot,
            &[Request {
                kind: PackageKind::Plugin,
                id: "acme.native",
                source: &source,
            }],
            &CurrentPolicy {
                generation: SourceGeneration(11),
                sources: &records,
            },
            prerequisites(),
        );
        assert!(p.view().errors.is_empty());
        assert_eq!(p.view().adds[0].trust, Some(TrustTier::Trusted));
        assert!(!p.view().adds[0].needs_approval);
        assert_eq!(p.adds()[0].artifact.as_ref().unwrap().runtime, runtime);
        let registry = PlanRegistry::new();
        let view = registry.insert(p).unwrap();
        assert!(
            registry
                .confirm(&view.plan_id, &[], prerequisites, &Executor::default())
                .is_ok()
        );
    }
}

#[test]
fn status_mapping_and_restart_unknown_plan_are_explicit() {
    for (error, status) in [
        (PlanError::NotFound, 404),
        (PlanError::Stale, 409),
        (PlanError::Consumed, 409),
        (PlanError::Busy, 409),
        (PlanError::InvalidApprovals, 400),
        (PlanError::ErrorPlan, 400),
    ] {
        assert_eq!(error.status_code(), status);
    }
    let registry = PlanRegistry::new();
    let view = registry
        .insert(plan(&plugin(None, None, 123), false))
        .unwrap();
    assert_eq!(
        PlanRegistry::new().confirm(
            &view.plan_id,
            &approvals(),
            prerequisites,
            &Executor::default()
        ),
        Err(PlanError::NotFound)
    );
}

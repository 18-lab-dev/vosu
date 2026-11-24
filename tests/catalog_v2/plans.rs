use semver::Version;
use serde_json::json;
use vosu_core::catalog::{
    plans::{self, CatalogExecutor, CatalogOperation, PlanError, PlanRegistry, Prerequisites},
    resolver::{BuiltinProviders, Candidate, Request, Snapshot},
    sources::{SourceGeneration, SourceIdentity},
    trust::CurrentPolicy,
};
use vosu_plugin_api::{catalog::Catalog, manifest::PackageKind};
fn prerequisites() -> Prerequisites {
    Prerequisites {
        source_generation: 31,
        trust_generation: 32,
        private_generation: 33,
        credential_generation: 34,
        installed_generation: 35,
        limit_generation: 36,
        snapshot_fingerprint: [37; 32],
        application: "2.1.0".into(),
        target: "x86_64-unknown-linux-gnu".into(),
        max_package_bytes: 536_870_912,
    }
}
fn plan(size: u64) -> plans::PinnedPlan {
    let catalog=Catalog::parse(&json!({"catalog_version":2,"name":"Plan fixture","published":"2026-11-24T00:00:00Z",
        "tools":[{"id":"review-tool","name":"Review","version":"2.3.4","host":"^2",
        "optional":[{"id":"field:acme.absent","version":"^1"}],
        "packages":{"any":{"url":"https://example.org/review.zip","sha256":"cd".repeat(32),"size":size}}}]}).to_string()).unwrap();
    assert_eq!(catalog.tools[0].packages["any"].size, size);
    let source = SourceIdentity::parse("acme/review").unwrap().identity;
    let candidates = [Candidate {
        kind: PackageKind::Tool,
        entry: &catalog.tools[0],
        source: &source,
        source_order: 0,
        catalog_order: 0,
        trusted: false,
    }];
    let builtin = BuiltinProviders::from_registry(
        &vosu_core::state::schema::SchemaRegistry::with_core().unwrap(),
        &[],
    );
    let app = Version::new(2, 1, 0);
    let snapshot = Snapshot {
        candidates: &candidates,
        installed: &[],
        builtins: &builtin,
        application: &app,
        target: "x86_64-unknown-linux-gnu",
        max_package_bytes: 536_870_912,
    };
    plans::build(
        &snapshot,
        &[Request {
            kind: PackageKind::Tool,
            id: "review-tool",
            source: &source,
        }],
        &CurrentPolicy {
            generation: SourceGeneration(31),
            sources: &[],
        },
        prerequisites(),
    )
}
struct Complete;
impl CatalogExecutor for Complete {
    fn start(&self, operation: CatalogOperation) -> Result<(), String> {
        let confirmed = operation.confirmed().unwrap();
        assert!(confirmed.approvals.is_empty());
        assert_eq!(
            confirmed.plan.adds()[0].entry.packages["any"].sha256,
            "cd".repeat(32)
        );
        assert_eq!(confirmed.plan.adds()[0].package_limit_bytes, 536_870_912);
        operation.finish(Ok(()));
        Ok(())
    }
}
#[test]
fn plans_public_review_confirm_and_catalog_job_are_not_action_jobs() {
    let registry = PlanRegistry::new();
    let view = registry.insert(plan(128)).unwrap();
    assert_eq!(
        serde_json::to_value(&view).unwrap(),
        json!({"plan_id":view.plan_id,"adds":[{"id":"review-tool","kind":"tool","source":"acme/review","version":"2.3.4","reason":"requested","native":false,"trust":null,"runtime":null,"needs_approval":false,"requires_internal_paths":false}],"errors":[]})
    );
    assert_eq!(
        registry.confirm(
            &view.plan_id,
            &["review-tool".into()],
            prerequisites,
            &Complete
        ),
        Err(PlanError::InvalidApprovals)
    );
    let response = registry
        .confirm(&view.plan_id, &[], prerequisites, &Complete)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"job_id":response.job_id})
    );
    assert_eq!(
        serde_json::to_value(registry.job(&response.job_id).unwrap()).unwrap(),
        json!({"id":response.job_id,"status":"completed","phase":"finished","tool":"","error":null})
    );
    assert_eq!(
        registry.confirm(&view.plan_id, &[], prerequisites, &Complete),
        Err(PlanError::Consumed)
    );
}
#[test]
fn plans_oversized_document_survives_sdk_but_cannot_confirm() {
    let plan = plan(536_870_913);
    assert!(plan.view().adds.is_empty());
    assert!(plan.adds().is_empty());
    assert_eq!(
        plan.view().errors,
        ["package exceeds local limit: review-tool"]
    );
    let registry = PlanRegistry::new();
    let view = registry.insert(plan).unwrap();
    assert_eq!(
        registry.confirm(&view.plan_id, &[], prerequisites, &Complete),
        Err(PlanError::ErrorPlan)
    );
}
#[test]
fn plans_stale_order_limit_installed_and_fingerprint_refuse_unseen_recompute() {
    for field in 0..4 {
        let registry = PlanRegistry::new();
        let view = registry.insert(plan(256)).unwrap();
        assert_eq!(
            registry.confirm(
                &view.plan_id,
                &[],
                || {
                    let mut p = prerequisites();
                    match field {
                        0 => p.source_generation += 1,
                        1 => p.max_package_bytes = 1_048_576,
                        2 => p.installed_generation += 1,
                        _ => p.snapshot_fingerprint = [38; 32],
                    };
                    p
                },
                &Complete
            ),
            Err(PlanError::Stale)
        );
        assert!(
            registry
                .confirm(&view.plan_id, &[], prerequisites, &Complete)
                .is_ok()
        );
    }
}

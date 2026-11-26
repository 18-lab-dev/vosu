//! Plan creation, single-use confirmation, catalog jobs and game-offer
//! acceptance. Plans resolve exactly once against one coherent input set;
//! confirmation never re-resolves.
//!
//! Prerequisites (A6b.6): S0 advances its one source generation on every
//! source, order, enabled, trust, private-network and credential change, so
//! that generation is pinned for each of those inputs. The installed
//! generation is the store's. The package limit is fully captured by its
//! checked byte value. `snapshot_fingerprint` is SHA-256 over the source
//! records, the installed document and the candidate-snapshot epoch (a new
//! publication at an unchanged generation is a new epoch). Confirmation
//! recomputes all of it from in-memory state only, under P0's gate.
use super::*;
use crate::catalog::{
    game::PlanInputs,
    install::{InstalledDocument, InstalledKind},
    installed::InstalledGraph,
    plans::{self, Prerequisites},
    resolver::{Candidate, Installed, Request, Snapshot},
    sources::{SourceGeneration, SourceIdentity, SourceRecord},
    trust::CurrentPolicy,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use vosu_plugin_api::manifest::PackageKind;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallBody {
    kind: InstalledKind,
    id: String,
    source: SourceIdentity,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmBody {
    #[serde(default)]
    approve: Vec<String>,
}

/// One coherent, owned input set for a single resolution.
struct Inputs {
    generation: SourceGeneration,
    records: Vec<SourceRecord>,
    snapshot: Arc<CandidateSnapshot>,
    graph: InstalledGraph,
    prerequisites: Prerequisites,
}

fn fingerprint(
    generation: SourceGeneration,
    records: &[SourceRecord],
    document: &InstalledDocument,
    epoch: u64,
) -> [u8; 32] {
    let bytes = serde_json::to_vec(&(generation.0, records, document, epoch))
        .expect("source and installed records are serializable");
    Sha256::digest(bytes).into()
}

fn prerequisites(
    inner: &Inner,
    generation: SourceGeneration,
    records: &[SourceRecord],
    document: &InstalledDocument,
    snapshot: &Arc<CandidateSnapshot>,
    limit: u64,
) -> Prerequisites {
    let facts = inner.runtime().host_facts();
    Prerequisites {
        source_generation: generation.0,
        trust_generation: generation.0,
        private_generation: generation.0,
        credential_generation: generation.0,
        installed_generation: document.generation,
        limit_generation: limit,
        snapshot_fingerprint: fingerprint(generation, records, document, inner.epoch(snapshot)),
        application: facts.application.to_string(),
        target: facts.target.clone(),
        max_package_bytes: limit,
    }
}

/// In-memory recomputation for P0's confirm-time comparison. Anything
/// unavailable yields prerequisites that can never equal a pinned plan.
fn current(inner: &Inner) -> Prerequisites {
    let unmatched = Prerequisites {
        source_generation: u64::MAX,
        trust_generation: u64::MAX,
        private_generation: u64::MAX,
        credential_generation: u64::MAX,
        installed_generation: u64::MAX,
        limit_generation: u64::MAX,
        snapshot_fingerprint: [0; 32],
        application: String::new(),
        target: String::new(),
        max_package_bytes: 0,
    };
    let runtime = inner.runtime();
    let (Some(fetch), Some(store), Some(limit)) = (
        runtime.fetch(),
        runtime.installed(),
        (inner.services.package_limit)(),
    ) else {
        return unmatched;
    };
    let (Ok((generation, records)), Ok(Some(snapshot))) = (
        fetch.sources().snapshot(),
        fetch.sources().candidate_snapshot(),
    ) else {
        return unmatched;
    };
    if snapshot.generation() != generation {
        return unmatched;
    }
    prerequisites(
        inner,
        generation,
        &records,
        &store.snapshot(),
        &snapshot,
        limit,
    )
}

/// Blocking: reads the committed packages' manifests for the kept graph.
fn gather(inner: &Inner) -> Result<Inputs, ApiError> {
    let runtime = inner.runtime();
    let fetch = runtime.fetch().ok_or_else(ApiError::disabled)?;
    let store = runtime.installed().ok_or_else(ApiError::disabled)?;
    let limit = (inner.services.package_limit)()
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid package limit"))?;
    let (generation, records) = fetch.sources().snapshot()?;
    let snapshot = fetch
        .sources()
        .candidate_snapshot()?
        .filter(|snapshot| snapshot.generation() == generation)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "catalog sources changed or were never fetched; refresh them first",
            )
        })?;
    let document = store.snapshot();
    let graph = InstalledGraph::read(store.layout(), &document)?;
    let prerequisites = prerequisites(inner, generation, &records, &document, &snapshot, limit);
    Ok(Inputs {
        generation,
        records,
        snapshot,
        graph,
        prerequisites,
    })
}

/// Borrow `inputs` into R0's immutable snapshot and T0's current policy.
fn resolve<T>(
    inner: &Inner,
    inputs: &Inputs,
    run: impl FnOnce(&Snapshot<'_>, &CurrentPolicy<'_>) -> T,
) -> T {
    let candidates: Vec<Candidate<'_>> = inputs
        .snapshot
        .candidates()
        .map(|candidate| Candidate {
            kind: candidate.kind,
            entry: candidate.entry,
            source: &candidate.source.identity,
            source_order: candidate.source.order,
            catalog_order: candidate.catalog_index,
            trusted: candidate.source.trusted,
        })
        .collect();
    let installed: Vec<Installed<'_>> = inputs
        .graph
        .nodes()
        .iter()
        .filter(|node| node.record.matches_fixture_source())
        .map(|node| Installed {
            manifest: &node.manifest,
        })
        .collect();
    let facts = inner.runtime().host_facts();
    run(
        &Snapshot {
            candidates: &candidates,
            installed: &installed,
            builtins: &inner.services.builtins,
            application: &facts.application,
            target: &facts.target,
            max_package_bytes: inputs.prerequisites.max_package_bytes,
        },
        &CurrentPolicy {
            generation: inputs.generation,
            sources: &inputs.records,
        },
    )
}

/// `POST /api/catalog/install {"kind","id","source"}`: resolve once and
/// register an immutable reviewable plan (errors are part of the review).
pub(super) async fn install(inner: &Arc<Inner>, body: &[u8]) -> ApiResult {
    let InstallBody { kind, id, source } = parse(body)?;
    let inner = Arc::clone(inner);
    let view = tokio::task::spawn_blocking(move || {
        let inputs = gather(&inner)?;
        let kind = PackageKind::from(kind);
        let plan = resolve(&inner, &inputs, |snapshot, policy| {
            plans::build(
                snapshot,
                &[Request {
                    kind,
                    id: &id,
                    source: &source,
                }],
                policy,
                inputs.prerequisites.clone(),
            )
        });
        inner.runtime().plans().insert(plan).map_err(ApiError::from)
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "catalog worker failed"))??;
    Ok(json(StatusCode::OK, &view))
}

/// `POST /api/catalog/plans/<plan_id>/confirm {"approve":[…]}`: one
/// single-use confirmation; the job runs on I0's executor.
pub(super) fn confirm(inner: &Arc<Inner>, plan: &str, body: &[u8]) -> ApiResult {
    let ConfirmBody { approve } = parse(body)?;
    let runtime = inner.runtime();
    let executor = runtime.executor().ok_or_else(ApiError::disabled)?;
    let response = runtime
        .plans()
        .confirm(plan, &approve, || current(inner), executor.as_ref())?;
    Ok(json(StatusCode::OK, &response))
}

/// Catalog polling envelope `{id,status,phase,tool,error}`, not an API-v1 job.
pub(super) fn job(inner: &Inner, job: &str) -> ApiResult {
    inner
        .runtime()
        .plans()
        .job(job)
        .map(|job| json(StatusCode::OK, &job))
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown or expired catalog job"))
}

/// G0 acceptance: a normal requested plan for the offer's exact identity.
pub(super) async fn accept_offer(inner: &Arc<Inner>, offer: &str) -> ApiResult {
    let inner = Arc::clone(inner);
    let offer = offer.to_owned();
    let view = tokio::task::spawn_blocking(move || {
        let inputs = gather(&inner)?;
        let runtime = inner.runtime();
        resolve(&inner, &inputs, |snapshot, policy| {
            runtime.offers().accept(
                &offer,
                PlanInputs {
                    snapshot,
                    policy,
                    prerequisites: inputs.prerequisites.clone(),
                    registry: runtime.plans(),
                },
            )
        })
        .map_err(|error| ApiError::new(status(error.status_code()), error.to_string()))
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "catalog worker failed"))??;
    Ok(json(StatusCode::OK, &view))
}

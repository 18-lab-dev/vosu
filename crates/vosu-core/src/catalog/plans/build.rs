use crate::{
    catalog::{
        approvals::Artifact,
        resolver::{self, Node, Request, Snapshot},
        sources::SourceIdentity,
        trust::{CurrentPolicy, TrustTier},
    },
    state::InternalGrant,
};
use ring::digest::{SHA256, digest};
use serde::Serialize;
use std::sync::Arc;
use vosu_plugin_api::{
    catalog::CatalogEntry,
    manifest::{PackageKind, Runtime},
};

/// Every authority/input revision is pinned, including changes that restore an
/// older value. `snapshot_fingerprint` covers source entries and installed graph
/// metadata, not secrets. Composition computes it from its immutable snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prerequisites {
    pub source_generation: u64,
    pub trust_generation: u64,
    pub private_generation: u64,
    pub credential_generation: u64,
    pub installed_generation: u64,
    pub limit_generation: u64,
    pub snapshot_fingerprint: [u8; 32],
    pub application: String,
    pub target: String,
    pub max_package_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Reason {
    Requested,
    Dependency,
}

/// Exact public review envelope. Null trust/runtime on web-only adds are retained.
#[derive(Debug, Clone, Serialize)]
pub struct PlanAdd {
    pub id: String,
    #[serde(serialize_with = "serialize_kind")]
    pub kind: PackageKind,
    pub source: SourceIdentity,
    pub version: String,
    pub reason: Reason,
    pub native: bool,
    pub trust: Option<TrustTier>,
    pub runtime: Option<&'static str>,
    pub needs_approval: bool,
    pub requires_internal_paths: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fixture: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub linux_user_namespaces: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct PlanView {
    pub plan_id: String,
    pub adds: Vec<PlanAdd>,
    pub errors: Vec<String>,
}
/// Dependency-first immutable executor input. The full validated entry is pinned,
/// not looked up again by ID. `package_limit_bytes` bounds compressed AND expanded
/// bytes. Downloads use `package_target` and remain public-only/credential-free.
#[derive(Debug)]
pub struct PinnedAdd {
    pub entry: Arc<CatalogEntry>,
    pub kind: PackageKind,
    pub source: SourceIdentity,
    pub fixture: bool,
    pub package_target: String,
    pub package_limit_bytes: u64,
    pub artifact: Option<Artifact>,
    pub reason: Reason,
}
#[derive(Debug)]
pub struct PinnedPlan {
    pub(super) view: PlanView,
    pub(super) prerequisites: Prerequisites,
    pub(super) adds: Vec<PinnedAdd>,
    pub(super) fingerprint: [u8; 32],
}
impl PinnedPlan {
    pub fn view(&self) -> &PlanView {
        &self.view
    }
    pub fn prerequisites(&self) -> &Prerequisites {
        &self.prerequisites
    }
    pub fn adds(&self) -> &[PinnedAdd] {
        &self.adds
    }
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }
    pub(super) fn calculate_fingerprint(&self) -> [u8; 32] {
        // All public fields and prerequisites, including the complete input
        // snapshot fingerprint; selected hashes/sizes/URLs are pinned separately.
        let packages: Vec<_> = self
            .adds
            .iter()
            .map(|a| {
                let p = &a.entry.packages[&a.package_target];
                (&a.package_target, &p.url, &p.sha256, p.size)
            })
            .collect();
        let bytes = serde_json::to_vec(&(&self.view, &self.prerequisites, packages))
            .expect("plan fields are serializable");
        digest(&SHA256, &bytes)
            .as_ref()
            .try_into()
            .expect("SHA-256 length")
    }
}

/// Resolves once; confirmation never invokes the solver. Invalid/empty results
/// remain reviewable error plans with no executable adds. A plan is bounded by
/// resolver limits and a 4 MiB serialized review envelope.
pub fn build(
    snapshot: &Snapshot<'_>,
    requests: &[Request<'_>],
    policy: &CurrentPolicy<'_>,
    prerequisites: Prerequisites,
) -> PinnedPlan {
    let resolution = resolver::resolve(snapshot, requests);
    let mut errors: Vec<_> = resolution.errors.iter().map(ToString::to_string).collect();
    if prerequisites.application != snapshot.application.to_string()
        || prerequisites.target != snapshot.target
        || prerequisites.max_package_bytes != snapshot.max_package_bytes
        || prerequisites.source_generation != policy.generation.0
    {
        errors.push("plan prerequisites do not match resolver snapshot".into());
    }
    let mut adds = Vec::new();
    let mut views = Vec::new();
    for i in resolution.adds {
        let candidate = &snapshot.candidates[i];
        let entry = candidate.entry;
        let native = candidate.kind == PackageKind::Plugin;
        let target = if native { snapshot.target } else { "any" };
        let artifact = if native {
            let package = &entry.packages[target];
            let hash = decode_hash(&package.sha256);
            match (hash, entry.runtime) {
                (Some(sha256), Some(runtime)) => Some(Artifact {
                    id: entry.id.clone(),
                    source: candidate.source.clone(),
                    version: entry.version.clone(),
                    sha256,
                    runtime,
                    linux_user_namespaces: entry.linux_user_namespaces.unwrap_or_default(),
                }),
                _ => {
                    errors.push("invalid native artifact metadata".into());
                    continue;
                }
            }
        } else {
            None
        };
        let requirement = match artifact.as_ref().map(|a| policy.requirement(a)).transpose() {
            Ok(value) => value,
            Err(error) => {
                errors.push(format!("{}: {error}", entry.id));
                continue;
            }
        };
        let reason = if requests
            .iter()
            .any(|r| r.kind == candidate.kind && r.id == entry.id && r.source == candidate.source)
        {
            Reason::Requested
        } else {
            Reason::Dependency
        };
        let requires_internal_paths = native
            && resolution
                .edges
                .iter()
                .filter(|edge| edge.consumer == Node::Catalog(i))
                .any(|edge| InternalGrant::names_internal_path(&edge.requirement.id));
        let fixture = candidate.source.is_official_fixture();
        views.push(PlanAdd {
            id: entry.id.clone(),
            kind: candidate.kind,
            source: candidate.source.clone(),
            version: entry.version.clone(),
            reason,
            native,
            trust: requirement.map(|r| r.trust),
            runtime: artifact.as_ref().map(|a| match a.runtime {
                Runtime::Process => "process",
                Runtime::InProcess => "in-process",
            }),
            needs_approval: requirement.is_some_and(|r| r.needs_approval),
            requires_internal_paths,
            fixture,
            linux_user_namespaces: artifact.as_ref().is_some_and(|a| a.linux_user_namespaces),
        });
        adds.push(PinnedAdd {
            entry: Arc::new(entry.clone()),
            kind: candidate.kind,
            source: candidate.source.clone(),
            fixture,
            package_target: target.into(),
            package_limit_bytes: snapshot.max_package_bytes,
            artifact,
            reason,
        });
    }
    if adds.is_empty() && errors.is_empty() {
        errors.push("plan has no adds".into());
    }
    if serde_json::to_vec(&views).is_ok_and(|v| v.len() > 4 * 1024 * 1024) {
        errors.push("plan review exceeds size limit".into());
    }
    if !errors.is_empty() {
        adds.clear();
        views.clear();
    }
    let mut plan = PinnedPlan {
        view: PlanView {
            plan_id: String::new(),
            adds: views,
            errors,
        },
        prerequisites,
        adds,
        fingerprint: [0; 32],
    };
    plan.fingerprint = plan.calculate_fingerprint();
    plan
}
fn decode_hash(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut hash = [0; 32];
    for (byte, pair) in hash.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *byte = (pair[0] as char).to_digit(16)? as u8 * 16 + (pair[1] as char).to_digit(16)? as u8;
    }
    Some(hash)
}

fn serialize_kind<S: serde::Serializer>(
    kind: &PackageKind,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(match kind {
        PackageKind::Plugin => "plugin",
        PackageKind::Tool => "tool",
        PackageKind::Overlay => "overlay",
    })
}

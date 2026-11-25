use super::{CatalogOperation, PinnedPlan, PlanView};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use std::sync::Mutex;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

pub const MAX_PENDING_PLANS: usize = 64;
pub const PLAN_TTL: Duration = Duration::from_secs(600);
/// Catalog polling compatibility: deliberately not actions::JobSnapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogJob {
    pub id: String,
    pub status: String,
    pub phase: String,
    pub tool: String,
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfirmResponse {
    pub job_id: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("unknown or expired plan")]
    NotFound,
    #[error("stale plan")]
    Stale,
    #[error("plan already consumed")]
    Consumed,
    #[error("catalog operation or registry busy")]
    Busy,
    #[error("invalid native approvals")]
    InvalidApprovals,
    #[error("plan contains errors or no adds")]
    ErrorPlan,
    #[error("secure randomness unavailable")]
    Randomness,
    #[error("catalog executor unavailable")]
    Executor,
}
impl PlanError {
    pub fn status_code(self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::Stale | Self::Consumed | Self::Busy => 409,
            Self::InvalidApprovals | Self::ErrorPlan => 400,
            Self::Randomness | Self::Executor => 500,
        }
    }
}
pub(super) struct Record {
    pub plan: Arc<PinnedPlan>,
    pub created: Instant,
    pub consumed: bool,
}
#[derive(Default)]
pub(super) struct State {
    pub plans: HashMap<String, Record>,
    pub jobs: HashMap<String, (CatalogJob, Instant)>,
    pub running: Option<String>,
}
impl State {
    pub fn expire(&mut self, now: Instant) {
        self.plans
            .retain(|_, r| now.saturating_duration_since(r.created) < PLAN_TTL);
        let running = self.running.as_deref();
        self.jobs.retain(|id, (_, created)| {
            running == Some(id.as_str()) || now.saturating_duration_since(*created) < PLAN_TTL
        });
    }
}
#[derive(Clone, Default)]
pub struct PlanRegistry {
    pub(super) state: Arc<Mutex<State>>,
}
impl PlanRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&self, plan: PinnedPlan) -> Result<PlanView, PlanError> {
        self.insert_at(plan, Instant::now())
    }
    pub(super) fn insert_at(
        &self,
        mut plan: PinnedPlan,
        now: Instant,
    ) -> Result<PlanView, PlanError> {
        let mut state = self.state.lock().expect("catalog registry poisoned");
        state.expire(now);
        // Consumed tombstones also count toward the bound, guaranteeing bounded
        // replay memory even if a caller rapidly creates/confirms tiny plans.
        if state.plans.len() >= MAX_PENDING_PLANS {
            return Err(PlanError::Busy);
        }
        let id = unique_id(&state.plans)?;
        plan.view.plan_id.clone_from(&id);
        plan.fingerprint = plan.calculate_fingerprint();
        let view = plan.view.clone();
        state.plans.insert(
            id,
            Record {
                plan: Arc::new(plan),
                created: now,
                consumed: false,
            },
        );
        Ok(view)
    }
    pub fn get(&self, id: &str) -> Result<PlanView, PlanError> {
        let mut state = self.state.lock().expect("catalog registry poisoned");
        state.expire(Instant::now());
        state
            .plans
            .get(id)
            .map(|r| r.plan.view.clone())
            .ok_or(PlanError::NotFound)
    }
    pub fn job(&self, id: &str) -> Option<CatalogJob> {
        let mut state = self.state.lock().expect("catalog registry poisoned");
        state.expire(Instant::now());
        state.jobs.get(id).map(|(job, _)| job.clone())
    }
    /// Shared catalog mutation gate. Retain through durable publication; do not
    /// hold the registry mutex during that work. No polling job for a mutation.
    pub fn begin_operation(&self) -> Result<CatalogOperation, PlanError> {
        let mut state = self.state.lock().expect("catalog registry poisoned");
        state.expire(Instant::now());
        if state.running.is_some() {
            return Err(PlanError::Busy);
        }
        let id = unique_id(&state.jobs)?;
        state.running = Some(id.clone());
        Ok(CatalogOperation::new(self.state.clone(), id, None))
    }
}
pub(super) fn unique_id<T>(existing: &HashMap<String, T>) -> Result<String, PlanError> {
    loop {
        let mut bytes = [0; 24];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| PlanError::Randomness)?;
        let mut id = String::with_capacity(48);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes {
            id.push(HEX[usize::from(byte >> 4)] as char);
            id.push(HEX[usize::from(byte & 15)] as char);
        }
        if !existing.contains_key(&id) {
            return Ok(id);
        }
    }
}

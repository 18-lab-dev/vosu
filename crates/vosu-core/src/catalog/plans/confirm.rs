use super::{
    ConfirmResponse, PinnedPlan, PlanError, PlanRegistry, Prerequisites,
    registry::{CatalogJob, State, unique_id},
};
use crate::catalog::approvals::Approval;
use std::sync::Mutex;
use std::{sync::Arc, time::Instant};

/// I0's immutable install input. Approval records are T0's exact artifact type;
/// they are persisted only with successful transaction publication, never here.
#[derive(Debug)]
pub struct ConfirmedPlan {
    pub plan: Arc<PinnedPlan>,
    pub approvals: Vec<Approval>,
    pub job_id: String,
}
/// Implemented by I0's bounded worker dispatcher. On error the operation must be
/// dropped; never retain it and also report failure. Executor owns worker handles,
/// cancellation signalling and joining on shutdown. Cancellation is a failed job
/// only after rollback; never release the operation while publication is running.
pub trait CatalogExecutor {
    fn start(&self, operation: CatalogOperation) -> Result<(), String>;
}
/// Exclusive operation ownership, movable to a worker. Completion/drop releases
/// only its own token. Dropping unfinished work reports failure, not success.
pub struct CatalogOperation {
    state: Arc<Mutex<State>>,
    id: String,
    confirmed: Option<ConfirmedPlan>,
    finished: bool,
}
impl CatalogOperation {
    pub(super) fn new(
        state: Arc<Mutex<State>>,
        id: String,
        confirmed: Option<ConfirmedPlan>,
    ) -> Self {
        Self {
            state,
            id,
            confirmed,
            finished: false,
        }
    }
    pub fn confirmed(&self) -> Option<&ConfirmedPlan> {
        self.confirmed.as_ref()
    }
    pub fn progress(&self, phase: &str, tool: &str) {
        let mut state = self.state.lock().expect("catalog registry poisoned");
        if let Some((job, _)) = state.jobs.get_mut(&self.id) {
            job.phase = phase.into();
            job.tool = tool.into();
        }
    }
    /// Error must be a redacted local diagnostic, never upstream URLs/bodies or
    /// credentials. `Ok` means the full graph was committed and loaded/handed off.
    pub fn finish(mut self, result: Result<(), String>) {
        self.complete(result);
    }
    fn complete(&mut self, result: Result<(), String>) {
        let mut state = self.state.lock().expect("catalog registry poisoned");
        if state.running.as_deref() == Some(&self.id) {
            state.running = None;
        }
        if let Some((job, completed)) = state.jobs.get_mut(&self.id) {
            job.status = if result.is_ok() {
                "completed"
            } else {
                "failed"
            }
            .into();
            job.phase = "finished".into();
            job.error = result.err();
            *completed = Instant::now();
        }
        self.finished = true;
    }
}
impl Drop for CatalogOperation {
    fn drop(&mut self) {
        if !self.finished {
            self.complete(Err("catalog operation cancelled".into()));
        }
    }
}
impl PlanRegistry {
    /// HTTP approval IDs are resolved against this exact immutable native plan,
    /// never against fresh catalog entries. Missing/extraneous/duplicate IDs fail.
    /// `current` must read coherent in-memory prerequisites only. It executes
    /// under the operation gate before single-use consumption; never do I/O here.
    pub fn confirm(
        &self,
        id: &str,
        approve: &[String],
        current: impl FnOnce() -> Prerequisites,
        executor: &impl CatalogExecutor,
    ) -> Result<ConfirmResponse, PlanError> {
        self.confirm_at(id, approve, current, executor, Instant::now())
    }
    pub(super) fn confirm_at(
        &self,
        id: &str,
        approve: &[String],
        current: impl FnOnce() -> Prerequisites,
        executor: &impl CatalogExecutor,
        now: Instant,
    ) -> Result<ConfirmResponse, PlanError> {
        let mut state = self.state.lock().expect("catalog registry poisoned");
        state.expire(now);
        let record = state.plans.get(id).ok_or(PlanError::NotFound)?;
        if record.consumed {
            return Err(PlanError::Consumed);
        }
        if state.running.is_some() {
            return Err(PlanError::Busy);
        }
        let plan = Arc::clone(&record.plan);
        if plan.view.adds.is_empty() || !plan.view.errors.is_empty() {
            return Err(PlanError::ErrorPlan);
        }
        if plan.prerequisites != current() || plan.fingerprint != plan.calculate_fingerprint() {
            return Err(PlanError::Stale);
        }
        let mut approvals = Vec::new();
        for (index, id) in approve.iter().enumerate() {
            if approve[..index].contains(id) {
                return Err(PlanError::InvalidApprovals);
            }
            let add_index = plan
                .view
                .adds
                .iter()
                .position(|a| a.id == *id && a.native && a.needs_approval)
                .ok_or(PlanError::InvalidApprovals)?;
            let artifact = plan.adds[add_index]
                .artifact
                .as_ref()
                .ok_or(PlanError::InvalidApprovals)?;
            approvals.push(Approval::grant(artifact));
        }
        if plan
            .view
            .adds
            .iter()
            .any(|a| a.needs_approval && !approve.contains(&a.id))
        {
            return Err(PlanError::InvalidApprovals);
        }
        let job_id = unique_id(&state.jobs)?;
        // Bound completed polling records as well as plans; never evict a job
        // silently while it is still in its advertised retention lifetime.
        if state.jobs.len() >= super::MAX_PENDING_PLANS {
            return Err(PlanError::Busy);
        }
        state
            .plans
            .get_mut(id)
            .expect("record retained under lock")
            .consumed = true;
        state.running = Some(job_id.clone());
        state.jobs.insert(
            job_id.clone(),
            (
                CatalogJob {
                    id: job_id.clone(),
                    status: "running".into(),
                    phase: "preparing".into(),
                    tool: String::new(),
                    error: None,
                },
                now,
            ),
        );
        let operation = CatalogOperation::new(
            self.state.clone(),
            job_id.clone(),
            Some(ConfirmedPlan {
                plan,
                approvals,
                job_id: job_id.clone(),
            }),
        );
        drop(state);
        executor.start(operation).map_err(|_| PlanError::Executor)?;
        Ok(ConfirmResponse { job_id })
    }
}

use super::*;
use crate::actions::deadline::DeadlinePolicy;
use crate::actions::waiters::{JobWait, MAX_WAITERS, MAX_WAITERS_PER_JOB};

fn create(registry: &JobRegistry) -> Arc<JobSnapshot> {
    registry
        .create(
            Principal::Broker,
            "fixture.work".into(),
            Some("session-17".into()),
        )
        .unwrap()
}

async fn bounded(wait: JobWait) -> Arc<JobSnapshot> {
    tokio::time::timeout(Duration::from_secs(2), wait.wait())
        .await
        .expect("bounded job wait")
}

#[test]
fn running_capacity_never_evicts_and_terminal_pressure_evicts_oldest() {
    let registry = JobRegistry::new();
    let now = Instant::now();
    let jobs: Vec<_> = (0..MAX_JOBS).map(|_| create(&registry)).collect();
    assert_eq!(create_result(&registry), Err(JobError::TooManyJobs));
    assert!(registry.get(&jobs[0].id, &Principal::Broker).is_ok());
    registry
        .finish_at(&jobs[11].id, TerminalOutcome::Cancelled, now)
        .unwrap();
    registry
        .finish_at(
            &jobs[3].id,
            TerminalOutcome::Cancelled,
            now + Duration::from_secs(1),
        )
        .unwrap();
    let replacement = registry
        .create_at(
            Principal::Broker,
            "fixture.work".into(),
            None,
            now + Duration::from_secs(2),
        )
        .unwrap();
    assert_eq!(
        registry.get(&jobs[11].id, &Principal::Broker),
        Err(JobError::UnknownJob)
    );
    assert!(registry.get(&jobs[3].id, &Principal::Broker).is_ok());
    assert_eq!(replacement.state, JobState::Running);
    assert_eq!(registry.inner.lock().entries.len(), MAX_JOBS);
}

fn create_result(registry: &JobRegistry) -> Result<Arc<JobSnapshot>, JobError> {
    registry.create(Principal::Broker, "fixture.work".into(), None)
}

#[test]
fn retention_expires_at_exact_boundary_without_expiring_running_jobs() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    let running = create(&registry);
    let now = Instant::now();
    registry
        .finish_at(&job.id, TerminalOutcome::Succeeded(Value::from(37)), now)
        .unwrap();
    assert!(
        registry
            .get_at(
                &job.id,
                &Principal::Broker,
                now + TERMINAL_RETENTION - Duration::from_nanos(1)
            )
            .is_ok()
    );
    assert_eq!(
        registry.get_at(&job.id, &Principal::Broker, now + TERMINAL_RETENTION),
        Err(JobError::UnknownJob)
    );
    assert!(
        registry
            .get_at(&running.id, &Principal::Broker, now + TERMINAL_RETENTION)
            .is_ok()
    );
}

#[test]
fn ownership_is_exact_launch_identity_and_broker_is_authoritative() {
    let registry = JobRegistry::new();
    let owner = JobOwner::new();
    let principal = Principal::Tool(owner.clone());
    let stranger = Principal::Tool(JobOwner::new());
    let job = registry
        .create(principal.clone(), "fixture.work".into(), None)
        .unwrap();
    assert!(registry.get(&job.id, &Principal::Tool(owner)).is_ok());
    assert_eq!(registry.get(&job.id, &stranger), Err(JobError::UnknownJob));
    assert_eq!(
        registry.request_cancel(&job.id, &stranger).unwrap_err(),
        JobError::UnknownJob
    );
    assert!(!registry.inner.lock().entries[&job.id].cancel_requested);
    assert!(
        registry
            .request_cancel(&job.id, &Principal::Broker)
            .unwrap()
            .dispatch
    );
    assert!(registry.get(&job.id, &principal).is_ok());
    let broker_job = create(&registry);
    assert_eq!(
        registry.get(&broker_job.id, &principal),
        Err(JobError::UnknownJob)
    );
}

#[test]
fn cancel_is_advisory_idempotent_and_session_never_retargets() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    let other = registry
        .create(Principal::Broker, "fixture.work".into(), None)
        .unwrap();
    assert_eq!(registry.cancel_session("session-17"), vec![job.id.clone()]);
    assert!(registry.cancel_session("session-17").is_empty());
    let cancel = registry
        .request_cancel(&job.id, &Principal::Broker)
        .unwrap();
    assert!(!cancel.dispatch);
    assert_eq!(cancel.job.state, JobState::Running);
    assert_eq!(cancel.job.revision, 0);
    assert_eq!(cancel.job.session.as_deref(), Some("session-17"));
    assert!(
        registry
            .finish(&job.id, TerminalOutcome::Succeeded(Value::from(91)))
            .unwrap()
    );
    let current = registry
        .request_cancel(&job.id, &Principal::Broker)
        .unwrap();
    assert_eq!(current.job.state, JobState::Succeeded);
    assert_eq!(current.job.result, Some(Value::from(91)));
    assert!(!current.dispatch);
    assert_eq!(
        registry.get(&other.id, &Principal::Broker).unwrap().state,
        JobState::Running
    );
}

#[test]
fn snapshots_are_immutable_and_first_terminal_result_wins() {
    let registry = JobRegistry::new();
    let initial = create(&registry);
    assert!(
        registry
            .progress(
                &initial.id,
                JobProgress {
                    fraction: Some(0.42),
                    message: Some("working".into())
                }
            )
            .unwrap()
    );
    let progress = registry.get(&initial.id, &Principal::Broker).unwrap();
    assert_eq!(progress.revision, 1);
    assert_eq!(progress.progress.as_ref().unwrap().fraction, Some(0.42));
    assert!(
        registry
            .finish(
                &initial.id,
                TerminalOutcome::Failed(JobFailure {
                    code: "plugin_crashed".into(),
                    message: "lost".into()
                })
            )
            .unwrap()
    );
    assert!(
        !registry
            .finish(&initial.id, TerminalOutcome::Succeeded(Value::from(52)))
            .unwrap()
    );
    assert!(
        !registry
            .progress(
                &initial.id,
                JobProgress {
                    fraction: Some(0.9),
                    message: None
                }
            )
            .unwrap()
    );
    let terminal = registry.get(&initial.id, &Principal::Broker).unwrap();
    assert_eq!(terminal.revision, 2);
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.error.as_ref().unwrap().code, "plugin_crashed");
    assert_eq!(terminal.result, None);
    assert_eq!(initial.state, JobState::Running);
    assert_eq!(initial.revision, 0);
    assert_eq!(initial.progress, None);
    assert_eq!(progress.state, JobState::Running);
}

#[test]
fn cancelled_failure_has_no_public_error() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    registry
        .finish(
            &job.id,
            TerminalOutcome::Failed(JobFailure {
                code: "cancelled".into(),
                message: "stopped".into(),
            }),
        )
        .unwrap();
    let terminal = registry.get(&job.id, &Principal::Broker).unwrap();
    assert_eq!(terminal.state, JobState::Cancelled);
    assert_eq!(terminal.error, None);
    assert_eq!(terminal.result, None);
}

#[tokio::test]
async fn waiter_reservation_drops_both_counters_and_overflow_is_current() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    let mut guards = Vec::new();
    for _ in 0..MAX_WAITERS_PER_JOB {
        let wait = registry
            .poll(&job.id, &Principal::Broker, 0, Duration::from_secs(1))
            .unwrap();
        assert!(matches!(wait, JobWait::Waiting(_)));
        guards.push(wait);
    }
    assert!(matches!(
        registry
            .poll(&job.id, &Principal::Broker, 0, Duration::from_secs(1))
            .unwrap(),
        JobWait::Immediate(_)
    ));
    guards.pop();
    assert_eq!(registry.inner.lock().waiters, MAX_WAITERS_PER_JOB - 1);
    assert_eq!(
        registry.inner.lock().entries[&job.id].waiters,
        MAX_WAITERS_PER_JOB - 1
    );
    drop(guards);
    assert_eq!(registry.inner.lock().waiters, 0);
    assert_eq!(registry.inner.lock().entries[&job.id].waiters, 0);
}

#[tokio::test]
async fn global_waiter_capacity_and_drop_release() {
    let registry = JobRegistry::new();
    let mut guards = Vec::new();
    for _ in 0..MAX_WAITERS / MAX_WAITERS_PER_JOB {
        let job = create(&registry);
        for _ in 0..MAX_WAITERS_PER_JOB {
            guards.push(
                registry
                    .poll(&job.id, &Principal::Broker, 0, Duration::from_secs(1))
                    .unwrap(),
            );
        }
    }
    assert_eq!(registry.inner.lock().waiters, MAX_WAITERS);
    let extra = create(&registry);
    assert!(matches!(
        registry
            .poll(&extra.id, &Principal::Broker, 0, Duration::from_secs(1))
            .unwrap(),
        JobWait::Immediate(_)
    ));
    guards.pop();
    assert!(matches!(
        registry
            .poll(&extra.id, &Principal::Broker, 0, Duration::from_secs(1))
            .unwrap(),
        JobWait::Waiting(_)
    ));
    drop(guards);
    assert_eq!(registry.inner.lock().waiters, 0);
}

#[tokio::test]
async fn progress_between_reservation_and_wait_is_not_lost() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    let wait = registry
        .poll(&job.id, &Principal::Broker, 0, Duration::from_secs(1))
        .unwrap();
    registry
        .progress(
            &job.id,
            JobProgress {
                fraction: Some(0.75),
                message: None,
            },
        )
        .unwrap();
    let result = bounded(wait).await;
    assert_eq!(result.revision, 1);
    assert_eq!(result.state, JobState::Running);
    assert_eq!(registry.inner.lock().waiters, 0);
    let terminal_wait = registry
        .poll(
            &job.id,
            &Principal::Broker,
            u64::MAX,
            Duration::from_secs(1),
        )
        .unwrap();
    registry
        .finish(&job.id, TerminalOutcome::Cancelled)
        .unwrap();
    assert_eq!(bounded(terminal_wait).await.state, JobState::Cancelled);
}

#[tokio::test]
async fn elapsed_wait_returns_unchanged_snapshot_and_releases_slot() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    let wait = registry
        .poll(&job.id, &Principal::Broker, 0, Duration::from_millis(1))
        .unwrap();
    assert!(matches!(wait, JobWait::Waiting(_)));
    assert_eq!(bounded(wait).await, job);
    assert_eq!(registry.inner.lock().waiters, 0);
    let immediate = registry
        .poll(&job.id, &Principal::Broker, 0, Duration::ZERO)
        .unwrap();
    assert!(matches!(immediate, JobWait::Immediate(_)));
}

#[tokio::test]
async fn evicted_waiter_releases_global_count_without_touching_new_job() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    let wait = registry
        .poll(&job.id, &Principal::Broker, 0, Duration::from_secs(1))
        .unwrap();
    registry
        .finish(&job.id, TerminalOutcome::Cancelled)
        .unwrap();
    let future = Instant::now() + TERMINAL_RETENTION;
    registry
        .create_at(Principal::Broker, "fixture.work".into(), None, future)
        .unwrap();
    assert!(!registry.inner.lock().entries.contains_key(&job.id));
    assert_eq!(bounded(wait).await.state, JobState::Cancelled);
    assert_eq!(registry.inner.lock().waiters, 0);
}

#[tokio::test]
async fn deadline_is_terminal_and_late_success_cannot_resurrect() {
    let registry = JobRegistry::new();
    assert_eq!(DeadlinePolicy::default().timeout, Duration::from_secs(30));
    let job = create(&registry);
    let _guard = DeadlinePolicy {
        timeout: Duration::ZERO,
    }
    .arm(&registry, &job.id)
    .unwrap();
    let wait = registry
        .poll(&job.id, &Principal::Broker, 0, Duration::from_secs(1))
        .unwrap();
    let terminal = bounded(wait).await;
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.error.as_ref().unwrap().code, "timed_out");
    assert!(
        !registry
            .finish(&job.id, TerminalOutcome::Succeeded(Value::from(71)))
            .unwrap()
    );
    assert_eq!(registry.get(&job.id, &Principal::Broker).unwrap(), terminal);
}

#[tokio::test]
async fn deadlines_do_not_keep_registry_alive() {
    let registry = JobRegistry::new();
    let weak = Arc::downgrade(&registry.inner);
    let job = create(&registry);
    let _guard = DeadlinePolicy::default().arm(&registry, &job.id).unwrap();
    drop(registry);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn unauthorized_poll_does_not_reserve_a_waiter() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    assert!(matches!(
        registry.poll(
            &job.id,
            &Principal::Tool(JobOwner::new()),
            0,
            Duration::from_secs(1)
        ),
        Err(JobError::UnknownJob)
    ));
    assert_eq!(registry.inner.lock().waiters, 0);
}

#[test]
fn creation_snapshot_stays_running_after_synchronous_success() {
    let registry = JobRegistry::new();
    let initial = create(&registry);
    registry
        .finish(&initial.id, TerminalOutcome::Succeeded(Value::from(83)))
        .unwrap();
    assert_eq!(initial.state, JobState::Running);
    assert_eq!(initial.revision, 0);
    let current = registry.get(&initial.id, &Principal::Broker).unwrap();
    assert_eq!(current.state, JobState::Succeeded);
    assert_eq!(current.revision, 1);
    assert_eq!(current.result, Some(Value::from(83)));
    assert_eq!(current.error, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn competing_terminal_results_accept_exactly_one() {
    let registry = JobRegistry::new();
    let job = create(&registry);
    let first_registry = registry.clone();
    let first_id = job.id.clone();
    let first = tokio::spawn(async move {
        first_registry
            .finish(&first_id, TerminalOutcome::Succeeded(Value::from(62)))
            .unwrap()
    });
    let second_registry = registry.clone();
    let second_id = job.id.clone();
    let second = tokio::spawn(async move {
        second_registry
            .finish(&second_id, TerminalOutcome::Cancelled)
            .unwrap()
    });
    let (first, second) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(first, second)
    })
    .await
    .expect("bounded terminal race");
    assert_ne!(first.unwrap(), second.unwrap());
    let current = registry.get(&job.id, &Principal::Broker).unwrap();
    assert_eq!(current.revision, 1);
    assert!(current.state.is_terminal());
    assert_eq!(
        current.result,
        if current.state == JobState::Succeeded {
            Some(Value::from(62))
        } else {
            None
        }
    );
}

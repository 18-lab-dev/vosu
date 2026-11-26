//! J3: real HTTP -> executor -> ABI lifecycle. OS process crashes are M5 evidence;
//! M3 drives the production host's instance-loss lifecycle seam instead.
use super::{
    job_fixture as fixture,
    support::{Harness, bounded},
};
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use vosu_core::actions::{
    ActionDescriptor, InvocationPolicy, JobOwner, JobRegistry, Principal, ToolAuthority,
};
use vosu_core::server::api_v1::{
    actions::{ActionAuthority, ActionRoutes},
    router::ApiV1Router,
    subscriptions::ApiV1SubscriptionHub,
};

pub struct Authority(pub JobOwner);
impl ToolAuthority for Authority {
    fn authorizes(&self, owner: &JobOwner, _: &ActionDescriptor) -> bool {
        owner.same(&self.0)
    }
}
impl ActionAuthority for Authority {
    fn authenticate_action_session(&self, token: &str) -> Option<Principal> {
        (token == "j3-launch").then(|| Principal::Tool(self.0.clone()))
    }
    fn authorize_action_session(&self, token: &str, _: &str) -> Option<Principal> {
        self.authenticate_action_session(token)
    }
}
pub fn request(method: Method, path: &str, args: Value) -> Request {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:25050")
        .header("x-vosu-tool-session", "j3-launch")
        .body(Body::from(json!({"arguments":args}).to_string()))
        .unwrap()
}
pub async fn value(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &bounded(to_bytes(response.into_body(), 2 * 1024 * 1024))
            .await
            .unwrap(),
    )
    .unwrap()
}
fn router(
    h: &Harness,
    jobs: JobRegistry,
    deadline: Duration,
) -> (ApiV1Router, ApiV1SubscriptionHub) {
    let source = h.host.api_state_source();
    let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
    (
        ApiV1Router::new(source, hub.clone()).with_actions(ActionRoutes::new(
            h.host.clone(),
            h.host.invocation_executor_with_policy(InvocationPolicy {
                deadline,
                ..InvocationPolicy::default()
            }),
            jobs,
            Arc::new(Authority(JobOwner::for_tool("j3".into()))),
        )),
        hub,
    )
}

#[test]
fn timeout_survives_real_late_abi_success_and_instance_loss_fails_running_job() {
    let h = Harness::new();
    let (events, _) = fixture::load(&h, 783301, "fixture.jthree.loss", "");
    h.runtime.block_on(bounded(async {
        let (api, hub) = router(&h, JobRegistry::new(), Duration::from_millis(200));
        let response = api
            .handle(request(
                Method::POST,
                "/api/v1/actions/fixture.jthree.loss.nested.echo?wait=0",
                json!("timeout"),
            ))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let path = response.headers()["location"].to_str().unwrap().to_owned();
        assert_eq!(value(response).await["state"], "running");
        let progress = value(
            api.handle(request(
                Method::GET,
                &format!("{path}?after=0&wait=1000"),
                Value::Null,
            ))
            .await,
        )
        .await;
        assert_eq!(progress["progress"]["fraction"], 0.375);
        assert_eq!(progress["revision"], 1);
        let terminal = value(
            api.handle(request(
                Method::GET,
                &format!("{path}?after=1&wait=1000"),
                Value::Null,
            ))
            .await,
        )
        .await;
        assert_eq!(terminal["state"], "failed");
        assert_eq!(terminal["error"]["code"], "timed_out");
        fixture::timer(&h, 783301, 1);
        // Acknowledgement is emitted only after the late host.send returned.
        bounded(async {
            loop {
                if let Ok(vosu_plugin_api::Message::TimerFired(t)) = events.try_recv() {
                    assert_ne!(t.timer, 0);
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(
            value(api.handle(request(Method::GET, &path, Value::Null)).await).await,
            terminal
        );
        let response = api
            .handle(request(
                Method::POST,
                "/api/v1/actions/fixture.jthree.loss.nested.echo?wait=0",
                json!("loss"),
            ))
            .await;
        let path = response.headers()["location"].to_str().unwrap().to_owned();
        let _ = value(response).await;
        let progress = value(
            api.handle(request(
                Method::GET,
                &format!("{path}?after=0&wait=1000"),
                Value::Null,
            ))
            .await,
        )
        .await;
        assert_eq!(progress["revision"], 1);
        assert_eq!(progress["state"], "running");
        h.host
            .action_instance_lost(vosu_core::identity::PluginInstanceId::new(783301));
        let terminal = value(api.handle(request(Method::GET, &path, Value::Null)).await).await;
        assert_eq!(terminal["state"], "failed");
        assert_eq!(terminal["error"]["code"], "plugin_crashed");
        hub.shutdown();
    }));
}

#[test]
fn public_job_waiter_limits_retention_eviction_and_running_capacity() {
    use futures_util::StreamExt;
    use vosu_core::actions::{
        Principal, TerminalOutcome,
        jobs::MAX_JOBS,
        waiters::{MAX_WAITERS, MAX_WAITERS_PER_JOB},
    };
    let h = Harness::new();
    h.runtime.block_on(bounded(async {
        let jobs = JobRegistry::new();
        let owner = JobOwner::for_tool("j3".into());
        let source = h.host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
        let api = ApiV1Router::new(source, hub.clone()).with_actions(ActionRoutes::new(
            h.host.clone(),
            h.host.invocation_executor(),
            jobs.clone(),
            Arc::new(Authority(owner.clone())),
        ));
        let principal = Principal::Tool(owner);
        let first = jobs
            .create(principal.clone(), "waiter.fixture".into(), None)
            .unwrap();
        let mut waits = futures_util::stream::FuturesUnordered::new();
        let uri = format!("/api/v1/jobs/{}?after=0&wait=30000", first.id);
        for _ in 0..MAX_WAITERS_PER_JOB {
            waits.push(api.handle(request(Method::GET, &uri, Value::Null)));
        }
        assert!(futures_util::poll!(waits.next()).is_pending());
        let overflow = api.handle(request(Method::GET, &uri, Value::Null));
        tokio::pin!(overflow);
        let std::task::Poll::Ready(response) = futures_util::poll!(overflow) else {
            panic!("per-job overflow must return immediately");
        };
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(value(response).await["revision"], 0);
        drop(waits);
        let mut waits = futures_util::stream::FuturesUnordered::new();
        let mut uris = vec![uri];
        for _ in 1..MAX_WAITERS / MAX_WAITERS_PER_JOB {
            let job = jobs
                .create(principal.clone(), "waiter.fixture".into(), None)
                .unwrap();
            uris.push(format!("/api/v1/jobs/{}?after=0&wait=30000", job.id));
        }
        for uri in &uris {
            for _ in 0..MAX_WAITERS_PER_JOB {
                waits.push(api.handle(request(Method::GET, uri, Value::Null)));
            }
        }
        assert!(futures_util::poll!(waits.next()).is_pending());
        let extra = jobs
            .create(principal.clone(), "waiter.extra".into(), None)
            .unwrap();
        // FuturesUnordered may yield cooperatively before polling all 256.
        // Reserve the global limit synchronously through the same registry.
        drop(waits);
        let mut guards = Vec::new();
        for uri in &uris {
            let id = uri
                .strip_prefix("/api/v1/jobs/")
                .unwrap()
                .split('?')
                .next()
                .unwrap();
            for _ in 0..MAX_WAITERS_PER_JOB {
                let guard = jobs
                    .poll(id, &principal, 0, Duration::from_secs(30))
                    .unwrap();
                assert!(matches!(
                    guard,
                    vosu_core::actions::waiters::JobWait::Waiting(_)
                ));
                guards.push(guard);
            }
        }
        let overflow_uri = format!("/api/v1/jobs/{}?after=0&wait=30000", extra.id);
        let overflow = api.handle(request(Method::GET, &overflow_uri, Value::Null));
        tokio::pin!(overflow);
        let std::task::Poll::Ready(response) = futures_util::poll!(overflow) else {
            panic!("global overflow must return immediately");
        };
        assert_eq!(response.status(), StatusCode::OK);
        drop(guards);
        let reservation = jobs
            .poll(&extra.id, &principal, 0, Duration::from_secs(30))
            .unwrap();
        assert!(
            matches!(
                reservation,
                vosu_core::actions::waiters::JobWait::Waiting(_)
            ),
            "dropping waiters must recover capacity"
        );
        drop(reservation);
        let recovered = api
            .handle(request(
                Method::GET,
                &format!("/api/v1/jobs/{}?after=0&wait=1", extra.id),
                Value::Null,
            ))
            .await;
        assert_eq!(recovered.status(), StatusCode::OK);
        // Terminal entries are retained until pressure; running entries never evict.
        jobs.finish(&first.id, TerminalOutcome::Succeeded(json!(37)))
            .unwrap();
        assert_eq!(
            value(
                api.handle(request(
                    Method::GET,
                    &format!("/api/v1/jobs/{}", first.id),
                    Value::Null
                ))
                .await
            )
            .await["result"],
            37
        );
        for _ in uris.len() + 1..MAX_JOBS {
            jobs.create(principal.clone(), "capacity.fixture".into(), None)
                .unwrap();
        }
        jobs.create(principal.clone(), "evicts.terminal".into(), None)
            .unwrap();
        assert_eq!(
            api.handle(request(
                Method::GET,
                &format!("/api/v1/jobs/{}", first.id),
                Value::Null
            ))
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            api.handle(request(
                Method::GET,
                &format!("/api/v1/jobs/{}", extra.id),
                Value::Null
            ))
            .await
            .status(),
            StatusCode::OK
        );
        assert!(jobs.create(principal, "full".into(), None).is_err());
        hub.shutdown();
    }));
}

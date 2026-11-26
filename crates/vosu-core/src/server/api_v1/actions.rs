//! HTTP action creation is prepare -> job -> bind -> dispatch, in that order.
use super::{
    authorization::Credentials,
    error::{error, source_error},
    jobs::{job_error, query, snapshot},
    router::{decode_segment, target},
};
use crate::{
    actions::{
        ActionRegistry, InvocationError, InvocationExecutor, JobFailure, JobRegistry, JobSink,
        Principal, TerminalOutcome,
    },
    api::ApiStateSource,
    state::StateValue,
};
use axum::{
    body::to_bytes,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use vosu_plugin_api::ActionSessionMode;

/// The tool host implements this boundary; the router never owns tool internals.
pub trait ActionAuthority: crate::actions::ToolAuthority {
    fn authenticate_action_session(&self, token: &str) -> Option<Principal>;
    fn authorize_action_session(&self, token: &str, action: &str) -> Option<Principal>;
}

#[derive(Clone)]
pub struct ActionRoutes {
    pub(super) registry: Arc<dyn ActionRegistry>,
    pub(super) executor: InvocationExecutor,
    pub(super) jobs: JobRegistry,
    pub(super) tools: Arc<dyn ActionAuthority>,
}
impl ActionRoutes {
    pub fn new(
        registry: Arc<dyn ActionRegistry>,
        executor: InvocationExecutor,
        jobs: JobRegistry,
        tools: Arc<dyn ActionAuthority>,
    ) -> Self {
        let executor = executor.with_tool_authority(tools.clone());
        Self {
            registry,
            executor,
            jobs,
            tools,
        }
    }

    pub(super) async fn create(
        &self,
        source: &ApiStateSource,
        mut request: Request,
        session: Option<&str>,
        action: &str,
    ) -> Response {
        let credentials = match Credentials::authenticate(&request, self.tools.as_ref()) {
            Ok(value) => value,
            Err(e) => return e.into_response(),
        };
        let (_, wait) = match query(request.uri().query(), false) {
            Ok(value) => value,
            Err(e) => return e.into_response(),
        };
        let action = match decode_segment(action) {
            Ok(value) => value,
            Err(()) => return unknown_action(),
        };
        let Some(action) = self.registry.get(&action) else {
            return unknown_action();
        };
        if matches!(
            (action.session, session.is_some()),
            (ActionSessionMode::Required, false) | (ActionSessionMode::None, true)
        ) {
            return unknown_action();
        }
        if let Err(e) = credentials.authorize(self.tools.as_ref(), &action.id) {
            return e.into_response();
        }
        if matches!(credentials.principal, Principal::Broker) && !action.cli {
            return invocation_error(InvocationError::PermissionDenied);
        }
        let session = match session {
            Some(id) => {
                let target = match target(id) {
                    Ok(value) => value,
                    Err(()) => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "Invalid session",
                        );
                    }
                };
                match source.session(&target) {
                    Ok(summary) => match summary.get(&["session"]) {
                        Some(StateValue::String(id)) => Some(id.to_string()),
                        _ => {
                            return error(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "serialization_failed",
                                "Session unavailable",
                            );
                        }
                    },
                    Err(e) => return source_error(e),
                }
            }
            None => None,
        };
        let body = match bounded_body(&mut request).await {
            Ok(body) => body,
            Err(e) => return e,
        };
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            arguments: serde_json::Value,
        }
        let input: Input = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                return error(
                    StatusCode::BAD_REQUEST,
                    "invalid_arguments",
                    "Expected action arguments",
                );
            }
        };
        // Body receipt is an await boundary: check credential revocation again.
        let credentials = match Credentials::authenticate(&request, self.tools.as_ref()) {
            Ok(value) => value,
            Err(e) => return e.into_response(),
        };
        if let Err(e) = credentials.authorize(self.tools.as_ref(), &action.id) {
            return e.into_response();
        }
        let prepared = match self.executor.prepare(
            credentials.principal.clone(),
            action.clone(),
            session.clone(),
            input.arguments,
        ) {
            Ok(value) => value,
            Err(e) => return invocation_error(e),
        };
        let initial =
            match self
                .jobs
                .create(credentials.principal.clone(), action.id.clone(), session)
            {
                Ok(value) => value,
                Err(e) => return job_error(e),
            };
        if let Err(e) = self
            .jobs
            .bind_invocation(&initial.id, &self.executor, prepared.id())
        {
            return job_error(e);
        }
        if let Err(e) = prepared.invoke(Arc::new(JobSink::new(
            self.jobs.clone(),
            initial.id.clone(),
        ))) {
            // Dispatch failure must not strand a retained running job.
            let _ = self.jobs.finish(
                &initial.id,
                TerminalOutcome::Failed(JobFailure {
                    code: e.to_string(),
                    message: "Action dispatch failed".into(),
                }),
            );
            return invocation_error(e);
        }
        if wait == Duration::ZERO {
            return snapshot(StatusCode::ACCEPTED, &initial);
        }
        let waiting = async {
            let mut job = initial.clone();
            while !job.state.is_terminal() {
                let revision = job.revision;
                job = self
                    .jobs
                    .wait(&credentials.principal, &job.id, revision, wait)
                    .await?;
                if job.revision == revision {
                    break;
                }
            }
            Ok(job)
        };
        let job = match tokio::time::timeout(wait, waiting).await {
            Ok(Ok(value)) => value,
            Ok(Err(e)) => return job_error(e),
            Err(_) => match self.jobs.get(&credentials.principal, &initial.id) {
                Ok(value) => value,
                Err(e) => return job_error(e),
            },
        };
        let credentials = match Credentials::authenticate(&request, self.tools.as_ref()) {
            Ok(value) => value,
            Err(e) => return e.into_response(),
        };
        if let Err(e) = credentials.authorize(self.tools.as_ref(), &job.action) {
            return e.into_response();
        }
        snapshot(
            if job.state.is_terminal() {
                StatusCode::OK
            } else {
                StatusCode::ACCEPTED
            },
            &job,
        )
    }
}

pub(super) async fn bounded_body(request: &mut Request) -> Result<axum::body::Bytes, Response> {
    to_bytes(std::mem::take(request.body_mut()), 1024 * 1024)
        .await
        .map_err(|_| {
            error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "args_too_large",
                "Request body too large",
            )
        })
}
fn unknown_action() -> Response {
    error(StatusCode::NOT_FOUND, "unknown_action", "Unknown action")
}
fn invocation_error(value: InvocationError) -> Response {
    let (status, code, message) = match value {
        InvocationError::UnknownAction => return unknown_action(),
        InvocationError::PermissionDenied => {
            (StatusCode::FORBIDDEN, "forbidden", "Action forbidden")
        }
        InvocationError::InvalidSession => {
            (StatusCode::NOT_FOUND, "unknown_session", "Unknown session")
        }
        InvocationError::InvalidArguments => (
            StatusCode::BAD_REQUEST,
            "invalid_arguments",
            "Invalid action arguments",
        ),
        InvocationError::ArgumentsTooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "args_too_large",
            "Encoded arguments too large",
        ),
        InvocationError::Capacity => (
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_jobs",
            "Action capacity reached",
        ),
        InvocationError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "plugin_crashed",
            "Action unavailable",
        ),
    };
    error(status, code, message)
}

use super::{
    actions::ActionRoutes,
    authorization::Credentials,
    error::{RequestFailure, error, json},
    router::decode_form_component,
};
use axum::{
    extract::Request,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::{sync::Arc, time::Duration};
use vosu_core::actions::{JobError, JobSnapshot};

pub(super) fn query(
    raw: Option<&str>,
    polling: bool,
) -> Result<(Option<u64>, Duration), RequestFailure> {
    let mut after = None;
    let mut wait = None;
    if let Some(raw) = raw {
        if raw.len() > 128 {
            return Err(invalid_query());
        }
        for pair in raw.split('&') {
            let (key, value) = pair.split_once('=').ok_or_else(invalid_query)?;
            let key = decode_form_component(key).map_err(|_| invalid_query())?;
            let value = decode_form_component(value).map_err(|_| invalid_query())?;
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid_query());
            }
            match key.as_str() {
                "after" if polling && after.is_none() => {
                    after = Some(value.parse().map_err(|_| invalid_query())?)
                }
                "wait" if wait.is_none() => {
                    let ms: u64 = value.parse().map_err(|_| invalid_query())?;
                    if ms > 30_000 {
                        return Err(invalid_query());
                    }
                    wait = Some(Duration::from_millis(ms));
                }
                _ => return Err(invalid_query()),
            }
        }
    }
    if polling && (after.is_some() != wait.is_some()) {
        return Err(invalid_query());
    }
    Ok((
        after,
        wait.unwrap_or(Duration::from_millis(if polling { 0 } else { 250 })),
    ))
}
fn invalid_query() -> RequestFailure {
    RequestFailure(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Invalid job query",
    )
}

impl ActionRoutes {
    pub(super) async fn job(&self, mut request: Request, id: &str) -> Response {
        if let Err(e) = Credentials::authenticate(&request, self.tools.as_ref()) {
            return e.into_response();
        }
        let (after, wait) = if request.method() == Method::GET {
            match query(request.uri().query(), true) {
                Ok(value) => value,
                Err(e) => return e.into_response(),
            }
        } else {
            if request.uri().query().is_some() {
                return invalid_query().into_response();
            }
            (None, Duration::ZERO)
        };
        if let Err(e) = super::actions::bounded_body(&mut request).await {
            return e;
        }
        let credentials = match Credentials::authenticate(&request, self.tools.as_ref()) {
            Ok(value) => value,
            Err(e) => return e.into_response(),
        };
        let job = match self.jobs.get(&credentials.principal, id) {
            Ok(value) => value,
            Err(e) => return job_error(e),
        };
        if let Err(e) = credentials.authorize(self.tools.as_ref(), &job.action) {
            return e.into_response();
        }
        let job = if request.method() == Method::DELETE {
            self.jobs.cancel(&credentials.principal, id)
        } else if let Some(after) = after {
            self.jobs
                .wait(&credentials.principal, id, after, wait)
                .await
        } else {
            Ok(job)
        };
        // A long poll must not return data after token or broker revocation.
        let credentials = match Credentials::authenticate(&request, self.tools.as_ref()) {
            Ok(value) => value,
            Err(e) => return e.into_response(),
        };
        match job {
            Ok(job) => {
                if let Err(e) = credentials.authorize(self.tools.as_ref(), &job.action) {
                    return e.into_response();
                }
                snapshot(StatusCode::OK, &job)
            }
            Err(e) => job_error(e),
        }
    }
}

pub(super) fn snapshot(status: StatusCode, job: &Arc<JobSnapshot>) -> Response {
    let mut response = json(
        status,
        serde_json::to_vec(job.as_ref()).expect("finite job values"),
    );
    if status == StatusCode::ACCEPTED {
        response.headers_mut().insert(
            header::LOCATION,
            format!("/api/v1/jobs/{}", job.id)
                .parse()
                .expect("core job id"),
        );
    }
    response
}
pub(super) fn job_error(value: JobError) -> Response {
    match value {
        JobError::UnknownJob => error(StatusCode::NOT_FOUND, "unknown_job", "Unknown job"),
        JobError::TooManyJobs => error(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_jobs",
            "Job capacity reached",
        ),
        JobError::IdExhausted => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "serialization_failed",
            "Job unavailable",
        ),
    }
}

use axum::{
    body::Body,
    http::{StatusCode, header},
    response::Response,
};
use vosu_core::api::ApiError;

pub fn error(status: StatusCode, code: &str, message: &str) -> Response {
    json(
        status,
        serde_json::json!({"error": {"code": code, "message": message}})
            .to_string()
            .into_bytes(),
    )
}

pub(super) fn source_error(value: ApiError) -> Response {
    match value {
        ApiError::UnknownSession => {
            error(StatusCode::NOT_FOUND, "unknown_session", "Unknown session")
        }
        ApiError::InvalidSelection(_) => error(
            StatusCode::BAD_REQUEST,
            "invalid_pointer",
            "Invalid JSON Pointer",
        ),
        ApiError::Closed | ApiError::Capacity => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "serialization_failed",
            "State unavailable",
        ),
    }
}

pub(super) fn json(status: StatusCode, body: Vec<u8>) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("valid API response")
}

#[derive(Debug)]
pub(super) struct RequestFailure(pub StatusCode, pub &'static str, pub &'static str);
impl axum::response::IntoResponse for RequestFailure {
    fn into_response(self) -> Response {
        error(self.0, self.1, self.2)
    }
}
#[cfg(test)]
impl RequestFailure {
    pub fn status(&self) -> StatusCode {
        self.0
    }
}

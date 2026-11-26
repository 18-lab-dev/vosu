use super::error::error;
use crate::{plugin::ResourceRequestError, resources::RegistryError};
use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};

pub(super) fn secure(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

pub(super) fn missing(tree: bool, gone: bool) -> Response {
    let (status, code) = match (tree, gone) {
        (false, false) => (StatusCode::NOT_FOUND, "unknown_resource"),
        (true, false) => (StatusCode::NOT_FOUND, "unknown_tree"),
        (false, true) => (StatusCode::GONE, "unpublished_resource"),
        (true, true) => (StatusCode::GONE, "unpublished_tree"),
    };
    secure(error(status, code, "Resource is not available"))
}

pub(super) fn registry_error(error: RegistryError, tree: bool) -> Response {
    missing(tree, error == RegistryError::Gone)
}

pub(super) fn provider_error(failure: ResourceRequestError, tree: bool) -> Response {
    match failure {
        ResourceRequestError::Gone => missing(tree, true),
        ResourceRequestError::NotFound => missing(false, false),
        ResourceRequestError::TimedOut => secure(error(
            StatusCode::GATEWAY_TIMEOUT,
            "provider_timed_out",
            "Resource provider timed out",
        )),
        _ => secure(error(
            StatusCode::BAD_GATEWAY,
            "provider_failed",
            "Resource provider failed",
        )),
    }
}

/// Separate header fields are multiple ranges too, even without a comma.
pub(super) fn range(headers: &HeaderMap, size: u64) -> Result<crate::server::files::ByteRange, ()> {
    let mut values = headers.get_all(header::RANGE).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(());
    }
    let text = first.map(|v| v.to_str().map_err(|_| ())).transpose()?;
    crate::server::files::parse_range(text, size).map_err(|_| ())
}

pub(super) fn invalid_range(size: u64) -> Response {
    let mut response = secure(error(
        StatusCode::RANGE_NOT_SATISFIABLE,
        "invalid_range",
        "Invalid byte range",
    ));
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{size}")).expect("numeric range"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_statuses_are_typed_and_never_encode_failures() {
        vosu_proptest::bounded!(async {
            for (failure, status, code) in [
                (
                    ResourceRequestError::Gone,
                    StatusCode::GONE,
                    "unpublished_tree",
                ),
                (
                    ResourceRequestError::NotFound,
                    StatusCode::NOT_FOUND,
                    "unknown_resource",
                ),
                (
                    ResourceRequestError::TimedOut,
                    StatusCode::GATEWAY_TIMEOUT,
                    "provider_timed_out",
                ),
                (
                    ResourceRequestError::InvalidReply,
                    StatusCode::BAD_GATEWAY,
                    "provider_failed",
                ),
                (
                    ResourceRequestError::ProviderFailed,
                    StatusCode::BAD_GATEWAY,
                    "provider_failed",
                ),
            ] {
                let response = provider_error(failure, true);
                assert_eq!(response.status(), status);
                assert_eq!(
                    response.headers()[header::CONTENT_SECURITY_POLICY],
                    "sandbox"
                );
                assert_eq!(
                    response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                    "nosniff"
                );
                let body = axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["error"]["code"], code);
            }
        });
    }

    #[test]
    fn range_headers_reject_multiple_fields_and_bound_overflow() {
        let mut headers = HeaderMap::new();
        headers.append(header::RANGE, HeaderValue::from_static("bytes=0-1"));
        headers.append(header::RANGE, HeaderValue::from_static("bytes=3-4"));
        assert!(range(&headers, 7).is_err());
        headers.clear();
        headers.insert(
            header::RANGE,
            HeaderValue::from_static("bytes=-18446744073709551615"),
        );
        assert_eq!(
            range(&headers, 7),
            Ok(crate::server::files::ByteRange::Partial {
                start: 0,
                end_inclusive: 6
            })
        );
        assert!(range(&headers, 0).is_err());
        headers.insert(
            header::RANGE,
            HeaderValue::from_static("bytes=-18446744073709551616"),
        );
        assert!(range(&headers, 7).is_err());
    }
}

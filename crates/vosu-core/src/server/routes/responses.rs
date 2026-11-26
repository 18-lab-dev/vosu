use super::*;

pub(super) fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("Not Found"))
        .unwrap()
}

/// An unknown route: the styled 404 page for a person, the plain body for
/// scripts and API clients (see [`super::super::pages::wants_page`]).
pub(super) fn not_found_for(request: &Request<Body>) -> Response {
    if !super::super::pages::wants_page(request.method(), request.headers()) {
        return not_found();
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(Bytes::from_static(
            super::super::pages::PAGE_NOT_FOUND,
        )))
        .unwrap()
}
pub(super) fn error_json(message: &str) -> Bytes {
    Bytes::from(serde_json::to_vec(&serde_json::json!({"error": message})).unwrap())
}
pub(super) fn json(status: StatusCode, body: Bytes) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}
pub(super) fn cors(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Origin, X-Requested-With, Content-Type, Accept"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, GET, PUT, DELETE, OPTIONS"),
    );
    headers.insert(
        HeaderName::from_static("access-control-allow-private-network"),
        HeaderValue::from_static("true"),
    );
    response
}

pub(super) fn api_v1_cors(response: Response) -> Response {
    let methods = response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_METHODS)
        .cloned();
    let mut response = cors(response);
    if let Some(methods) = methods {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_METHODS, methods);
    }
    response
}

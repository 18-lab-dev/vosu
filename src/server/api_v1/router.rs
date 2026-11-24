use super::{
    error::{error, source_error},
    http,
    subscriptions::ApiV1SubscriptionHub,
};
use crate::server::websocket::UpgradeCallbackTracker;
use axum::{
    body::Body,
    extract::{FromRequestParts, Request, ws::WebSocketUpgrade},
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use vosu_core::api::{ApiStateSource, SessionTarget};

#[derive(Clone)]
pub struct ApiV1Router {
    source: ApiStateSource,
    hub: ApiV1SubscriptionHub,
    actions: Option<super::actions::ActionRoutes>,
}

#[derive(Debug, PartialEq, Eq)]
enum Route<'a> {
    Sessions,
    Session(&'a str),
    State(&'a str, Option<&'a str>),
    Subscribe(Option<&'a str>),
    Action(Option<&'a str>, &'a str),
    Job(&'a str),
}

fn classify(path: &str) -> Option<Route<'_>> {
    let path = path.strip_prefix("/api/v1/")?;
    let mut action_segments = path.split('/');
    match (
        action_segments.next(),
        action_segments.next(),
        action_segments.next(),
        action_segments.next(),
        action_segments.next(),
    ) {
        (Some("actions"), Some(action), None, None, None) if !action.is_empty() => {
            return Some(Route::Action(None, action));
        }
        (Some("jobs"), Some(id), None, None, None) if !id.is_empty() => {
            return Some(Route::Job(id));
        }
        (Some("sessions"), Some(id), Some("actions"), Some(action), None)
            if !id.is_empty() && !action.is_empty() =>
        {
            return Some(Route::Action(Some(id), action));
        }
        _ => {}
    }
    let mut segments = path.split('/');
    if segments.next()? != "sessions" {
        return None;
    }
    let Some(id) = segments.next() else {
        return Some(Route::Sessions);
    };
    if id.is_empty() {
        return None;
    }
    if id == "subscribe" {
        return segments.next().is_none().then_some(Route::Subscribe(None));
    }
    match (segments.next(), segments.next(), segments.next()) {
        (None, None, None) => Some(Route::Session(id)),
        (Some("state"), None, None) => Some(Route::State(id, None)),
        (Some("state"), Some(ns), None) if !ns.is_empty() => Some(Route::State(id, Some(ns))),
        (Some("subscribe"), None, None) => Some(Route::Subscribe(Some(id))),
        _ => None,
    }
}

impl ApiV1Router {
    pub fn new(source: ApiStateSource, hub: ApiV1SubscriptionHub) -> Self {
        Self {
            source,
            hub,
            actions: None,
        }
    }

    pub fn with_actions(mut self, actions: super::actions::ActionRoutes) -> Self {
        self.actions = Some(actions);
        self
    }

    pub async fn handle(&self, request: Request) -> Response {
        let uri = request.uri().clone();
        let Some(route) = classify(uri.path()) else {
            return error(
                StatusCode::NOT_FOUND,
                "invalid_request",
                "Unknown API route",
            );
        };
        let methods = match route {
            Route::Action(..) => "POST",
            Route::Job(_) => "GET, DELETE",
            _ => "GET",
        };
        if request.method() == Method::OPTIONS {
            return Response::builder()
                .status(StatusCode::NO_CONTENT)
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, methods)
                .header(header::CACHE_CONTROL, "no-store")
                .body(Body::empty())
                .expect("valid preflight");
        }
        let method_allowed = match route {
            Route::Action(..) => request.method() == Method::POST,
            Route::Job(_) => matches!(*request.method(), Method::GET | Method::DELETE),
            _ => request.method() == Method::GET,
        };
        if !method_allowed {
            let mut response = error(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Method not allowed",
            );
            response
                .headers_mut()
                .insert(header::ALLOW, axum::http::HeaderValue::from_static(methods));
            return response;
        }
        // Only these two exact route shapes may upgrade. A request to a read
        // endpoint with upgrade headers must not silently turn into a read.
        if request.headers().contains_key(header::UPGRADE) && !matches!(route, Route::Subscribe(_))
        {
            return error(
                StatusCode::NOT_FOUND,
                "invalid_request",
                "Unknown WebSocket route",
            );
        }
        if matches!(route, Route::Sessions | Route::Session(_))
            && request.uri().query().is_some_and(|query| !query.is_empty())
        {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Unexpected session query",
            );
        }
        match route {
            Route::Action(session, action) => match &self.actions {
                Some(actions) => actions.create(&self.source, request, session, action).await,
                None => error(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "Action credential required",
                ),
            },
            Route::Job(id) => match &self.actions {
                Some(actions) => match decode_segment(id) {
                    Ok(id) => actions.job(request, &id).await,
                    Err(()) => error(StatusCode::NOT_FOUND, "unknown_job", "Unknown job"),
                },
                None => error(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "Action credential required",
                ),
            },
            Route::Sessions => http::sessions(&self.source),
            Route::Session(id) => match target(id) {
                Ok(target) => http::summary(&self.source, &target),
                Err(()) => invalid_target(),
            },
            Route::State(id, namespace) => {
                let target = match target(id) {
                    Ok(target) => target,
                    Err(()) => return invalid_target(),
                };
                let namespace = match namespace.map(decode_segment).transpose() {
                    Ok(value) => value,
                    Err(()) => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "Invalid namespace",
                        );
                    }
                };
                http::state(
                    &self.source,
                    &target,
                    namespace.as_deref(),
                    request.uri().query(),
                )
            }
            Route::Subscribe(id) => {
                if request.uri().query().is_some() {
                    return error(
                        StatusCode::BAD_REQUEST,
                        "invalid_request",
                        "Unexpected subscription query",
                    );
                }
                let connection = match id {
                    Some(id) => {
                        let target = match target(id) {
                            Ok(target) => target,
                            Err(()) => return invalid_target(),
                        };
                        match self.hub.connect(target) {
                            Ok(connection) => connection,
                            Err(error) => return source_error(error),
                        }
                    }
                    None => self.hub.connect_sessions(),
                };
                let Some(tracker) = request
                    .extensions()
                    .get::<Arc<UpgradeCallbackTracker>>()
                    .cloned()
                else {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "serialization_failed",
                        "Upgrade transport unavailable",
                    );
                };
                let (mut parts, _) = request.into_parts();
                let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
                    Ok(upgrade) => upgrade,
                    Err(_) => {
                        return error(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            "Invalid WebSocket upgrade",
                        );
                    }
                };
                let Some(registration) = tracker.register() else {
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "serialization_failed",
                        "Listener shutting down",
                    );
                };
                upgrade
                    .max_frame_size(1024 * 1024)
                    .max_message_size(1024 * 1024)
                    .on_upgrade(move |socket| async move {
                        registration
                            .run(super::transport::bridge(socket, connection))
                            .await;
                    })
                    .into_response()
            }
        }
    }
}

pub(super) fn target(id: &str) -> Result<SessionTarget, ()> {
    let id = decode_segment(id)?;
    Ok(if id == "active" {
        SessionTarget::Active
    } else {
        SessionTarget::Id(id)
    })
}

fn invalid_target() -> Response {
    error(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Invalid session id",
    )
}

pub(super) fn decode_segment(raw: &str) -> Result<String, ()> {
    decode_component(raw, false)
}

pub(super) fn decode_form_component(raw: &str) -> Result<String, ()> {
    decode_component(raw, true)
}

fn decode_component(raw: &str, form: bool) -> Result<String, ()> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        decoded.push(if byte == b'%' {
            let high = char::from(bytes.next().ok_or(())?).to_digit(16).ok_or(())?;
            let low = char::from(bytes.next().ok_or(())?).to_digit(16).ok_or(())?;
            u8::try_from(high * 16 + low).map_err(|_| ())?
        } else if form && byte == b'+' {
            b' '
        } else {
            byte
        });
    }
    String::from_utf8(decoded).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_routes_and_subscription_allowlist() {
        assert_eq!(classify("/api/v1/sessions"), Some(Route::Sessions));
        assert_eq!(
            classify("/api/v1/sessions/subscribe"),
            Some(Route::Subscribe(None))
        );
        assert_eq!(
            classify("/api/v1/sessions/active/subscribe"),
            Some(Route::Subscribe(Some("active")))
        );
        assert_eq!(
            classify("/api/v1/sessions/id/state/vosu.play"),
            Some(Route::State("id", Some("vosu.play")))
        );
        for path in [
            "/api/v1/sessions/",
            "/api/v1/sessions/subscribe/",
            "/api/v1/sessions/id/subscribe/precise",
            "/api/v1/sessions/id/state/a/b",
            "/api/v1/sessions//state",
        ] {
            assert_eq!(classify(path), None, "{path}");
        }
        assert_eq!(
            decode_segment("vosu%2Eplay+combo"),
            Ok("vosu.play+combo".into())
        );
        assert!(decode_segment("%GG").is_err());
    }
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod transport_tests;

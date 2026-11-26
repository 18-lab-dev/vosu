//! Credentials stop at the HTTP boundary; core receives only typed principals.
use super::{actions::ActionAuthority, error::RequestFailure};
use crate::actions::Principal;
use crate::server::{broker::BrokerToken, routes::PeerAddress};
use axum::{
    extract::{ConnectInfo, Request},
    http::{StatusCode, header},
};
use std::{net::SocketAddr, sync::Arc};

pub(super) const TOOL_SESSION: &str = "x-vosu-tool-session";

#[derive(Clone)]
pub(super) struct Credentials {
    pub principal: Principal,
    token: Option<String>,
}

impl Credentials {
    pub fn authenticate(
        request: &Request,
        tools: &dyn ActionAuthority,
    ) -> Result<Self, RequestFailure> {
        // Action authority never relaxes the outer peer policy. Additionally,
        // credentials are same-origin only, even if read CORS is configured open.
        for name in [header::ORIGIN, header::REFERER] {
            let mut values = request.headers().get_all(&name).iter();
            if let Some(value) = values.next() {
                let valid = values.next().is_none()
                    && value
                        .to_str()
                        .ok()
                        .and_then(|value| url::Url::parse(value).ok())
                        .zip(
                            request
                                .headers()
                                .get(header::HOST)
                                .and_then(|v| v.to_str().ok()),
                        )
                        .is_some_and(|(url, host)| {
                            request.headers().get_all(header::HOST).iter().count() == 1
                                && url::Url::parse(&format!("http://{host}"))
                                    .is_ok_and(|expected| url.origin() == expected.origin())
                        });
                if !valid {
                    return Err(RequestFailure(
                        StatusCode::FORBIDDEN,
                        "forbidden",
                        "Cross-origin action forbidden",
                    ));
                }
            }
        }
        let tokens = request.headers().get_all(TOOL_SESSION);
        if request.headers().contains_key(header::AUTHORIZATION) {
            if tokens.iter().next().is_some() {
                return Err(unauthorized());
            }
            let peer = request
                .extensions()
                .get::<ConnectInfo<PeerAddress>>()
                .map(|p| p.0.0.ip())
                .or_else(|| {
                    request
                        .extensions()
                        .get::<ConnectInfo<SocketAddr>>()
                        .map(|p| p.0.ip())
                })
                .or_else(|| request.extensions().get::<PeerAddress>().map(|p| p.0.ip()));
            let broker = request
                .extensions()
                .get::<Option<Arc<BrokerToken>>>()
                .and_then(Option::as_ref);
            if broker.is_some_and(|broker| broker.authorize(request.headers(), peer) == Ok(true)) {
                return Ok(Self {
                    principal: Principal::Broker,
                    token: None,
                });
            }
            return Err(unauthorized());
        }
        let mut tokens = tokens.iter();
        let token = tokens
            .next()
            .and_then(|value| value.to_str().ok())
            .filter(|s| !s.is_empty())
            .ok_or_else(unauthorized)?;
        if tokens.next().is_some() {
            return Err(unauthorized());
        }
        let principal = tools
            .authenticate_action_session(token)
            .ok_or_else(unauthorized)?;
        Ok(Self {
            principal,
            token: Some(token.to_owned()),
        })
    }

    pub fn authorize(
        &self,
        tools: &dyn ActionAuthority,
        action: &str,
    ) -> Result<(), RequestFailure> {
        if let Some(token) = &self.token {
            // Distinguish expired credentials from a live token without a grant.
            tools
                .authenticate_action_session(token)
                .ok_or_else(unauthorized)?;
            tools
                .authorize_action_session(token, action)
                .ok_or(RequestFailure(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "Action capability required",
                ))?;
        }
        Ok(())
    }
}

fn unauthorized() -> RequestFailure {
    RequestFailure(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "Action credential required",
    )
}

#[cfg(test)]
#[path = "tests_authorization.rs"]
mod tests;

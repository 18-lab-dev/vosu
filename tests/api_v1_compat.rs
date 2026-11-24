//! Public API composition: real ABI instances, core privacy/demand and TCP transport.
#![cfg(feature = "test-harness")]

#[path = "api_v1/actions.rs"]
mod actions;
#[path = "api_v1/resource_redirects.rs"]
mod resource_redirects;
#[path = "api_v1/resources.rs"]
mod resources;
#[path = "api_v1/security.rs"]
mod security;
#[path = "api_v1/sessions.rs"]
mod sessions;
#[path = "api_v1/state.rs"]
mod state;
#[path = "api_v1/subscriptions.rs"]
mod subscriptions;
#[path = "api_v1/support.rs"]
mod support;

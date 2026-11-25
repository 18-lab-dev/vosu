//! Public API composition: real ABI instances, core privacy/demand and TCP transport.
#![cfg(feature = "test-harness")]
#![expect(
    clippy::disallowed_methods,
    reason = "test code: drives the vosu CLI as a child process"
)]

#[path = "api_v1/actions.rs"]
mod actions;
#[path = "api_v1/cli.rs"]
mod cli;
#[path = "api_v1/import.rs"]
mod import;
#[path = "api_v1/import_fixture.rs"]
mod import_fixture;
#[path = "api_v1/job_fixture.rs"]
mod job_fixture;
#[path = "api_v1/jobs.rs"]
mod jobs;
#[path = "api_v1/relay.rs"]
mod relay;
#[path = "api_v1/resource_lifecycle.rs"]
mod resource_lifecycle;
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
#[path = "api_v1/trees.rs"]
mod trees;

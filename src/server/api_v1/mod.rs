//! Public API v1 transport adapters; composition and routing are owned by the server.
pub mod actions;
mod authorization;
pub mod error;
mod http;
mod jobs;
pub mod router;
mod selection;
mod session_events;
pub mod subscriptions;
mod transport;

#[cfg(test)]
mod pointer_properties;

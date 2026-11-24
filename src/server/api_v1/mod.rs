//! Public API v1 transport adapters; composition and routing are owned by the server.
pub mod error;
mod http;
mod resource_headers;
pub mod resources;
pub mod router;
mod selection;
mod session_events;
pub mod subscriptions;
mod transport;
mod trees;

#[cfg(test)]
mod pointer_properties;

//! Reloadable HTTP and WebSocket service.

#[cfg(feature = "test-harness")]
pub mod adapter_capture;
pub mod api_keys;
pub mod api_v1;
pub mod auth;
pub(crate) mod broker;
pub mod bus_provider;
pub mod catalog;
pub mod files;
pub mod ids;
pub mod origins;
pub mod pages;
pub mod routes;
pub mod runtime;
pub mod service;

pub mod commands;
pub mod subscription_wire;
pub mod websocket;

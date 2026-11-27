//! Production process composition root (M7 C0).
//!
//! This module deliberately owns process-wide resources so `main` remains a
//! thin argument/exit-code adapter. The executable injects its bundled set as
//! one [`Bundle`]; core composes it without naming any bundled crate.
#![cfg_attr(
    not(feature = "test-harness"),
    doc = "
Normal builds cannot obtain the test-harness-only WebSocket replay clock:
```compile_fail,E0432
use vosu_core::runtime::TestProductionComposition;
let _ = TestProductionComposition::websocket_replay_tick;
```"
)]

use std::{
    env,
    ffi::OsString,
    future::Future,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[cfg(any(test, feature = "test-harness"))]
use std::fs;

use arc_swap::ArcSwap;
#[cfg(test)]
use std::path::Path;
use tokio::{
    runtime::Handle,
    sync::{Mutex as AsyncMutex, broadcast, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use vosu_surface::filter::Filter;
use vosu_sys::platform::{PathResolver, Paths};

use crate::server::service::{
    ConfigUpdateService, OverlayComposition, ProductionRouteDelegate, ProductionServices,
    ReportInputProvider, UpdateRouteResult, UpdaterHandoff, WebSocketEndpoints,
};
use crate::server::{
    auth::{AuthorizationDecision, AuthorizationPolicy, RequestHeaders},
    routes::{RequestAuthorizer, RouteProviders},
    runtime::{
        AxumListenerFactory, ListenTarget, ListenerRuntime, ListenerState, TokioBinder,
        TokioResolver,
    },
    websocket::{
        Schema as WebSocketSchema, StateProvider as WebSocketStateProvider,
        StateSnapshot as WebSocketStateSnapshot, WebSocketHub,
    },
};
use crate::{
    app::{
        App, EnvironmentSnapshot, HookFuture, RuntimeHooks, StartupDecision, StartupOutcome,
        StartupPlatform,
    },
    config::{Config, ConfigChanged, ConfigDocument, ConfigStore},
    logging::{Logger, LoggerOptions, StdoutConsole},
    plugin::{
        PluginHost,
        settings::{ConfigSettingsBridgeB2, PluginSettingsService, SettingsStorage},
    },
    report::{ReportInput, ReportInstance},
    updater::{
        LATEST_RELEASE_API, NativePlatform, RELEASE_PLATFORM, RecoveryAction, StartupDispatch,
        StartupReadiness, UpdateCheck, check_latest_and_prepare, check_startup_and_prepare,
        dispatch_startup_role, recover_startup_update,
    },
};

/// Daemon logger options: every console line is prefixed with the bundle's
/// version, never a library crate's own package version.
pub fn daemon_logger_options(bundle: &Bundle, debug_log: bool) -> LoggerOptions {
    LoggerOptions {
        version: bundle.version().to_string(),
        debug_log,
        ..LoggerOptions::default()
    }
}

mod adapters;
mod bundle;
mod catalog;
mod cli;
mod config;
mod dashboard;
mod dev_plugins;
mod discovery;
mod lifecycle;
mod overlay_root;
mod plugin_management;
mod projection;
mod service;
mod signals;
mod startup;
#[cfg(any(test, feature = "bench-harness"))]
mod test_paths;
#[cfg(feature = "test-harness")]
mod test_support;
mod tracing_log;
mod worker;

pub use bundle::{
    AdapterRequesters, AdapterServices, Bundle, BundledHosts, HostRoutes, OverlayCatalog,
    OverlayInputs, OverlayRootSetting, Projections,
};
pub use catalog::{CatalogComposition, CatalogRoots, catalog_target};
#[cfg(feature = "test-harness")]
pub use config::test_updater_handoff;
pub use lifecycle::{ProductionRuntime, run_cli};
#[cfg(feature = "bench-harness")]
pub use lifecycle::{TestAdapters, TestDaemon};
pub use overlay_root::RuntimeSettingsStorage;
pub use projection::ReloadableAuthorizer;
pub use startup::is_startup_owner_conflict;
#[cfg(feature = "test-harness")]
pub use test_support::{
    TestGame, TestProductionComposition, TestProductionCompositionOptions,
    start_test_production_composition,
};
pub use worker::{ListenerOwner, WebSocketHubs};

use config::*;
use projection::*;
use service::*;
#[cfg(test)]
use signals::cancel_on_signal;
use startup::*;

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use tokio::sync::mpsc;

    use super::test_paths::{RuntimeTestPathResolver, runtime_test_paths};
    use super::*;
    use vosu_proptest::bounded;

    include!("runtime/tests/cli.rs");
    include!("runtime/tests/cli_broker.rs");
    include!("runtime/tests/worker.rs");
    include!("runtime/tests/service.rs");
    include!("runtime/tests/package_limit.rs");
    include!("runtime/tests/version.rs");
    include!("runtime/tests/lifecycle.rs");
}

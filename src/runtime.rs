//! Production process composition root.
//!
//! This module deliberately owns process-wide resources so `main` remains a
//! thin argument/exit-code adapter. Service-specific adapters are composed here
//! rather than hidden behind global state.

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
use bytes::Bytes;
#[cfg(test)]
use std::path::Path;
use tokio::{
    runtime::Handle,
    sync::{Mutex as AsyncMutex, broadcast, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use vosu_adapter_support::filtered::Filter;
use vosu_osu::capture::poll_duration;
use vosu_sys::platform::{PathResolver, Paths};

use crate::{
    counters::ReloadableCounterService,
    server::{
        auth::{AuthorizationDecision, AuthorizationPolicy, RequestHeaders},
        catalog::{CatalogOverlay, CatalogService, LocalOverlay},
        commands::{CommandProcessor, SettingsProvider},
        pages::{DynamicPage, EmbeddedResponse, PageService},
        routes::{CachedJson, RequestAuthorizer, RouteProviders, StateJsonProvider, StateRoute},
        runtime::{
            AxumListenerFactory, ListenTarget, ListenerRuntime, ListenerState, TokioBinder,
            TokioResolver,
        },
        service::{
            BeatmapFiles, ConfigUpdateService, CurrentBeatmapProvider, DeclaredCalculateProvider,
            PageDataProvider, ProductionRouteDelegate, ProductionServices, ReportInputProvider,
            UpdateRouteResult, UpdaterHandoff, WebSocketEndpoints, WebSocketRelaySink,
        },
        websocket::{
            Schema as WebSocketSchema, StateProvider as WebSocketStateProvider,
            StateSnapshot as WebSocketStateSnapshot, WebSocketHub,
        },
    },
};
use vosu_core::{
    app::{
        App, EnvironmentSnapshot, HookFuture, RuntimeHooks, StartupDecision, StartupFlags,
        StartupOutcome, StartupPlatform,
    },
    config::{Config, ConfigChanged, ConfigDocument, ConfigStore},
    logging::{Logger, LoggerOptions, StdoutConsole},
    plugin::{
        PluginHost,
        settings::{ConfigSettingsBridgeB2, PluginSettingsService, SettingsStorage},
    },
    report::{ReportCounter, ReportInput, ReportInstance},
    updater::{
        LATEST_RELEASE_API, NativePlatform, RELEASE_PLATFORM, RecoveryAction, StartupDispatch,
        StartupReadiness, UpdateCheck, check_latest_and_prepare, check_startup_and_prepare,
        dispatch_startup_role, recover_startup_update,
    },
};
use vosu_osu_reader::discovery::{StartupArgs, StartupValue};

/// The running executable's version: the root package's, never a library
/// crate's own package version. The updater, dashboard, catalog and log
/// prefix all report this one.
pub(crate) fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("root package version is valid semver")
}

/// Daemon logger options; every console line is prefixed with [`current_version`].
fn daemon_logger_options(debug_log: bool) -> LoggerOptions {
    LoggerOptions {
        version: current_version().to_string(),
        debug_log,
        ..LoggerOptions::default()
    }
}

mod adapters;
mod cli;
mod config;
mod dev_plugins;
mod discovery;
mod lifecycle;
mod plugin_management;
mod preview_scheme;
mod projection;
mod render;
mod service;
mod signals;
#[cfg(feature = "test-harness")]
mod test_support;
mod tool_context;
mod tracing_log;
mod worker;

#[cfg(feature = "test-harness")]
pub use config::test_updater_handoff;
pub use lifecycle::{ProductionRuntime, is_startup_owner_conflict, run_cli};
pub use projection::ReloadableAuthorizer;
#[cfg(feature = "test-harness")]
pub use test_support::{
    TestGame, TestProductionComposition, TestProductionCompositionOptions,
    start_test_production_composition,
};
pub use worker::{ListenerOwner, WebSocketHubs};

use config::*;
#[cfg(test)]
use lifecycle::*;
use projection::*;
use render::*;
use service::*;
#[cfg(test)]
use signals::cancel_on_signal;

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use tokio::sync::mpsc;

    use super::*;

    include!("runtime/tests/cli.rs");
    include!("runtime/tests/projection.rs");
    include!("runtime/tests/worker.rs");
    include!("runtime/tests/service.rs");
    include!("runtime/tests/update_notice.rs");
    include!("runtime/tests/version.rs");
    include!("runtime/tests/lifecycle.rs");
}

//! Production process composition root.
//!
//! This module deliberately owns process-wide resources so `main` remains a
//! thin argument/exit-code adapter. Service-specific adapters are composed here
//! rather than hidden behind global state.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

#[cfg(any(test, feature = "test-harness"))]
use std::fs;

use arc_swap::ArcSwap;
use bytes::Bytes;
use serde::Serialize;
use tokio::{
    runtime::Handle,
    sync::{Mutex as AsyncMutex, broadcast, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use vosu_osu::capture::poll_duration;
use vosu_osu_model::enums::ClientType;
use vosu_sys::platform::{PathResolver, Paths};

use crate::{
    api::{
        ApiError, ApiRuntimeSettings, ApiSchema, CombinedSnapshot, ManagerInstanceSnapshot,
        ManagerSnapshot, SnapshotManager,
    },
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
            BeatmapFiles, ConfigUpdateService, CurrentBeatmapProvider, LegacyCalculateProvider,
            PageDataProvider, ProductionRouteDelegate, ProductionServices, ReportInputProvider,
            SnapshotStateProvider, UpdateRouteResult, UpdaterHandoff, WebSocketEndpoints,
            WebSocketRelaySink,
        },
        v2_subscription::{V2SubscriptionHub, V2SubscriptionProvider, V2SubscriptionSnapshot},
        websocket::{
            Filter, Schema as WebSocketSchema, StateProvider as WebSocketStateProvider,
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
        current_version, dispatch_startup_role, recover_startup_update,
    },
};
use vosu_osu_reader::discovery::{StartupArgs, StartupValue};
use vosu_osu_state::{StateBundleSnapshot, StateWorkerError, duration_from_millis_saturating};

pub(crate) mod bridge_b1;
mod cli;
mod config;
mod dev_plugins;
mod discovery;
mod lifecycle;
mod preview_scheme;
mod projection;
mod render;
mod service;
#[cfg(feature = "test-harness")]
mod test_support;
mod tracing_log;
mod worker;

#[cfg(feature = "test-harness")]
pub use config::test_updater_handoff;
pub use lifecycle::{ProductionRuntime, is_startup_owner_conflict, run_cli};
pub use projection::{
    ProductionSnapshotEncoder, ReloadableAuthorizer, SnapshotEncoder, SnapshotProviders,
};
#[cfg(feature = "test-harness")]
pub use test_support::{
    TestProductionComposition, TestProductionCompositionOptions, start_test_production_composition,
};
pub use worker::{ListenerOwner, WebSocketHubs};

#[cfg(test)]
use bridge_b1::*;
use config::*;
#[cfg(test)]
use lifecycle::*;
use projection::*;
use render::*;
use service::*;
#[cfg(test)]
use vosu_osu_state::{Delivery, SnapshotPublisher};

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, atomic::Ordering};

    use tokio::sync::mpsc;

    use super::*;

    include!("runtime/tests/cli.rs");
    include!("runtime/tests/worker.rs");
    include!("runtime/tests/bridge_b1.rs");
    include!("runtime/tests/projection.rs");
    include!("runtime/tests/service.rs");
    include!("runtime/tests/package_limit.rs");
    include!("runtime/tests/update_notice.rs");
    include!("runtime/tests/lifecycle.rs");
}

//! Production process composition root.
//!
//! This module deliberately owns process-wide resources so `main` remains a
//! thin argument/exit-code adapter. Service-specific adapters are composed here
//! rather than hidden behind global state.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use bytes::Bytes;
use serde::Serialize;
use tokio::{
    sync::{Mutex as AsyncMutex, broadcast, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    api::{
        ApiError, ApiRuntimeSettings, ApiSchema, CombinedSnapshot, ManagerInstanceSnapshot,
        ManagerSnapshot, SnapshotManager,
    },
    app::{
        App, EnvironmentSnapshot, HookFuture, RuntimeHooks, StartupArgs, StartupDecision,
        StartupOutcome, StartupPlatform,
    },
    assets::DaemonAssets,
    calculation::default_calculated_mods,
    config::{Config, ConfigChanged, ConfigStore},
    counters::ReloadableCounterService,
    instance::{
        InstanceGeneration, InstanceLifecycleSnapshot, InstanceRuntime, NativeProcessProvider,
    },
    logging::{Logger, LoggerOptions, StdoutConsole},
    memory::{
        ReaderSynchronization, SynchronizedMemoryReader,
        lazer::{LazerMemory, LazerPattern},
        lazer_loader::{LazerOffsetLoader, ReqwestOffsetFetcher},
        stable::{StableMemory, StablePatterns},
    },
    model::enums::{ClientType, GameState},
    platform::{PathResolver, Paths},
    report::{ReportCounter, ReportInput, ReportInstance},
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
    state::{
        PollIntervalUpdates, ReadContext, RegularCapturePlan, SnapshotPublisher,
        SourcePreciseReader, SourceRegularReader, StateBundle, StateBundleSnapshot,
        StateWorkerError, StateWorkerHandle, duration_from_millis_saturating, spawn_state_worker,
    },
    updater::{
        LATEST_RELEASE_API, NativePlatform, RELEASE_PLATFORM, RecoveryAction, StartupDispatch,
        StartupReadiness, UpdateCheck, check_latest_and_prepare, check_startup_and_prepare,
        current_version, dispatch_startup_role, recover_startup_update,
    },
};

mod config;
mod lifecycle;
mod projection;
mod render;
mod service;
#[cfg(feature = "test-harness")]
mod test_support;
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
pub use worker::{ListenerOwner, ProductionStateSupervisor, WebSocketHubs};

use config::*;
#[cfg(test)]
use lifecycle::*;
use projection::*;
use render::*;
use service::*;
use worker::*;

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use tokio::sync::mpsc;

    use super::*;

    include!("runtime/tests/worker.rs");
    include!("runtime/tests/projection.rs");
    include!("runtime/tests/service.rs");
    include!("runtime/tests/lifecycle.rs");
}

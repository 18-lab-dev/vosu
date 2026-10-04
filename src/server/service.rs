//! Concrete production adapters joining immutable state, legacy calculation, and HTTP services.

use std::{
    collections::HashMap,
    convert::Infallible,
    io::SeekFrom,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError, Weak},
    task::{Context, Poll},
};

use axum::{
    body::{Body, Bytes},
    extract::{
        ConnectInfo,
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket},
    },
    http::{Request, StatusCode, header},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::{io::ReaderStream, sync::CancellationToken};

use crate::{
    api::{self, ApiError, SnapshotManager},
    calculation::engine::{
        HitResultPriority, LegacyMods, LegacyPerformanceRequest, LegacyRosuEngine,
    },
    counters::{
        CounterError, CounterService, CounterSetting, ReloadableCounterService, SettingInput,
    },
    report::{NativeReportProbe, ReportInput, ReportProbe, generate_report_document},
};

use super::{
    commands::{CommandAction, CommandClient, CommandProcessor, Relay, RelaySink},
    files::{self, MissingKind, ResourceOutcome},
    pages::{DynamicPage, EmbeddedResponse, PageLookup, PageService, StaticFallback},
    routes::{
        CachedJson, CalculatePpProvider, CalculateResult, Endpoint, PeerAddress, PpQuery,
        ProviderFuture, ResponseFlushBarrier, ResponseFlushRegistry, RouteDelegate,
        StateJsonProvider, StateRoute,
    },
    v2_subscription::{
        SubscriptionRequestError, V2SubscriptionHub, V2SubscriptionOutbound,
        parse_subscription_request,
    },
    websocket::{self, ConnectionControl, Inbound, Outbound, UpgradeCallbackTracker, WebSocketHub},
};

const JSON: &str = "application/json; charset=utf-8";
const NOT_FOUND: &str = "Not Found";

include!("service/providers.rs");
include!("service/relay.rs");
include!("service/routing.rs");
include!("service/websocket_transport.rs");
include!("service/responses.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ApiRuntimeSettings, ManagerSnapshot};
    use crate::server::routes::ModsInput;

    #[derive(Default)]
    struct Current {
        bytes: Option<Bytes>,
        files: Option<BeatmapFiles>,
    }

    impl CurrentBeatmapProvider for Current {
        fn files(&self) -> BeatmapFiles {
            self.files.clone().unwrap_or_default()
        }
        fn current_beatmap_bytes(&self) -> Result<Option<Bytes>, String> {
            Ok(self.bytes.clone())
        }
    }

    struct NoState {
        events: tokio::sync::watch::Receiver<u64>,
    }

    impl websocket::StateProvider for NoState {
        fn poll_rate(&self, _: websocket::Schema) -> std::time::Duration {
            std::time::Duration::from_millis(100)
        }
        fn subscribe(&self, _: websocket::Schema) -> tokio::sync::watch::Receiver<u64> {
            self.events.clone()
        }
        fn capture(&self, _: websocket::Schema) -> Option<Arc<dyn websocket::StateSnapshot>> {
            None
        }
    }

    impl crate::server::v2_subscription::V2SubscriptionSnapshot for NoState {
        fn revision(&self) -> u64 {
            *self.events.borrow()
        }

        fn document(&self) -> Result<Option<Value>, ApiError> {
            Ok(None)
        }
    }

    impl crate::server::v2_subscription::V2SubscriptionProvider for NoState {
        fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
            self.events.clone()
        }

        fn capture(&self) -> Arc<dyn crate::server::v2_subscription::V2SubscriptionSnapshot> {
            Arc::new(NoState {
                events: self.events.clone(),
            })
        }
    }

    struct CommandProviders;

    impl super::super::commands::SettingsProvider for CommandProviders {
        fn get(&self, _: &str) -> Result<Value, String> {
            Ok(json!({}))
        }
        fn save(&self, _: &str, _: Value) -> Result<(), String> {
            Ok(())
        }
    }

    struct UnusedServices;

    impl PageDataProvider for UnusedServices {
        fn render(
            &self,
            _: DynamicPage,
            _: Option<String>,
        ) -> ProviderFuture<'_, Result<EmbeddedResponse, String>> {
            Box::pin(async { Err("unused".into()) })
        }
        fn search_counters(
            &self,
            _: &str,
            _: bool,
            _: Option<String>,
        ) -> ProviderFuture<'_, Result<Bytes, String>> {
            Box::pin(async { Err("unused".into()) })
        }
    }

    impl ConfigUpdateService for UnusedServices {
        fn save(&self, _: Value) -> ProviderFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    impl ReportInputProvider for UnusedServices {
        fn capture(&self) -> Result<ReportInput, String> {
            Err("unused".into())
        }
    }

    impl UpdaterHandoff for UnusedServices {
        fn run_update(&self) -> ProviderFuture<'_, Result<UpdateRouteResult, String>> {
            Box::pin(async { Ok(UpdateRouteResult::Latest) })
        }
    }

    struct UpdatedServices(CancellationToken);

    impl UpdaterHandoff for UpdatedServices {
        fn run_update(&self) -> ProviderFuture<'_, Result<UpdateRouteResult, String>> {
            let started = self.0.clone();
            Box::pin(async move {
                Ok(UpdateRouteResult::Updated(DeferredUpdateTrigger::new(
                    move || started.cancel(),
                )))
            })
        }
    }

    struct AllowAll;

    impl super::super::routes::RequestAuthorizer for AllowAll {
        fn authorize(
            &self,
            _: std::net::IpAddr,
            _: super::super::auth::RequestHeaders<'_>,
        ) -> super::super::auth::AuthorizationDecision {
            super::super::auth::AuthorizationDecision {
                peer_ok: true,
                header_ok: true,
            }
        }
    }

    include!("service/tests/providers_and_transport.rs");
    include!("service/tests/update_lifecycle.rs");
    include!("service/tests/listener_and_files.rs");
}

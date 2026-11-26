//! Concrete production adapters joining immutable state, legacy calculation, and HTTP services.

use std::{
    collections::HashMap,
    convert::Infallible,
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
    http::{Method, Request, StatusCode, header},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
#[cfg(test)]
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use super::files::{MissingKind, ResourceOutcome};

use vosu_adapter_upstream::calculate::PpQuery;

use vosu_core::report::{NativeReportProbe, ReportInput, ReportProbe, generate_report_document};
use vosu_overlay_host::ReloadableCounterService;
use vosu_overlay_host::commands::{
    CommandAction, CommandClient, CommandProcessor, Relay, RelaySink,
};

use super::{
    files,
    pages::{EmbeddedResponse, PageLookup, PageService, StaticFallback},
    routes::{
        CachedJson, CalculatePpProvider, CalculateResult, Endpoint, PeerAddress, ProviderFuture,
        ResponseFlushBarrier, ResponseFlushRegistry, RouteDelegate, StateJsonProvider, StateRoute,
    },
    websocket::{self, Inbound, Outbound, UpgradeCallbackTracker, WebSocketHub},
};

const JSON: &str = "application/json; charset=utf-8";
const NOT_FOUND: &str = "Not Found";

include!("service/providers.rs");
include!("service/relay.rs");
include!("service/routing.rs");
include!("service/websocket_transport.rs");
include!("service/dashboard.rs");
include!("service/responses.rs");
include!("service/surface.rs");
include!("service/declared_calculate.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use vosu_proptest::bounded;

    #[derive(Default)]
    struct Current {
        files: Option<BeatmapFiles>,
    }

    impl CurrentBeatmapProvider for Current {
        fn files(&self) -> BeatmapFiles {
            self.files.clone().unwrap_or_default()
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

    impl StateJsonProvider for NoState {
        fn cached_json(&self, _: StateRoute) -> ProviderFuture<'_, CachedJson> {
            Box::pin(async { CachedJson::NotReady })
        }
    }
    struct CommandProviders;

    impl vosu_overlay_host::commands::SettingsProvider for CommandProviders {
        fn get(&self, _: &str) -> Result<Value, String> {
            Ok(json!({}))
        }
        fn save(&self, _: &str, _: Value) -> Result<(), String> {
            Ok(())
        }
    }

    struct UnusedServices;

    impl ConfigUpdateService for UnusedServices {
        fn save(&self, _: Value) -> ProviderFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn view(&self) -> Value {
            json!({})
        }
    }

    impl ReportInputProvider for UnusedServices {
        fn capture(&self) -> Result<ReportInput, String> {
            Err("unused".into())
        }
    }

    impl UpdaterHandoff for UnusedServices {
        fn run_update(&self, _: bool) -> ProviderFuture<'_, Result<UpdateRouteResult, String>> {
            Box::pin(async { Ok(UpdateRouteResult::Latest) })
        }
    }

    struct UpdatedServices(CancellationToken);

    impl UpdaterHandoff for UpdatedServices {
        fn run_update(&self, _: bool) -> ProviderFuture<'_, Result<UpdateRouteResult, String>> {
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
    include!("service/tests/skin_file_index.rs");
    include!("service/tests/command_filters.rs");
}

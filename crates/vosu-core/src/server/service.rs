//! Core's production route delegate (M7 C0): dashboard management, page
//! and file dispatch to registered surface routes, the legacy WebSocket
//! bridge and the overlay host's admitted requests, over the bundled hosts'
//! seams ([`OverlayComposition`]).

use std::{
    collections::HashMap,
    convert::Infallible,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
};

#[cfg(test)]
use crate::server::files;
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
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::server::files::{MissingKind, ResourceOutcome};

use crate::report::{NativeReportProbe, ReportInput, ReportProbe, generate_report_document};

use crate::server::{
    pages::EmbeddedResponse,
    routes::{
        CachedJson, Endpoint, PeerAddress, ProviderFuture, ResponseFlushBarrier,
        ResponseFlushRegistry, RouteDelegate,
    },
    websocket::{self, Inbound, Outbound, UpgradeCallbackTracker, WebSocketHub},
};

/// The content type of every JSON body the registered surface routes serve.
pub const JSON: &str = "application/json; charset=utf-8";
const NOT_FOUND: &str = "Not Found";

include!("service/overlay.rs");
include!("service/providers.rs");
include!("service/endpoints.rs");
include!("service/routing.rs");
include!("service/websocket_transport.rs");
include!("service/dashboard.rs");
include!("service/responses.rs");
include!("service/surface.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use vosu_proptest::bounded;

    #[derive(Default)]
    pub(super) struct TestOverlay(pub(super) PathBuf);

    impl OverlayComposition for TestOverlay {
        fn current_root(&self) -> PathBuf {
            self.0.clone()
        }
        fn reload_root(&self, _: PathBuf) {
            unreachable!("fixture has no root reload")
        }
        fn mount(&self, _: WebSocketHub) -> std::io::Result<()> {
            unreachable!("fixture has no routes to mount")
        }
        fn serve_static(&self, _: SurfaceRequest) -> ResponseFuture<'_, SurfaceResponse> {
            unreachable!("fixture has no static provider")
        }
        fn has_settings(&self, id: &str) -> bool {
            self.0.join(id).join("index.html").is_file()
        }
        fn installed_names(&self) -> HashMap<String, String> {
            if self.has_settings("counter") {
                HashMap::from([("counter".into(), "counter".into())])
            } else {
                HashMap::new()
            }
        }
        fn report_counters(&self) -> Result<Vec<crate::report::ReportCounter>, String> {
            Ok(vec![])
        }
        fn values_path(&self, _: &str) -> PathBuf {
            unreachable!("fixture has no saved values")
        }
        fn valid_folder(&self, name: &str) -> bool {
            valid_overlay_component(name)
        }
        fn query_overlay(&self, query: &str) -> Option<String> {
            let id = query
                .strip_prefix("/@vosu-profile/stream/")
                .unwrap_or(query);
            Some(id.to_owned())
        }
        fn command_session(
            &self,
            _: u64,
            _: u64,
            _: Option<&str>,
            _: Arc<dyn RelayControl>,
        ) -> Option<Box<dyn CommandSession>> {
            Some(Box::new(TestSession))
        }
        fn shutdown(&self) -> ProviderFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    struct TestSession;
    impl CommandSession for TestSession {
        fn process(&self, _: &str) -> CommandReply {
            CommandReply::NoAck
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

    impl crate::server::routes::RequestAuthorizer for AllowAll {
        fn authorize(
            &self,
            _: std::net::IpAddr,
            _: crate::server::auth::RequestHeaders<'_>,
        ) -> crate::server::auth::AuthorizationDecision {
            crate::server::auth::AuthorizationDecision {
                peer_ok: true,
                header_ok: true,
            }
        }
    }

    include!("service/tests/providers_and_transport.rs");
    include!("service/tests/update_lifecycle.rs");
    include!("service/tests/listener_and_files.rs");
    include!("service/tests/skin_file_index.rs");
}

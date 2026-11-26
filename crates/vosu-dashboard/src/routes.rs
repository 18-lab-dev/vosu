//! The dashboard's exact dashboard-listener registrations: four page shells
//! and the dashboard asset set. Core composes them through `vosu-surface`;
//! management data reaches the page only through core's JSON routes and the
//! page context core injects (A7.1).
use std::sync::Arc;

use bytes::Bytes;
use vosu_surface::{
    listener::ListenerRole,
    routes::{
        ProviderResponse, ResponseBody, ResponseFuture, RouteDescriptor, RoutePath, RouteProvider,
        SurfaceRequest, SurfaceResponse,
    },
};

use crate::page::{self, Page};

pub const PAGE_PATHS: [&str; 4] = ["/", "/settings", "/available", "/tools"];

pub struct Routes {
    version: String,
}

impl Routes {
    /// `version` is the running vosu version the page shell shows.
    pub fn new(version: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            version: version.into(),
        })
    }

    /// Every registration, `GET` on the dashboard listener only.
    pub fn descriptors(self: &Arc<Self>) -> Vec<(RouteDescriptor, Arc<dyn RouteProvider>)> {
        PAGE_PATHS
            .iter()
            .map(|path| (*path).to_owned())
            .chain(
                crate::assets::ASSETS
                    .iter()
                    .map(|asset| format!("/assets/{}", asset.name)),
            )
            .map(|path| {
                (
                    RouteDescriptor {
                        listener: ListenerRole::Dashboard,
                        method: "GET".into(),
                        path: RoutePath::Exact(path),
                        websocket: false,
                    },
                    Arc::clone(self) as Arc<dyn RouteProvider>,
                )
            })
            .collect()
    }

    pub fn response(&self, path: &str, query: &[(String, String)]) -> Option<SurfaceResponse> {
        let (content_type, body) = match Page::for_request(path, query) {
            Some(page) => (
                "text/html; charset=utf-8",
                Bytes::from(page::render(page, &self.version)),
            ),
            None => {
                let asset = crate::assets::find(path.strip_prefix("/assets/")?)?;
                (asset.content_type, Bytes::from_static(asset.bytes))
            }
        };
        Some(SurfaceResponse {
            status: 200,
            headers: vec![("content-type".into(), content_type.into())],
            body: ResponseBody::Bytes(body),
        })
    }
}

impl RouteProvider for Routes {
    fn respond(&self, request: SurfaceRequest) -> ResponseFuture<'_, ProviderResponse> {
        let response = self
            .response(&request.path, &request.query)
            .unwrap_or(SurfaceResponse {
                status: 404,
                headers: vec![("content-type".into(), "text/plain; charset=utf-8".into())],
                body: ResponseBody::Bytes(Bytes::from_static(b"Not Found")),
            });
        Box::pin(async move { ProviderResponse::Http(response) })
    }
}

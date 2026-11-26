//! Request handling for every tool host registration (A7.2 retained routes).
//! Every route is registered by exact method and path, or as one installed
//! tool's `/tools/<id>` prefix; nothing here mints or checks a session.
use crate::assets;
use bytes::Bytes;
use std::path::Path;
use vosu_surface::{
    files::{FileError, RelativeChild},
    routes::{
        ProviderResponse, ResponseBody, ResponseFuture, RouteProvider, SurfaceRequest,
        SurfaceResponse,
    },
    tool_view::{ToolHostViews, ToolPresentation},
};

pub(crate) const BRIDGE: &str = "/tools/_host/v2/bridge.js";
pub(crate) const THEME: &str = "/tools/_host/v1/theme.css";
pub(crate) const DESIGN: &str = "/tools/_host/v1/design";
const JSON: &str = "application/json";
/// Art may come from a tool the user has not trusted; an SVG opened directly
/// must not run script on the overlay origin.
const ART_POLICY: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; sandbox";

pub(crate) struct Routes {
    pub(crate) views: ToolHostViews,
}

impl RouteProvider for Routes {
    fn respond(&self, request: SurfaceRequest) -> ResponseFuture<'_, ProviderResponse> {
        Box::pin(async move { ProviderResponse::Http(self.http(request).await) })
    }
}

fn bytes(status: u16, content_type: &str, body: Bytes) -> SurfaceResponse {
    SurfaceResponse {
        status,
        headers: vec![("content-type".into(), content_type.into())],
        body: ResponseBody::Bytes(body),
    }
}
fn error(status: u16, message: &str) -> SurfaceResponse {
    bytes(
        status,
        JSON,
        Bytes::from(serde_json::json!({ "error": message }).to_string()),
    )
}
fn redirect(location: String) -> SurfaceResponse {
    SurfaceResponse {
        status: 302,
        headers: vec![("location".into(), location)],
        body: ResponseBody::Empty,
    }
}

/// Percent-decode a request path exactly once; malformed encoding is `None`.
fn decode_once(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = encoded.get(index + 1..index + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

impl Routes {
    async fn http(&self, request: SurfaceRequest) -> SurfaceResponse {
        let get = request.method == "GET";
        match request.path.as_str() {
            BRIDGE if get => return self.bridge(),
            THEME if get => {
                return bytes(
                    200,
                    "text/css; charset=utf-8",
                    Bytes::from_static(assets::THEME.as_bytes()),
                );
            }
            DESIGN if get => {
                return bytes(
                    200,
                    "text/html; charset=utf-8",
                    Bytes::from_static(assets::DESIGN),
                );
            }
            path if get && let Some(body) = assets::design_system(path) => {
                return bytes(200, vosu_surface::files::content_type(Path::new(path)), body);
            }
            _ => {}
        }
        if !get && request.method != "HEAD" {
            return error(404, "not found");
        }
        let Some(rest) = request.path.strip_prefix("/tools/") else {
            return error(404, "not found");
        };
        let (id, tail) = match rest.split_once('/') {
            Some((id, tail)) => (id, Some(tail)),
            None => (rest, None),
        };
        // Clone the entry out of the latest view; no view borrow crosses an await.
        let tool = self
            .views
            .borrow()
            .tools
            .iter()
            .find(|tool| tool.id == id)
            .cloned();
        let Some(tool) = tool else {
            return error(404, "unknown tool");
        };
        let Some(tail) = tail else {
            return redirect(format!("{}/", request.path));
        };
        let Some(decoded) = decode_once(tail) else {
            return error(404, "not found");
        };
        // The deleted 1.x service proxy never falls through to static files.
        if decoded.split(['/', '\\']).next() == Some("api") {
            return error(404, "not found");
        }
        let art = match decoded.as_str() {
            "_icon" => Some(&tool.icon),
            "_banner" => Some(&tool.banner),
            _ => None,
        };
        if let Some(art) = art {
            // Declared art identifies a tool in every state.
            let Some(art) = art else {
                return error(404, "not found");
            };
            return serve(&tool, art, get, Some(ART_POLICY), None).await;
        }
        if !tool.servable {
            return error(503, "tool is unavailable");
        }
        if decoded == "_settings" {
            if !get {
                return error(404, "not found");
            }
            return match &tool.settings {
                Some(values) => {
                    let mut body = Vec::with_capacity(values.len() + 11);
                    body.extend_from_slice(b"{\"values\":");
                    body.extend_from_slice(values);
                    body.push(b'}');
                    bytes(200, JSON, Bytes::from(body))
                }
                None => error(500, "tool settings are unreadable"),
            };
        }
        let relative = if decoded.is_empty() {
            tool.ui_entry.clone()
        } else if decoded.ends_with('/') {
            format!("{decoded}index.html")
        } else {
            decoded.clone()
        };
        let directory = (!decoded.is_empty() && !decoded.ends_with('/'))
            .then(|| format!("{}/", request.path));
        serve(&tool, &ui_path(&tool, &relative), get, None, directory).await
    }

    fn bridge(&self) -> SurfaceResponse {
        let view = self.views.borrow().clone();
        let metadata = serde_json::Value::Array(
            view.tools
                .iter()
                .map(|tool| {
                    serde_json::json!({"id": tool.id, "name": tool.name, "version": tool.version})
                })
                .collect(),
        );
        let source = format!(
            "const metadata = {metadata};\nconst dashboardUrl = {};\n{}",
            serde_json::json!(view.dashboard_origin),
            assets::BRIDGE
        );
        bytes(
            200,
            "text/javascript; charset=utf-8",
            Bytes::from(source),
        )
    }
}

fn ui_path(tool: &ToolPresentation, relative: &str) -> String {
    match tool.ui_root.trim_end_matches('/') {
        "" | "." => relative.to_owned(),
        root => format!("{root}/{relative}"),
    }
}

/// Opens one package-relative file through the core-minted read-only lease.
/// `directory` is the trailing-slash location to redirect to when the request
/// names a UI directory that has an `index.html`.
async fn serve(
    tool: &ToolPresentation,
    relative: &str,
    get: bool,
    policy: Option<&str>,
    directory: Option<String>,
) -> SurfaceResponse {
    let Ok(child) = RelativeChild::parse(relative) else {
        return error(404, "not found");
    };
    if relative.contains(['\\', '\0']) {
        return error(404, "not found");
    }
    let file = match tool.root.open(&child).await {
        Ok(file) => file,
        Err(FileError::NotFound | FileError::InvalidPath) if directory.is_some() => {
            let index = RelativeChild::parse(&format!("{relative}/index.html"));
            return match index {
                Ok(index) if tool.root.open(&index).await.is_ok() => {
                    redirect(directory.expect("checked above"))
                }
                _ => error(404, "not found"),
            };
        }
        Err(_) => return error(404, "not found"),
    };
    let length = file.metadata.size;
    let mut headers = vec![
        (
            "content-type".to_owned(),
            vosu_surface::files::content_type(Path::new(relative)).to_owned(),
        ),
        ("content-length".to_owned(), length.to_string()),
    ];
    if let Some(policy) = policy {
        headers.push(("content-security-policy".into(), policy.into()));
    }
    SurfaceResponse {
        status: 200,
        headers,
        body: if get {
            ResponseBody::File {
                file,
                offset: 0,
                length,
            }
        } else {
            ResponseBody::Empty
        },
    }
}

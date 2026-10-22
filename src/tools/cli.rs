use super::*;
use axum::{
    extract::{ConnectInfo, Request},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};

fn response(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}
fn runnable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: path is a live NUL-terminated string; access only queries permissions.
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    }
    #[cfg(windows)]
    {
        true
    }
}
impl ToolHost {
    pub(super) fn cli_owner(&self, token: &str) -> Option<Arc<Entry>> {
        let id = {
            let mut sessions = self.cli_sessions.lock();
            sessions.retain(|_, s| s.expires > Instant::now());
            sessions
                .values()
                .find(|s| equal(&s.token, token))
                .map(|s| s.tool.clone())
        }?;
        let entry = self.entry(&id).ok()?;
        (!matches!(
            self.status(&entry).0.as_str(),
            "untrusted" | "disabled" | "invalid" | "blocked"
        ))
        .then_some(entry)
    }
    pub(super) async fn cli_api(
        self: &Arc<Self>,
        path: &str,
        request: Request,
        body: &[u8],
    ) -> Response {
        let peer = request
            .extensions()
            .get::<ConnectInfo<crate::server::routes::PeerAddress>>()
            .map(|p| p.0.0.ip());
        let authenticated = request
            .extensions()
            .get::<Option<Arc<crate::server::broker::BrokerToken>>>()
            .and_then(Option::as_ref)
            .is_some_and(|b| b.authorize(request.headers(), peer) == Ok(true));
        if !authenticated {
            return response(
                StatusCode::UNAUTHORIZED,
                json!({"error":"native broker credential and loopback peer required"}),
            );
        }
        if request.method() == Method::GET && path == "cli/catalog/sources" {
            return match self.source_view() {
                Ok(value) => response(StatusCode::OK, value),
                Err(error) => response(StatusCode::BAD_REQUEST, json!({"error":error})),
            };
        }
        if request.method() == Method::GET
            && let Some(job) = path.strip_prefix("cli/catalog/jobs/")
        {
            return match self.catalog_job(job) {
                Ok(value) => response(StatusCode::OK, value),
                Err(error) => response(StatusCode::BAD_REQUEST, json!({"error":error})),
            };
        }
        if request.method() == Method::POST
            && let Some(target) = path.strip_prefix("cli/catalog/")
            && let Some((id, action)) = target.split_once('/')
            && matches!(action, "install" | "update" | "uninstall")
        {
            return match self.begin_catalog(id, action, body) {
                Ok(value) => response(StatusCode::ACCEPTED, value),
                Err(error) => response(StatusCode::BAD_REQUEST, json!({"error":error})),
            };
        }
        if path == "cli" && request.method() == Method::GET {
            let entries = self.entries.lock().values().cloned().collect::<Vec<_>>();
            let tools = entries.iter().map(|e| {
                let m = e.manifest.as_ref();
                json!({"id":e.id,"name":m.map_or(e.id.as_str(), |m| &m.name),"version":m.map_or("", |m| &m.version),"status":self.status(e).0,"cli":m.and_then(|m| m.cli.as_ref()).map(|c| json!({"usage":format!("vosux {}",e.id),"summary":c.summary})),"ui":m.is_some_and(|m| m.ui.is_some()),"trusted":self.trusted(e),"enabled":self.enabled(e)})
            }).collect::<Vec<_>>();
            return response(StatusCode::OK, json!({"tools":tools}));
        }
        if let Some(session) = path.strip_prefix("cli/session/")
            && request.method() == Method::DELETE
            && !session.contains('/')
        {
            self.cli_sessions.lock().remove(session);
            return response(StatusCode::OK, json!({"result":"success"}));
        }
        if request.method() != Method::POST {
            return response(StatusCode::NOT_FOUND, json!({"error":"not found"}));
        }
        let _gate = self.gate.lock().await;
        if self.stopping.load(Ordering::Acquire) {
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":"vosu is stopping"}),
            );
        }
        let target = path.strip_prefix("cli/").unwrap_or_default();
        let (id, open) = target
            .strip_suffix("/open")
            .map_or((target, false), |id| (id, true));
        let Ok(entry) = self.entry(id) else {
            return response(StatusCode::NOT_FOUND, json!({"error":"unknown tool"}));
        };
        let status = self.status(&entry).0;
        if matches!(
            status.as_str(),
            "untrusted" | "disabled" | "invalid" | "blocked"
        ) {
            return response(
                StatusCode::FORBIDDEN,
                json!({"error":"tool is not runnable","status":status}),
            );
        }
        if open {
            // Reuse the dashboard launch path so codes have identical lifetime,
            // single-use behavior, theme validation, and session authority.
            let launch = Request::builder()
                .method(Method::POST)
                .uri(format!("/api/tools/{id}/launch"))
                .body(axum::body::Body::empty())
                .unwrap();
            return self
                .dashboard(launch, axum::body::Bytes::copy_from_slice(body))
                .await;
        }
        let Some(cli) = entry.manifest.as_ref().and_then(|m| m.cli.as_ref()) else {
            return response(StatusCode::CONFLICT, json!({"error":"tool has no CLI"}));
        };
        let prepare = || -> io::Result<Value> {
            fs::create_dir_all(&entry.data_dir)?;
            fs::create_dir_all(&entry.cache_dir)?;
            let mut argv = cli.command.clone();
            let executable = process::executable(&entry.root, &argv[0], entry.source == "dev");
            // Bare executable names follow the same PATH resolution as services,
            // but the launcher receives an absolute path, independent of its cwd.
            let executable = if executable.is_absolute() {
                executable
            } else {
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                    .map(|p| {
                        if p.is_absolute() {
                            p
                        } else {
                            entry.root.join(p)
                        }
                        .join(&executable)
                    })
                    .find(|p| runnable(p))
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, "CLI executable not found on PATH")
                    })?
            };
            argv[0] = executable.to_string_lossy().into_owned();
            let token = secret()?;
            let session = secret()?;
            let mut sessions = self.cli_sessions.lock();
            sessions.retain(|_, s| s.expires > Instant::now());
            if sessions.len() >= 64 {
                return Err(io::Error::other("too many live CLI sessions"));
            }
            sessions.insert(
                session.clone(),
                CliSession {
                    tool: id.into(),
                    token: token.clone(),
                    expires: Instant::now() + Duration::from_secs(24 * 60 * 60),
                },
            );
            Ok(
                json!({"session":session,"argv":argv,"cwd":entry.root,"env":{"VOSU_TOOL_PROTOCOL":"1","VOSU_TOOL_ID":id,"VOSU_TOOL_TOKEN":token,"VOSU_URL":self.overlay_url(),"VOSU_TOOL_DATA_DIR":entry.data_dir,"VOSU_TOOL_CACHE_DIR":entry.cache_dir,"VOSU_TOOL_INTERFACE":"cli"}}),
            )
        };
        match prepare() {
            Ok(value) => response(StatusCode::OK, value),
            Err(e) => response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":e.to_string()}),
            ),
        }
    }
}

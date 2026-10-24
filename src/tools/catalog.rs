use super::*;
use crate::net::{Deadlines, DownloadLimits, StreamingClient, ZipLimits};
use sha2::{Digest, Sha256};
use std::io::Read;

const OFFICIAL_SOURCE: &str = "18-lab-dev/vosu-tools";
const VOSULAY_SOURCE: &str = "18-lab-dev/vosulay";

/// Historical default identities are migration inputs only, never trust aliases.
pub(super) fn migrated_source(source: &str) -> Option<&'static str> {
    match source {
        "shiruviaa/vosu-tools" => Some(OFFICIAL_SOURCE),
        "shiruviaa/vosulay" => Some(VOSULAY_SOURCE),
        _ => None,
    }
}

fn official_source(source: &str) -> bool {
    matches!(source, OFFICIAL_SOURCE | VOSULAY_SOURCE)
}
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;

pub(super) fn record_source(state: &mut Saved, source: &str) {
    if state.catalog_source.as_deref() != Some(source) {
        state.trusted = false;
    }
    state.official = official_source(source);
    state.trusted |= state.official;
    state.catalog_source = Some(source.into());
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(super) struct Catalog {
    pub catalog_version: u32,
    pub published: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tools: Vec<Tool>,
    #[serde(default)]
    pub overlays: Vec<Overlay>,
    #[serde(skip)]
    pub sources: Vec<Value>,
    #[serde(skip)]
    pub shadowed: Vec<Value>,
}
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Tool {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub homepage: Option<String>,
    /// HTTPS art shown on Available cards before installation.
    pub icon_url: Option<String>,
    pub banner_url: Option<String>,
    pub protocol: u32,
    pub interfaces: Vec<String>,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    pub packages: BTreeMap<String, Package>,
    #[serde(skip_deserializing, default)]
    pub source: String,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct Requirement {
    pub id: String,
    pub version: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Package {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Overlay {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub homepage: Option<String>,
    pub icon_url: Option<String>,
    pub banner_url: Option<String>,
    pub resolution: Option<[u32; 2]>,
    pub preview: Option<String>,
    #[serde(default)]
    pub requires_tools: Vec<Requirement>,
    pub package: Package,
    #[serde(skip_deserializing, default)]
    pub source: String,
}
#[derive(Clone, Serialize)]
pub(super) struct Job {
    pub id: String,
    pub status: String,
    pub phase: String,
    pub tool: String,
    pub error: Option<String>,
}
fn validate_url(value: &str) -> Result<(), String> {
    let url = url::Url::parse(value).map_err(|e| e.to_string())?;
    #[cfg(feature = "test-harness")]
    if url.scheme() == "http"
        && url.host_str() == Some("tools.invalid")
        && url.username().is_empty()
        && url.password().is_none()
        && harness_socket().is_some_and(|socket| url.port() == Some(socket.port()))
    {
        return Ok(());
    }
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("catalog URLs must use HTTPS without credentials".into());
    }
    Ok(())
}
impl Catalog {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.catalog_version != 1 {
            return Err("unsupported catalog version".into());
        }
        let mut ids = HashSet::new();
        for tool in &self.tools {
            if !manifest::valid_id(&tool.id) || !ids.insert(&tool.id) {
                return Err("invalid or duplicate catalog id".into());
            }
            semver::Version::parse(&tool.version).map_err(|e| e.to_string())?;
            if tool.protocol != 1
                || tool.interfaces.is_empty()
                || tool
                    .interfaces
                    .iter()
                    .any(|v| !matches!(v.as_str(), "ui" | "service" | "cli"))
            {
                return Err(format!(
                    "unsupported interfaces or protocol for {}",
                    tool.id
                ));
            }
            let mut dependencies = HashSet::new();
            for requirement in &tool.requires {
                if !manifest::valid_id(&requirement.id)
                    || !dependencies.insert(&requirement.id)
                    || requirement.id == tool.id
                {
                    return Err("invalid catalog requirement".into());
                }
                semver::VersionReq::parse(&requirement.version).map_err(|e| e.to_string())?;
            }
            for url in tool.icon_url.iter().chain(&tool.banner_url) {
                validate_url(url)?;
            }
            for package in tool.packages.values() {
                validate_url(&package.url)?;
                if package.sha256.len() != 64
                    || !package.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                    || package.size == 0
                    || package.size > MAX_PACKAGE
                {
                    return Err("invalid catalog package digest or size".into());
                }
            }
        }
        Ok(())
    }
    fn order(&self, id: &str) -> Result<Vec<Tool>, String> {
        fn visit(
            catalog: &Catalog,
            id: &str,
            stack: &mut HashSet<String>,
            done: &mut HashSet<String>,
            order: &mut Vec<Tool>,
        ) -> Result<(), String> {
            if done.contains(id) {
                return Ok(());
            }
            if !stack.insert(id.into()) {
                return Err("catalog dependency cycle".into());
            }
            let tool = catalog
                .tools
                .iter()
                .find(|t| t.id == id)
                .ok_or_else(|| format!("catalog tool not found: {id}"))?;
            for dep in &tool.requires {
                let dependency = catalog
                    .tools
                    .iter()
                    .find(|t| t.id == dep.id)
                    .ok_or_else(|| format!("missing catalog dependency: {}", dep.id))?;
                if !semver::VersionReq::parse(&dep.version)
                    .map_err(|e| e.to_string())?
                    .matches(
                        &semver::Version::parse(&dependency.version).map_err(|e| e.to_string())?,
                    )
                {
                    return Err(format!("unsatisfied catalog dependency: {}", dep.id));
                }
                visit(catalog, &dep.id, stack, done, order)?;
            }
            stack.remove(id);
            done.insert(id.into());
            order.push(tool.clone());
            Ok(())
        }
        let mut order = Vec::new();
        visit(
            self,
            id,
            &mut HashSet::new(),
            &mut HashSet::new(),
            &mut order,
        )?;
        Ok(order)
    }
}
fn client(max_bytes: u64) -> StreamingClient {
    #[cfg(feature = "test-harness")]
    if let Some(socket) = harness_socket() {
        struct Resolver(std::net::IpAddr);
        impl crate::net::AddressResolver for Resolver {
            fn resolve<'a>(&'a self, _: &'a str) -> crate::net::ResolverFuture<'a> {
                Box::pin(async move { Ok(vec![self.0]) })
            }
        }
        let network = crate::net::TestNetworkConfig::new(
            Arc::new(Resolver(socket.ip())),
            vec!["tools.invalid".into()],
            vec![socket],
        )
        .expect("validated harness listener");
        return StreamingClient::for_test_network(
            network,
            Deadlines::default(),
            DownloadLimits {
                max_bytes,
                max_redirects: 5,
            },
        );
    }
    StreamingClient::production().with_deadlines_and_limits(
        Deadlines::default(),
        DownloadLimits {
            max_bytes,
            max_redirects: 5,
        },
    )
}
#[cfg(feature = "test-harness")]
fn harness_socket() -> Option<std::net::SocketAddr> {
    std::env::var("VOSU_TOOL_CATALOG_TEST_SOCKET")
        .ok()?
        .parse::<std::net::SocketAddr>()
        .ok()
        .filter(|socket| socket.ip().is_loopback() && socket.port() != 0)
}
fn target() -> &'static str {
    if cfg!(target_os = "windows") {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}
fn verify_package(path: &Path, package: &Package) -> Result<(), String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != package.size {
        return Err("package size mismatch".into());
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let expected = package.sha256.as_bytes();
    if expected.len() != 64
        || !digest
            .finalize()
            .iter()
            .zip(expected.chunks_exact(2))
            .all(|(byte, pair)| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|text| u8::from_str_radix(text, 16).ok())
                    == Some(*byte)
            })
    {
        return Err("package SHA-256 mismatch".into());
    }
    Ok(())
}
fn extract(
    path: &Path,
    destination: &Path,
    tool: &Tool,
    cancel: &CancellationToken,
) -> Result<(), String> {
    crate::net::streaming::extract_zip_atomic_validated(
        path,
        destination,
        ZipLimits {
            max_entries: 10_000,
            max_expanded_bytes: MAX_PACKAGE,
        },
        cancel,
        |root| {
            let text = fs::read_to_string(root.join("tool.toml"))?;
            let manifest = Manifest::parse(&text).map_err(crate::net::NetworkError::UnsafeZip)?;
            if manifest.id != tool.id
                || manifest.version != tool.version
                || !manifest
                    .requires
                    .iter()
                    .map(|r| (&r.id, &r.version))
                    .eq(tool.requires.iter().map(|r| (&r.id, &r.version)))
            {
                return Err(crate::net::NetworkError::UnsafeZip(
                    "package manifest differs from catalog".into(),
                ));
            }
            #[cfg(unix)]
            if root.join("bin").is_dir() {
                use std::os::unix::fs::PermissionsExt;
                for entry in fs::read_dir(root.join("bin"))? {
                    let entry = entry?;
                    if entry.file_type()?.is_file() {
                        fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755))?;
                    }
                }
            }
            Ok(())
        },
    )
    .map_err(|e| e.to_string())
}
impl ToolHost {
    pub(super) async fn catalog_view(&self) -> Result<Value, String> {
        let catalog = self.fetch_sources().await?;
        *self.catalog.lock() = catalog.clone();
        let tools = catalog
            .tools
            .iter()
            .map(|tool| {
                let installed = self.entry(&tool.id).ok();
                let version = installed
                    .as_ref()
                    .and_then(|e| e.manifest.as_ref())
                    .map(|m| m.version.clone());
                let mut value = serde_json::to_value(tool).unwrap();
                value["installed"] = json!(installed.is_some());
                value["managed"] = json!(
                    installed
                        .as_ref()
                        .is_some_and(|entry| entry.source != "dev")
                        && self
                            .saved
                            .lock()
                            .get(&tool.id)
                            .is_some_and(
                                |state| state.catalog_source.as_deref() == Some(&tool.source)
                            )
                );
                value["installed_version"] = json!(version);
                value["update_available"] =
                    json!(self.update_available(&tool.id, version.as_deref()));
                value["supported"] = json!(
                    tool.packages.contains_key(target()) || tool.packages.contains_key("any")
                );
                value
            })
            .collect::<Vec<_>>();
        let overlays = self.overlay_views(&catalog.overlays)?;
        Ok(
            json!({"catalog_version":1,"published":catalog.published,"tools":tools,
            "overlays":overlays,"sources":catalog.sources,"shadowed":catalog.shadowed}),
        )
    }
    pub(super) fn update_available(&self, id: &str, version: Option<&str>) -> bool {
        let Some(version) = version.and_then(|v| semver::Version::parse(v).ok()) else {
            return false;
        };
        self.catalog
            .lock()
            .tools
            .iter()
            .find(|t| t.id == id)
            .and_then(|t| semver::Version::parse(&t.version).ok())
            .is_some_and(|latest| latest > version)
    }
    pub(super) fn catalog_job(&self, id: &str) -> Result<Value, String> {
        self.catalog_jobs
            .lock()
            .get(id)
            .map(|job| json!(job))
            .ok_or_else(|| "unknown catalog job".into())
    }
    pub(super) fn begin_catalog(
        self: &Arc<Self>,
        id: &str,
        action: &str,
        body: &[u8],
    ) -> Result<Value, String> {
        if !manifest::valid_id(id) {
            return Err("invalid tool id".into());
        }
        let remove_data = if action == "uninstall" {
            serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|v| v.get("remove_data").and_then(Value::as_bool))
                .ok_or("uninstall requires explicit remove_data: true or false")?
        } else {
            false
        };
        if !matches!(
            action,
            "install" | "update" | "uninstall" | "overlay-install" | "overlay-update"
        ) {
            return Err("unknown catalog action".into());
        }
        let install_tools = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| value.get("install_tools").and_then(Value::as_bool))
            .unwrap_or(false);
        let job_id = secret().map_err(|e| e.to_string())?;
        let mut jobs = self.catalog_jobs.lock();
        if jobs.values().any(|j| j.status == "running") {
            return Err("another catalog operation is running".into());
        }
        if jobs.len() >= 64 {
            jobs.clear();
        }
        jobs.insert(
            job_id.clone(),
            Job {
                id: job_id.clone(),
                status: "running".into(),
                phase: "queued".into(),
                tool: id.into(),
                error: None,
            },
        );
        drop(jobs);
        let host = self.clone();
        let id = id.to_owned();
        let action = action.to_owned();
        let job = job_id.clone();
        tokio::spawn(async move {
            let result = if action == "uninstall" {
                host.uninstall_catalog(&id, remove_data).await
            } else if action.starts_with("overlay-") {
                host.install_overlay(&id, &action, install_tools, &job)
                    .await
            } else {
                host.install_catalog(&id, &action, &job).await
            };
            if let Some(state) = host.catalog_jobs.lock().get_mut(&job) {
                state.status = if result.is_ok() {
                    "completed"
                } else {
                    "failed"
                }
                .into();
                state.phase = "finished".into();
                state.error = result.err();
            }
        });
        Ok(json!({"job_id":job_id}))
    }
    fn phase(&self, job: &str, phase: &str) {
        if let Some(state) = self.catalog_jobs.lock().get_mut(job) {
            state.phase = phase.into();
        }
    }
    async fn install_catalog(
        self: &Arc<Self>,
        id: &str,
        action: &str,
        job: &str,
    ) -> Result<(), String> {
        let _operation = self.catalog_operation.lock().await;
        self.phase(job, "catalog");
        self.catalog_view().await?;
        let installed = self.entry(id).ok();
        if installed.as_ref().is_some_and(|entry| {
            entry.source == "dev"
                || self
                    .saved
                    .lock()
                    .get(id)
                    .is_none_or(|state| state.catalog_source.is_none())
        }) {
            return Err("cannot replace a development or manually installed tool".into());
        }
        if action == "install" && installed.is_some() {
            return Err("tool is already installed; use update".into());
        }
        if action == "update" && installed.is_none() {
            return Err("tool is not installed; use install".into());
        }
        let order = self.catalog.lock().order(id)?;
        for tool in order {
            if let Ok(entry) = self.entry(&tool.id) {
                if entry.source == "dev"
                    || self
                        .saved
                        .lock()
                        .get(&tool.id)
                        .is_none_or(|state| state.catalog_source.as_deref() != Some(&tool.source))
                {
                    return Err(format!(
                        "tool {} is installed from a different source",
                        tool.id
                    ));
                }
                if entry
                    .manifest
                    .as_ref()
                    .is_some_and(|m| m.version == tool.version)
                {
                    continue;
                }
            }
            let package = tool
                .packages
                .get(target())
                .or_else(|| tool.packages.get("any"))
                .ok_or_else(|| format!("{} is not available for this platform", tool.id))?
                .clone();
            self.phase(job, &format!("downloading {}", tool.id));
            fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
            let download = tempfile::tempdir_in(&self.cache).map_err(|e| e.to_string())?;
            let archive = download.path().join("package.zip");
            client(package.size)
                .download_atomic(&package.url, "vosu-tool-catalog", &archive, &self.cancel)
                .await
                .map_err(|e| e.to_string())?;
            verify_package(&archive, &package)?;
            self.phase(job, &format!("installing {}", tool.id));
            let _gate = self.gate.lock().await;
            let prior = self.entry(&tool.id).ok();
            let running = prior
                .as_ref()
                .is_some_and(|e| e.state.lock().task.is_some());
            if let Some(entry) = &prior {
                self.stop(entry).await;
            }
            let destination = self.data.join(&tool.id);
            let cancel = self.cancel.clone();
            let tool_copy = tool.clone();
            let result = tokio::task::spawn_blocking(move || {
                extract(&archive, &destination, &tool_copy, &cancel)
            })
            .await
            .map_err(|e| e.to_string())?;
            if let Err(error) = result {
                drop(_gate);
                if running {
                    let _ = self.start(&tool.id, true).await;
                }
                return Err(error);
            }
            {
                let mut saved = self.saved.lock();
                let state = saved.entry(tool.id.clone()).or_default();
                record_source(state, &tool.source);
                atomic_json(&self.config.join("state.json"), &*saved).map_err(|e| e.to_string())?;
            }
            drop(_gate);
            self.rescan().await;
            if running {
                self.start(&tool.id, true).await?;
            }
        }
        Ok(())
    }
    async fn uninstall_catalog(
        self: &Arc<Self>,
        id: &str,
        remove_data: bool,
    ) -> Result<(), String> {
        let _operation = self.catalog_operation.lock().await;
        let _gate = self.gate.lock().await;
        let entry = self.entry(id)?;
        if entry.source == "dev"
            || !self
                .saved
                .lock()
                .get(id)
                .is_some_and(|state| state.official || state.catalog_source.is_some())
        {
            return Err("only catalog-installed tools can be uninstalled".into());
        }
        if self.entries.lock().values().any(|e| {
            e.id != id
                && e.manifest
                    .as_ref()
                    .is_some_and(|m| m.requires.iter().any(|r| r.id == id))
        }) {
            return Err("another installed tool requires this tool".into());
        }
        self.stop(&entry).await;
        fs::remove_dir_all(&entry.root).map_err(|e| e.to_string())?;
        if remove_data {
            for path in [&entry.data_dir, &entry.cache_dir] {
                if path.exists() {
                    fs::remove_dir_all(path).map_err(|e| e.to_string())?;
                }
            }
        }
        self.saved.lock().remove(id);
        atomic_json(&self.config.join("state.json"), &*self.saved.lock())
            .map_err(|e| e.to_string())?;
        self.cli_sessions.lock().retain(|_, s| s.tool != id);
        self.sessions.lock().retain(|_, owner| owner != id);
        drop(_gate);
        self.rescan().await;
        Ok(())
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

#[path = "catalog_sources.rs"]
mod sources;

//! Core's tool registry (A7.1/A7.2): discovery of web-only `kind = "tool"`
//! packages, per-tool enabled/trust/settings state, legacy detection, launch
//! codes, launch sessions and action authority, and the `ToolHostView` pushed
//! to the bundled `vosu.toolhost`. Nothing here starts, proxies or reaches a
//! tool process: 1.x `tool.toml` packages are listed as `needs-2.0-update`,
//! never parsed, executed or served, and their files stay untouched for M8.
pub mod action_authority;
#[cfg(test)]
mod action_authority_tests;
pub mod capabilities;
mod catalog;
mod lease;
mod manage;
mod manifest;
mod replaces;
mod settings;
#[cfg(test)]
mod tests;
mod view;

use manifest::ToolPackage;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq as _;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub use view::ToolHostViews;

/// Legacy-package diagnosis (`ToolView.status_detail`, A7.2).
const NEEDS_UPDATE: &str = "needs-2.0-update";

#[derive(Clone, Default, Deserialize, Serialize)]
struct Saved {
    enabled: Option<bool>,
    trusted: bool,
    #[serde(default)]
    official: bool,
    #[serde(default)]
    catalog_source: Option<String>,
    /// A 1.x service preference; never read, preserved for M8's migration.
    #[serde(default)]
    autostart: Option<bool>,
    /// Catalog-authorized prior identities, retained for 1.x replacement recovery.
    #[serde(default)]
    replaces: Vec<String>,
}

struct Entry {
    id: String,
    root: PathBuf,
    source: &'static str,
    /// The validated v2 package; `None` for legacy and unreadable packages.
    manifest: Option<ToolPackage>,
    /// A 1.x `tool.toml`-only package (`needs-2.0-update`).
    legacy: bool,
    /// Why discovery refused the package (`status: "invalid"`).
    error: Option<String>,
    settings: Option<settings::Schema>,
    fingerprint: Vec<u8>,
    package: lease::PackageRoot,
}

struct Code {
    tool: String,
    expires: Instant,
}

pub struct ToolRegistry {
    entries: Mutex<BTreeMap<String, Arc<Entry>>>,
    saved: Mutex<BTreeMap<String, Saved>>,
    codes: Mutex<HashMap<String, Code>>,
    sessions: Mutex<HashMap<String, action_authority::LaunchSession>>,
    action_providers: Mutex<action_authority::Providers>,
    jobs: Mutex<Option<crate::actions::JobRegistry>>,
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    roots: Vec<(PathBuf, &'static str)>,
    overlay_url: String,
    dashboard_url: Option<String>,
    origins: Mutex<Option<Arc<crate::server::origins::Origins>>>,
    view: view::Publisher,
    /// Serializes discovery and every `state.json`/settings write.
    gate: tokio::sync::Mutex<()>,
    stopping: AtomicBool,
    cancel: CancellationToken,
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// `ToolView.update_available` for a catalog-installed tool (M7 D1);
    /// unset (no update) until the catalog composition attaches it.
    updates: std::sync::OnceLock<UpdateSource>,
}

/// Whether a newer installable version of the installed tool `id` is
/// listed by its own catalog source.
pub type UpdateSource = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub fn secret() -> io::Result<String> {
    use std::fmt::Write as _;
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    let mut result = String::with_capacity(64);
    for byte in bytes {
        write!(result, "{byte:02x}").unwrap();
    }
    Ok(result)
}
fn equal(a: &str, b: &str) -> bool {
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}
/// Publishes via tempfile `persist` (`MoveFileExW` on Windows), not the
/// cross-directory `vosu_sys::platform::atomic_replace`: readers of files
/// this writes (tool settings, `state.json`, catalog sources,
/// `overlay-catalog-state.json`) are out of scope for
/// `vosu_sys::platform::read_published`'s measured guarantee; see its doc
/// comment's exclusion list.
fn atomic_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    fs::create_dir_all(path.parent().unwrap())?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
/// `--tool <dir>`: development packages, each loading only its `manifest.toml`.
pub fn development_roots(args: &[std::ffi::OsString]) -> io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--tool" {
            result
                .push(PathBuf::from(iter.next().ok_or_else(|| {
                    io::Error::other("--tool requires a directory")
                })?));
        } else if let Some(value) = arg.to_str().and_then(|v| v.strip_prefix("--tool=")) {
            if value.is_empty() {
                return Err(io::Error::other("--tool requires a directory"));
            }
            result.push(value.into());
        }
    }
    result.into_iter().map(std::path::absolute).collect()
}
/// Tool roots ahead of the user root, in precedence order.
pub struct ExtraRoots {
    pub dev: Vec<PathBuf>,
}

/// What discovery found in one candidate directory.
struct Found {
    id: String,
    manifest: Option<ToolPackage>,
    legacy: bool,
    error: Option<String>,
    settings: Option<settings::Schema>,
    fingerprint: Vec<u8>,
}

fn inspect(path: &Path) -> Found {
    let fallback_id = || {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    let text = match fs::read_to_string(path.join("manifest.toml")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // Legacy detection never parses `tool.toml`.
            let legacy = path.join("tool.toml").is_file();
            return Found {
                id: fallback_id(),
                manifest: None,
                legacy,
                error: (!legacy).then(|| "manifest.toml: not found".into()),
                settings: None,
                fingerprint: if legacy {
                    b"legacy".to_vec()
                } else {
                    Vec::new()
                },
            };
        }
        Err(error) => {
            return Found {
                id: fallback_id(),
                manifest: None,
                legacy: false,
                error: Some(format!("manifest.toml: {error}")),
                settings: None,
                fingerprint: Vec::new(),
            };
        }
    };
    let parsed = ToolPackage::parse(&text);
    let mut error = parsed.as_ref().err().cloned();
    let schema_bytes = match fs::read(path.join("settings.json")) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            error = Some(format!("settings.json: {e}"));
            None
        }
    };
    let settings = schema_bytes
        .as_deref()
        .and_then(|bytes| match settings::Schema::parse(bytes) {
            Ok(schema) => Some(schema),
            Err(e) => {
                error = Some(format!("settings.json: {e}"));
                None
            }
        });
    let mut fingerprint = text.into_bytes();
    fingerprint.extend_from_slice(schema_bytes.as_deref().unwrap_or_default());
    if let Some(error) = &error {
        fingerprint.extend_from_slice(error.as_bytes());
    }
    Found {
        id: parsed
            .as_ref()
            .map_or_else(|_| fallback_id(), |m| m.tool.shared.id.clone()),
        manifest: parsed.ok(),
        legacy: false,
        error,
        settings,
        fingerprint,
    }
}

impl ToolRegistry {
    pub async fn new(
        config: PathBuf,
        data: PathBuf,
        cache: PathBuf,
        extra: ExtraRoots,
        overlay_url: String,
        dashboard_url: Option<String>,
    ) -> io::Result<Arc<Self>> {
        let ExtraRoots { dev } = extra;
        let user = data.clone();
        fs::create_dir_all(&user)?;
        let mut roots = dev.into_iter().map(|p| (p, "dev")).collect::<Vec<_>>();
        roots.push((user, "user"));
        let mut saved: BTreeMap<String, Saved> = match fs::read(config.join("state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        let mut migrated = false;
        for state in saved.values_mut() {
            if let Some(source) = state
                .catalog_source
                .as_deref()
                .and_then(catalog::migrated_source)
            {
                // Renaming the same catalog must preserve explicit user approval.
                let trusted = state.trusted;
                catalog::record_source(state, source);
                state.trusted |= trusted;
                migrated = true;
            }
        }
        if migrated {
            atomic_json(&config.join("state.json"), &saved)?;
        }
        let registry = Arc::new(Self {
            entries: Mutex::new(BTreeMap::new()),
            saved: Mutex::new(saved),
            codes: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            action_providers: Mutex::new(action_authority::Providers::default()),
            jobs: Mutex::new(None),
            config,
            data,
            cache,
            roots,
            view: view::Publisher::new(dashboard_url.clone()),
            overlay_url,
            dashboard_url,
            origins: Mutex::new(None),
            gate: tokio::sync::Mutex::new(()),
            stopping: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            watcher: Mutex::new(None),
            updates: std::sync::OnceLock::new(),
        });
        registry.recover_replacement().map_err(io::Error::other)?;
        registry.rescan().await;
        Ok(registry)
    }
    /// Attaches core's catalog update source once (M7 D1); a second call
    /// keeps the first source.
    pub fn set_update_source(&self, source: UpdateSource) {
        let _ = self.updates.set(source);
    }
    fn update_available(&self, id: &str) -> bool {
        self.updates.get().is_some_and(|source| source(id))
    }
    pub fn activate(self: &Arc<Self>, origins: Option<Arc<crate::server::origins::Origins>>) {
        *self.origins.lock() = origins;
        self.publish();
        let mut watcher = self.watcher.lock();
        if watcher.is_none() {
            let owner = self.clone();
            *watcher = Some(tokio::spawn(async move {
                owner.watch().await;
            }));
        }
    }
    fn overlay_url(&self) -> String {
        self.origins
            .lock()
            .as_ref()
            .map_or_else(|| self.overlay_url.clone(), |origins| origins.overlay())
    }
    /// The bridge's `dashboardUrl`: `None` while the dashboard is off (A7.3),
    /// since nothing serves its origin.
    fn dashboard_url(&self) -> Option<String> {
        match self.origins.lock().as_ref() {
            Some(origins) => origins.dashboard_enabled().then(|| origins.dashboard()),
            None => self.dashboard_url.clone(),
        }
    }
    fn enabled(&self, entry: &Entry) -> bool {
        !entry.legacy
            && self
                .saved
                .lock()
                .get(&entry.id)
                .and_then(|s| s.enabled)
                .unwrap_or(true)
    }
    fn trusted(&self, entry: &Entry) -> bool {
        entry.source != "user" || self.saved.lock().get(&entry.id).is_some_and(|s| s.trusted)
    }
    fn entry(&self, id: &str) -> Result<Arc<Entry>, String> {
        self.entries
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| "unknown tool".into())
    }
    fn tool_settings(&self, id: &str) -> PathBuf {
        self.config.join("settings").join(format!("{id}.json"))
    }
    /// Effective values (defaults plus stored); empty without a schema.
    fn settings(
        &self,
        entry: &Entry,
    ) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        let stored = match fs::read(self.tool_settings(&entry.id)) {
            Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(|e| e.to_string())?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        Ok(entry
            .settings
            .as_ref()
            .map(|schema| schema.effective(stored))
            .unwrap_or_default())
    }
    /// `(status, status_detail)` of the 2.0 `ToolView`.
    fn status(&self, entry: &Entry) -> (&'static str, Option<String>) {
        if entry.legacy {
            return ("disabled", Some(NEEDS_UPDATE.into()));
        }
        if let Some(error) = &entry.error {
            return ("invalid", Some(error.clone()));
        }
        if !self.trusted(entry) {
            return ("untrusted", None);
        }
        if !self.enabled(entry) {
            return ("disabled", None);
        }
        if let Some(reason) = self.blocked_capabilities(entry) {
            return ("blocked", Some(reason));
        }
        ("running", None)
    }
    /// Launchable and servable: enabled, trusted, non-legacy, resolvable.
    fn servable(&self, entry: &Entry) -> bool {
        entry.manifest.is_some() && self.status(entry).0 == "running"
    }

    /// Ends every launch session and pending code matching `revoked`, and
    /// requests advisory cancellation of the jobs those sessions created.
    fn invalidate(&self, revoked: impl Fn(&str, &action_authority::LaunchSession) -> bool) {
        let owners: Vec<_> = {
            let mut sessions = self.sessions.lock();
            let mut owners = Vec::new();
            sessions.retain(|_, session| {
                let keep = !revoked(&session.tool, session);
                if !keep {
                    owners.push(session.owner());
                }
                keep
            });
            owners
        };
        let jobs = self.jobs.lock().clone();
        if let Some(jobs) = jobs {
            for owner in &owners {
                jobs.cancel_owner(owner);
            }
        }
    }
    fn invalidate_tool(&self, id: &str) {
        self.codes.lock().retain(|_, code| code.tool != id);
        self.invalidate(|tool, _| tool == id);
    }

    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.cancel.cancel();
        let watcher = self.watcher.lock().take();
        if let Some(task) = watcher {
            let _ = task.await;
        }
        self.codes.lock().clear();
        self.invalidate(|_, _| true);
    }
    /// Catalog v2 installation's web-package handoff (M6b I0/K0): v2 tools
    /// commit into this registry's user root and are presented after this
    /// rescan.
    pub async fn rescan_installed(self: &Arc<Self>) {
        self.rescan().await;
    }
    /// U0 removal (M6b H0): end the tool's launch sessions and codes before
    /// its package directory moves.
    pub async fn revoke_installed(&self, id: &str) {
        self.invalidate_tool(id);
        if let Some(entry) = self.entries.lock().get(id) {
            entry.package.revoke();
        }
        self.publish();
    }
    /// U0 removal of the explicit target's durable data (user choice only).
    pub async fn remove_installed_data(&self, id: &str) -> io::Result<()> {
        if !crate::server::ids::valid_id(id) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let paths = [self.tool_data(id), self.cache.join(id)];
        tokio::task::spawn_blocking(move || {
            for path in paths {
                match fs::remove_dir_all(&path) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }
    async fn rescan(self: &Arc<Self>) {
        let _gate = self.gate.lock().await;
        if let Err(error) = self.recover_replacement() {
            tracing::error!(%error, "tool discovery paused until replacement recovery succeeds");
            return;
        }
        let mut found = BTreeMap::<String, Arc<Entry>>::new();
        for (root, source) in &self.roots {
            let paths = if *source == "dev" {
                vec![root.clone()]
            } else {
                fs::read_dir(root)
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    // Installer transaction directories (`.stage-*`, catalog
                    // `.vosu-backup-*`, legacy `.extract-*`) are never tools;
                    // a hidden name is never a valid tool id either.
                    .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                    .map(|e| e.path())
                    .collect()
            };
            for path in paths {
                let mut candidate = inspect(&path);
                // A user-root directory with neither manifest is not a tool.
                if *source != "dev"
                    && candidate.manifest.is_none()
                    && !candidate.legacy
                    && candidate.error.as_deref() == Some("manifest.toml: not found")
                {
                    continue;
                }
                let id = candidate.id.clone();
                let source = if *source == "user"
                    && self
                        .saved
                        .lock()
                        .get(&id)
                        .is_some_and(|state| state.official)
                {
                    &"official"
                } else {
                    source
                };
                if *source != "dev" && path.file_name().and_then(|v| v.to_str()) != Some(&id) {
                    candidate.error = Some("directory name must equal manifest id".into());
                }
                let mut key = id.clone();
                if let Some(prior) = found.get(&id) {
                    if prior.source == "dev" && *source != "dev" {
                        continue;
                    }
                    if prior.source == "official" && *source == "user" {
                        candidate.error = Some(format!("user tool duplicates official id {id}"));
                        key = format!("{id}:duplicate-user");
                    } else {
                        candidate.error = Some(format!("duplicate tool id {id}"));
                        key = format!("{id}:duplicate-{}", found.len());
                    }
                }
                if let Some(error) = &candidate.error {
                    candidate.fingerprint.extend_from_slice(error.as_bytes());
                }
                let old = self.entries.lock().get(&key).cloned();
                if let Some(old) = old
                    && old.root == path
                    && old.source == *source
                    && old.fingerprint == candidate.fingerprint
                    // A revoked (uninstalling) package is rediscovered fresh.
                    && old.package.is_live()
                {
                    found.insert(key, old);
                    continue;
                }
                found.insert(
                    key,
                    Arc::new(Entry {
                        id,
                        package: lease::PackageRoot::new(path.clone()),
                        root: path,
                        source,
                        manifest: candidate.manifest,
                        legacy: candidate.legacy,
                        error: candidate.error,
                        settings: candidate.settings,
                        fingerprint: candidate.fingerprint,
                    }),
                );
            }
        }
        let replaced: Vec<Arc<Entry>> = self
            .entries
            .lock()
            .values()
            .filter(|entry| !found.values().any(|e| Arc::ptr_eq(e, entry)))
            .cloned()
            .collect();
        *self.entries.lock() = found;
        for entry in &replaced {
            // A replaced or removed package's sessions, codes and lease die
            // with it; a later launch never inherits them.
            entry.package.revoke();
            self.codes.lock().retain(|_, code| code.tool != entry.id);
            self.invalidate(|_, session| session.is(entry));
        }
        drop(_gate);
        self.publish();
    }
    async fn watch(self: Arc<Self>) {
        use notify::Watcher as _;
        let changed = Arc::new(Notify::new());
        let flag = changed.clone();
        let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            flag.notify_one();
        })
        .ok();
        if let Some(watcher) = &mut watcher {
            for (root, _) in &self.roots {
                let _ = watcher.watch(root, notify::RecursiveMode::Recursive);
            }
        }
        let mut ticks = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => break,
                _ = ticks.tick() => {},
                () = changed.notified() => {
                    tokio::select! {
                        () = self.cancel.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_millis(100)) => {},
                    }
                },
            }
            // The timer also follows provider, trust and settings changes
            // that no file event reports; unchanged views are not republished.
            self.rescan().await;
        }
    }
    fn tool_data(&self, id: &str) -> PathBuf {
        self.data
            .parent()
            .unwrap_or(&self.data)
            .join("tool-data")
            .join(id)
    }
}

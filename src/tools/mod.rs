//! Discovery, trust, credentials and lifetime ownership for out-of-process tools.
pub(crate) mod bundled;
mod http;
pub mod manifest;
mod process;
#[cfg(test)]
mod tests;

use crate::counters::{CounterSetting, SettingInput};
use manifest::Manifest;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq as _;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default, Deserialize, Serialize)]
struct Saved {
    enabled: Option<bool>,
    trusted: bool,
}
#[derive(Clone, Serialize)]
struct LogLine {
    seq: u64,
    ts: String,
    stream: String,
    text: String,
}
struct State {
    status: String,
    detail: Option<String>,
    port: Option<u16>,
    token: Option<String>,
    pid: Option<u32>,
    started_at: Option<String>,
    restarts: u64,
    stop: Option<CancellationToken>,
    task: Option<tokio::task::JoinHandle<()>>,
    last_used: Instant,
    desired: bool,
}
struct Entry {
    id: String,
    manifest: Option<Manifest>,
    root: PathBuf,
    source: &'static str,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    fingerprint: Vec<u8>,
    state: Mutex<State>,
    ready: Notify,
    logs: Mutex<VecDeque<LogLine>>,
    sequence: Mutex<u64>,
    schema: Vec<CounterSetting>,
    active: AtomicUsize,
}
impl Entry {
    fn log(&self, stream: &str, text: &str) {
        let text = self.state.lock().token.as_ref().map_or_else(
            || text.to_owned(),
            |token| text.replace(token, "[redacted]"),
        );
        let mut seq = self.sequence.lock();
        *seq += 1;
        let mut logs = self.logs.lock();
        logs.push_back(LogLine {
            seq: *seq,
            ts: chrono::Utc::now().to_rfc3339(),
            stream: stream.into(),
            text: text.chars().take(16 * 1024).collect(),
        });
        if logs.len() > 1000 {
            logs.pop_front();
        }
    }
}

struct Activity(Arc<Entry>);
impl Activity {
    fn new(entry: Arc<Entry>) -> Self {
        entry.active.fetch_add(1, Ordering::AcqRel);
        Self(entry)
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.state.lock().last_used = Instant::now();
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Code {
    tool: String,
    expires: Instant,
}
pub struct ToolHost {
    entries: Mutex<BTreeMap<String, Arc<Entry>>>,
    saved: Mutex<BTreeMap<String, Saved>>,
    codes: Mutex<HashMap<String, Code>>,
    sessions: Mutex<HashMap<String, String>>,
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    roots: Vec<(PathBuf, &'static str)>,
    overlay_url: String,
    dashboard_url: String,
    origins: Mutex<Option<Arc<crate::server::origins::Origins>>>,
    http: reqwest::Client,
    gate: tokio::sync::Mutex<()>,
    pending: Semaphore,
    stopping: AtomicBool,
    cancel: CancellationToken,
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
    context: Arc<dyn Fn() -> Value + Send + Sync>,
    _bundle: Option<bundled::Bundle>,
}
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
    pub bundled: Option<bundled::Bundle>,
}
impl ToolHost {
    pub async fn new(
        config: PathBuf,
        data: PathBuf,
        cache: PathBuf,
        extra: ExtraRoots,
        overlay_url: String,
        dashboard_url: String,
        context: Arc<dyn Fn() -> Value + Send + Sync>,
    ) -> io::Result<Arc<Self>> {
        let ExtraRoots { dev, bundled } = extra;
        let user = data.clone();
        fs::create_dir_all(&user)?;
        let mut roots = dev.into_iter().map(|p| (p, "dev")).collect::<Vec<_>>();
        if let Some(bundle) = &bundled {
            roots.push((bundle.root.clone(), "bundled"));
        }
        roots.push((user, "user"));
        let saved = match fs::read(config.join("state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        let host = Arc::new(Self {
            entries: Mutex::new(BTreeMap::new()),
            saved: Mutex::new(saved),
            codes: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            config,
            data,
            cache,
            roots,
            overlay_url,
            dashboard_url,
            origins: Mutex::new(None),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(io::Error::other)?,
            gate: tokio::sync::Mutex::new(()),
            pending: Semaphore::new(64),
            stopping: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            watcher: Mutex::new(None),
            context,
            _bundle: bundled,
        });
        host.rescan().await;
        Ok(host)
    }
    pub fn activate(self: &Arc<Self>, origins: Option<Arc<crate::server::origins::Origins>>) {
        *self.origins.lock() = origins;
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
    fn dashboard_url(&self) -> String {
        self.origins
            .lock()
            .as_ref()
            .map_or_else(|| self.dashboard_url.clone(), |origins| origins.dashboard())
    }
    fn enabled(&self, entry: &Entry) -> bool {
        self.saved
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
    fn settings(&self, entry: &Entry) -> Result<serde_json::Map<String, Value>, String> {
        let mut values = serde_json::Map::new();
        for setting in &entry.schema {
            if setting.kind != crate::counters::SettingType::Header {
                values.insert(setting.unique_id.clone(), setting.value.clone());
            }
        }
        let path = self
            .config
            .join("settings")
            .join(format!("{}.json", entry.id));
        match fs::read(path) {
            Ok(bytes) => {
                let stored: serde_json::Map<String, Value> =
                    serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                for (id, value) in stored {
                    if values.contains_key(&id) {
                        values.insert(id, value);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        Ok(values)
    }
    fn blocked(&self, entry: &Entry) -> Option<String> {
        let manifest = entry.manifest.as_ref()?;
        for dep in &manifest.requires {
            let Ok(other) = self.entry(&dep.id) else {
                return Some(format!("missing dependency {}", dep.id));
            };
            let Some(m) = &other.manifest else {
                return Some(format!("invalid dependency {}", dep.id));
            };
            if !self.enabled(&other)
                || !self.trusted(&other)
                || other.state.lock().status == "invalid"
            {
                return Some(format!("unavailable dependency {}", dep.id));
            }
            if !semver::VersionReq::parse(&dep.version)
                .unwrap()
                .matches(&semver::Version::parse(&m.version).unwrap())
            {
                return Some(format!(
                    "dependency {} version {} does not match {}",
                    dep.id, m.version, dep.version
                ));
            }
        }
        None
    }
    fn status(&self, entry: &Entry) -> (String, Option<String>) {
        let state = entry.state.lock();
        if state.status == "invalid" {
            return (state.status.clone(), state.detail.clone());
        }
        drop(state);
        if !self.trusted(entry) {
            return ("untrusted".into(), None);
        }
        if !self.enabled(entry) {
            return ("disabled".into(), None);
        }
        if let Some(reason) = self.blocked(entry) {
            return ("blocked".into(), Some(reason));
        }
        let state = entry.state.lock();
        (state.status.clone(), state.detail.clone())
    }
    fn view(&self, entry: &Entry) -> Value {
        let (status, detail) = self.status(entry);
        let m = entry.manifest.as_ref();
        let requires = m
            .map(|m| {
                m.requires
                    .iter()
                    .map(|dep| {
                        let satisfied = self
                            .entry(&dep.id)
                            .ok()
                            .and_then(|e| {
                                e.manifest.as_ref().map(|m| {
                                    semver::VersionReq::parse(&dep.version)
                                        .unwrap()
                                        .matches(&semver::Version::parse(&m.version).unwrap())
                                })
                            })
                            .unwrap_or(false);
                        json!({"id": dep.id, "version": dep.version, "satisfied": satisfied})
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let required_by = self
            .entries
            .lock()
            .values()
            .filter(|e| {
                e.manifest
                    .as_ref()
                    .is_some_and(|m| m.requires.iter().any(|d| d.id == entry.id))
            })
            .map(|e| e.id.clone())
            .collect::<Vec<_>>();
        let state = entry.state.lock();
        json!({"id":entry.id,"name":m.map_or(entry.id.as_str(), |m| &m.name),"version":m.map_or("", |m| &m.version),"description":m.map_or("", |m| &m.description),"author":m.map_or("", |m| &m.author),"homepage":m.and_then(|m| m.homepage.as_ref()),"icon_url":m.and_then(|m| m.icon.as_ref()).map(|_| format!("{}/tools/{}/_icon",self.overlay_url(),entry.id)),"source":entry.source,"path":entry.root,"trusted":self.trusted(entry),"enabled":self.enabled(entry),"kind":m.map(|m| match (m.ui.is_some(),m.service.is_some()) {(true,true)=>"hybrid",(true,false)=>"ui",_=>"service"}),"startup":m.and_then(|m| m.service.as_ref()).map(|s| &s.startup),"status":status,"status_detail":detail,"ui_url":m.filter(|m| m.ui.is_some()).map(|_|format!("{}/tools/{}/",self.overlay_url(),entry.id)),"requires":requires,"required_by":required_by,"permissions":m.map(|m| &m.permissions),"api":m.map(|m| &m.api),"has_settings":entry.root.join("settings.json").is_file(),"data":{"data_dir":entry.data_dir,"cache_dir":entry.cache_dir,"data_bytes":directory_bytes(&entry.data_dir),"cache_bytes":directory_bytes(&entry.cache_dir)},"process":{"pid":state.pid,"started_at":state.started_at,"restarts":state.restarts}})
    }
    async fn start(self: &Arc<Self>, id: &str, manual: bool) -> Result<(), String> {
        let _gate = self.gate.lock().await;
        if self.stopping.load(Ordering::Acquire) {
            return Err("vosu is shutting down".into());
        }
        let mut order = Vec::new();
        self.visit(id, &mut HashSet::new(), &mut HashSet::new(), &mut order)?;
        for id in order {
            let entry = self.entry(&id)?;
            let (status, detail) = self.status(&entry);
            if matches!(
                status.as_str(),
                "invalid" | "blocked" | "disabled" | "untrusted"
            ) {
                return Err(detail.unwrap_or(status));
            }
            if entry.manifest.as_ref().unwrap().service.is_none() {
                continue;
            }
            if status == "failed" && !manual {
                return Err("tool failed; start it from the dashboard".into());
            }
            let stale = {
                let mut state = entry.state.lock();
                if state.task.as_ref().is_some_and(|t| t.is_finished()) {
                    state.task.take()
                } else {
                    None
                }
            };
            if let Some(task) = stale {
                let _ = task.await;
            }
            {
                let mut state = entry.state.lock();
                state.last_used = Instant::now();
                state.desired = true;
                if state.task.is_none() {
                    let stop = CancellationToken::new();
                    state.stop = Some(stop.clone());
                    state.status = "starting".into();
                    state.detail = None;
                    state.task = Some(tokio::spawn(process::supervise(
                        self.clone(),
                        entry.clone(),
                        stop,
                    )));
                }
            }
            tokio::time::timeout(Duration::from_secs(24), async {
                loop {
                    let notified = entry.ready.notified();
                    {
                        let state = entry.state.lock();
                        if state.port.is_some() {
                            return Ok(());
                        }
                        if state.status == "failed" || state.status == "stopped" {
                            return Err(state
                                .detail
                                .clone()
                                .unwrap_or_else(|| "service stopped".into()));
                        }
                    }
                    notified.await;
                }
            })
            .await
            .map_err(|_| "service readiness timed out".to_owned())??;
        }
        Ok(())
    }
    fn visit(
        &self,
        id: &str,
        active: &mut HashSet<String>,
        done: &mut HashSet<String>,
        order: &mut Vec<String>,
    ) -> Result<(), String> {
        if done.contains(id) {
            return Ok(());
        }
        if !active.insert(id.into()) {
            return Err(format!("dependency cycle containing {id}"));
        }
        let entry = self.entry(id)?;
        let m = entry.manifest.as_ref().ok_or("invalid manifest")?;
        for dep in &m.requires {
            self.visit(&dep.id, active, done, order)?;
        }
        active.remove(id);
        done.insert(id.into());
        order.push(id.into());
        Ok(())
    }
    async fn stop_serialized(&self, entry: &Entry, manual: bool) {
        let _gate = self.gate.lock().await;
        if manual {
            entry.state.lock().desired = false;
        }
        self.stop(entry).await;
    }
    async fn stop(&self, entry: &Entry) {
        let task = {
            let mut state = entry.state.lock();
            if let Some(stop) = state.stop.take() {
                stop.cancel();
            }
            state.task.take()
        };
        if let Some(task) = task {
            let _ = task.await;
        }
        if entry.manifest.as_ref().is_some_and(|m| m.service.is_some()) {
            entry.state.lock().status = "stopped".into();
        }
    }
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        self.cancel.cancel();
        let entries = self.entries.lock().values().cloned().collect::<Vec<_>>();
        for entry in &entries {
            if let Some(stop) = &entry.state.lock().stop {
                stop.cancel();
            }
        }
        let watcher = self.watcher.lock().take();
        if let Some(task) = watcher {
            let _ = task.await;
        }
        let _gate = self.gate.lock().await;
        for entry in entries {
            self.stop(&entry).await;
        }
    }
    async fn rescan(self: &Arc<Self>) {
        let _gate = self.gate.lock().await;
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
                    .map(|e| e.path())
                    .collect()
            };
            for path in paths {
                if *source != "dev" && !path.join("tool.toml").is_file() {
                    continue;
                }
                let text = fs::read_to_string(path.join("tool.toml"));
                let parsed = text
                    .as_ref()
                    .map_err(|e| format!("tool.toml: {e}"))
                    .and_then(|text| Manifest::parse(text));
                let id = parsed.as_ref().map(|m| m.id.clone()).unwrap_or_else(|_| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
                let mut error = parsed.as_ref().err().cloned();
                if *source != "dev" && path.file_name().and_then(|v| v.to_str()) != Some(&id) {
                    error = Some("directory name must equal manifest id".into());
                }
                let mut key = id.clone();
                if let Some(prior) = found.get(&id) {
                    if prior.source == "dev" && *source != "dev" {
                        continue;
                    }
                    if prior.source == "bundled" && *source == "user" {
                        error = Some(format!("user tool duplicates bundled id {id}"));
                        key = format!("{id}:duplicate-user");
                    } else {
                        error = Some(format!("duplicate tool id {id}"));
                        key = format!("{id}:duplicate-{}", found.len());
                    }
                }
                let schema_bytes = match fs::read(path.join("settings.json")) {
                    Ok(bytes) => Some(bytes),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                    Err(e) => {
                        error = Some(format!("settings.json: {e}"));
                        None
                    }
                };
                let mut schema: Vec<CounterSetting> = if let Some(bytes) = &schema_bytes {
                    match serde_json::from_slice(bytes) {
                        Ok(schema) => schema,
                        Err(e) => {
                            error = Some(format!("settings.json: {e}"));
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                crate::counters::normalize_settings(&mut schema);
                let mut fingerprint = text.unwrap_or_default().into_bytes();
                fingerprint.extend_from_slice(schema_bytes.as_deref().unwrap_or_default());
                if let Some(error) = &error {
                    fingerprint.extend_from_slice(error.as_bytes());
                }
                if *source == "dev"
                    && let Ok(m) = &parsed
                    && let Some(service) = &m.service
                {
                    for command in &service.command {
                        let executable = if command == &service.command[0] {
                            process::executable(&path, command, true)
                        } else {
                            path.join(command)
                        };
                        if let Ok(metadata) = fs::metadata(executable) {
                            fingerprint.extend_from_slice(
                                format!("{:?}:{}", metadata.modified(), metadata.len()).as_bytes(),
                            );
                        }
                    }
                }
                let old = self.entries.lock().get(&key).cloned();
                if let Some(old) = &old
                    && old.root == path
                    && old.fingerprint == fingerprint
                {
                    found.insert(key, old.clone());
                    continue;
                }
                let desired = old.as_ref().map_or_else(
                    || {
                        parsed.as_ref().is_ok_and(|m| {
                            m.service.as_ref().is_some_and(|s| s.startup == "always")
                        })
                    },
                    |entry| entry.state.lock().desired,
                );
                if let Some(old) = &old {
                    self.stop(old).await;
                }
                let entry = Arc::new(Entry {
                    id: id.clone(),
                    manifest: parsed.ok(),
                    root: path,
                    source,
                    data_dir: self.data.join("data").join(&id),
                    cache_dir: self.cache.join(&id),
                    fingerprint,
                    state: Mutex::new(State {
                        status: if error.is_some() {
                            "invalid"
                        } else {
                            "stopped"
                        }
                        .into(),
                        detail: error,
                        port: None,
                        token: None,
                        pid: None,
                        started_at: None,
                        restarts: old.as_ref().map_or(0, |entry| entry.state.lock().restarts),
                        stop: None,
                        task: None,
                        last_used: Instant::now(),
                        desired,
                    }),
                    ready: Notify::new(),
                    logs: Mutex::new(
                        old.as_ref()
                            .map_or_else(VecDeque::new, |entry| entry.logs.lock().clone()),
                    ),
                    sequence: Mutex::new(old.as_ref().map_or(0, |entry| *entry.sequence.lock())),
                    schema,
                    active: AtomicUsize::new(0),
                });
                if entry.manifest.as_ref().is_some_and(|m| m.service.is_none())
                    && entry.state.lock().status != "invalid"
                {
                    entry.state.lock().status = "running".into();
                }
                found.insert(key, entry);
            }
        }
        let old = self.entries.lock().values().cloned().collect::<Vec<_>>();
        for entry in old {
            if !found.values().any(|e| Arc::ptr_eq(e, &entry)) {
                self.stop(&entry).await;
            }
        }
        *self.entries.lock() = found;
        let ids = self.entries.lock().keys().cloned().collect::<Vec<_>>();
        for id in ids {
            if let Err(error) = self.visit(
                &id,
                &mut HashSet::new(),
                &mut HashSet::new(),
                &mut Vec::new(),
            ) && error == format!("dependency cycle containing {id}")
                && let Ok(entry) = self.entry(&id)
            {
                let mut state = entry.state.lock();
                state.status = "invalid".into();
                state.detail = Some(error);
            }
        }
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
            // The timer also tracks development binaries outside watched roots.
            self.rescan().await;
            let entries = self.entries.lock().values().cloned().collect::<Vec<_>>();
            for entry in entries {
                let desired = entry.state.lock().desired;
                if desired && self.status(&entry).0 == "stopped" {
                    let _ = self.start(&entry.id, false).await;
                }
                let idle = entry
                    .manifest
                    .as_ref()
                    .and_then(|m| m.service.as_ref())
                    .is_some_and(|s| s.startup == "on-demand")
                    && entry.active.load(Ordering::Acquire) == 0
                    && entry.state.lock().last_used.elapsed() >= Duration::from_secs(600);
                if idle {
                    self.stop_serialized(&entry, true).await;
                }
                let unavailable = matches!(
                    self.status(&entry).0.as_str(),
                    "blocked" | "disabled" | "untrusted" | "invalid"
                );
                let has_task = entry.state.lock().task.is_some();
                if unavailable && has_task {
                    self.stop_serialized(&entry, false).await;
                }
            }
        }
    }
}
fn directory_bytes(root: &Path) -> u64 {
    fn count(root: &Path, left: &mut usize) -> u64 {
        if *left == 0 {
            return 0;
        }
        let mut total = 0;
        for item in fs::read_dir(root)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            if *left == 0 {
                break;
            }
            *left -= 1;
            if let Ok(meta) = item.path().symlink_metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += count(&item.path(), left);
                }
            }
        }
        total
    }
    count(root, &mut 10000)
}

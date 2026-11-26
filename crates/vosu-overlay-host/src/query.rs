//! `vosu.overlayhost.overlays.query` (catalog-v2.md "Overlay query
//! action"): a sessionless action that lists installed overlays, page by
//! page, for another plugin. Core routes it only to plugins whose manifest
//! declares `action:vosu.overlayhost.overlays.query` in `requires` or
//! `optional`; this host additionally answers only `caller.kind = "plugin"`.
//!
//! A page carries the overlay listener's `origin` (core's effective
//! `scheme://host:port`, or `null` while that listener is not bound), so a
//! plugin never takes the URL from a browser. An entry carries the
//! overlay's folder id, display name, origin-relative entry page, its
//! version when the package states one, and its effective settings values
//! for the active profile. It never carries a filesystem path, and never a
//! `password` setting's value.
//!
//! The bundled instance and the composition that owns the overlay storage
//! share no Rust object across the plugin vtable, so composition binds its
//! [`ReloadableCounterService`] and its [`OverlayOrigin`] source to the
//! instance's `data_dir` (which core assigns per bundled plugin and
//! composition) with [`bind_overlay_query`]. The instance answers from its
//! own worker thread; dispatch only enqueues.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak, mpsc};
use std::thread::JoinHandle;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use vosu_plugin_api::{
    ActionInvoke, ActionResult, ActionStatus, CallerKind, CborValue, HostServices, Message,
    PluginError, vosu_status,
};

use crate::model::{CompactValues, Counter, CounterSetting, SettingType};
use crate::names::OverlayName;
use crate::service::{CounterError, ReloadableCounterService};
use crate::settings::{coerce_checkbox, coerce_number, validate_component};

/// The bare action id (`docs/v2/plugin-abi.md` §6.8).
pub const QUERY_ACTION: &str = "vosu.overlayhost.overlays.query";
/// Overlays per page when the caller names no `limit`.
pub const DEFAULT_PAGE: usize = 20;
/// The largest `limit` the argument schema admits.
pub const MAX_PAGE: usize = 50;
/// An overlay whose effective settings encode to more JSON bytes than this
/// reports `settings: null` instead of them.
pub const SETTINGS_BUDGET: usize = 32 * 1024;
/// Encoded JSON bytes of all entries on one page. A page stops early (with
/// `next` set) rather than exceed it. CBOR never needs more than three times
/// a value's JSON bytes, so a full page stays far inside the ABI's 1 MiB
/// frame.
pub const PAGE_BUDGET: usize = 256 * 1024;
/// Queries waiting for the worker; one more is answered `busy`.
const QUEUE: usize = 16;

/// Core's effective overlay listener origin (`scheme://host:port`, no
/// trailing slash), read on every query; `None` while it is not bound.
pub type OverlayOrigin = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Composition's binding of the overlay storage to one bundled instance's
/// `data_dir`. Dropping it unbinds; a later query answers `unavailable`.
#[must_use = "dropping the binding unbinds the overlay query"]
pub struct OverlayQueryBinding {
    id: u64,
}

struct Binding {
    id: u64,
    data_dir: PathBuf,
    counters: Weak<ReloadableCounterService>,
    origin: OverlayOrigin,
}

/// Live bindings: at most one per composition alive in this process.
static BINDINGS: Mutex<Vec<Binding>> = Mutex::new(Vec::new());
static NEXT_BINDING: AtomicU64 = AtomicU64::new(1);

/// Binds `counters` and the overlay listener's `origin` to the
/// `vosu.overlayhost` instance created with `data_dir`. The newest live
/// binding for a directory wins.
pub fn bind_overlay_query(
    data_dir: &Path,
    counters: &Arc<ReloadableCounterService>,
    origin: OverlayOrigin,
) -> OverlayQueryBinding {
    let id = NEXT_BINDING.fetch_add(1, Ordering::Relaxed);
    let mut bindings = BINDINGS.lock().unwrap_or_else(PoisonError::into_inner);
    bindings.retain(|binding| binding.counters.strong_count() > 0);
    bindings.push(Binding {
        id,
        data_dir: data_dir.to_owned(),
        counters: Arc::downgrade(counters),
        origin,
    });
    OverlayQueryBinding { id }
}

impl Drop for OverlayQueryBinding {
    fn drop(&mut self) {
        BINDINGS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|binding| binding.id != self.id);
    }
}

/// The live binding for `data_dir`. The origin source is called by the
/// caller, outside the registry lock.
fn bound(data_dir: &Path) -> Option<(Arc<ReloadableCounterService>, OverlayOrigin)> {
    BINDINGS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .rev()
        .filter(|binding| binding.data_dir == data_dir)
        .find_map(|binding| {
            binding
                .counters
                .upgrade()
                .map(|counters| (counters, Arc::clone(&binding.origin)))
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryArgs {
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// One page of installed overlays ordered by folder id: those after `after`
/// (exclusive), at most `limit` (clamped to `1..=MAX_PAGE`) and at most
/// [`PAGE_BUDGET`] encoded bytes. `next` is the cursor for the following
/// page, `null` on the last one; `origin` is reported as given. Re-reads
/// the overlay root on every call.
pub fn query_page(
    counters: &ReloadableCounterService,
    origin: Option<String>,
    after: Option<&str>,
    limit: usize,
) -> Result<Value, CounterError> {
    let limit = limit.clamp(1, MAX_PAGE);
    let mut installed = counters.installed()?;
    installed.retain(|counter| after.is_none_or(|after| counter.folder_name.as_str() > after));
    installed.sort_by(|a, b| a.folder_name.cmp(&b.folder_name));
    installed.dedup_by(|a, b| a.folder_name == b.folder_name);
    let names = counters.names().snapshot();
    let mut overlays = Vec::new();
    let mut used = 0;
    let mut next = None;
    for counter in &installed {
        if overlays.len() == limit {
            next = Some(last_id(&overlays));
            break;
        }
        let entry = entry(counters, counter, names.get(&counter.folder_name));
        let size = encoded_len(&entry);
        if !overlays.is_empty() && used + size > PAGE_BUDGET {
            next = Some(last_id(&overlays));
            break;
        }
        used += size;
        overlays.push(entry);
    }
    Ok(json!({ "origin": origin, "overlays": overlays, "next": next }))
}

fn last_id(overlays: &[Value]) -> Value {
    overlays
        .last()
        .map(|entry| entry["id"].clone())
        .unwrap_or(Value::Null)
}

fn encoded_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn entry(
    counters: &ReloadableCounterService,
    counter: &Counter,
    name: Option<&OverlayName>,
) -> Value {
    let settings = if validate_component(&counter.folder_name).is_err() {
        // A nested overlay folder has no settings store (one component only).
        Some(Map::new())
    } else {
        counters
            .get_settings(&counter.folder_name)
            .ok()
            .map(|(settings, values)| effective_values(&settings, values))
    }
    .map(Value::Object)
    .filter(|settings| encoded_len(settings) <= SETTINGS_BUDGET);
    json!({
        "id": counter.folder_name,
        "name": name
            .and_then(|name| name.nickname.as_deref())
            .unwrap_or(&counter.name),
        "entry": crate::cards::overlay_path(&counter.folder_name),
        "version": (counter.version_known && !counter.version.is_empty())
            .then_some(counter.version.as_str()),
        "settings": settings,
    })
}

/// The values the overlay itself receives (saved, else the schema default,
/// coerced by type) for exactly the keys its current schema declares, minus
/// every `password` and `header` key. A key any `password` setting declares
/// is omitted even if another setting reuses it.
fn effective_values(settings: &[CounterSetting], mut saved: CompactValues) -> CompactValues {
    let secret = |id: &str| {
        settings
            .iter()
            .any(|setting| setting.kind == SettingType::Password && setting.unique_id == id)
    };
    let mut values = Map::new();
    for setting in settings {
        if matches!(setting.kind, SettingType::Password | SettingType::Header)
            || secret(&setting.unique_id)
            || values.contains_key(&setting.unique_id)
        {
            continue;
        }
        let value = saved
            .remove(&setting.unique_id)
            .unwrap_or_else(|| setting.value.clone());
        let value = match setting.kind {
            SettingType::Number => coerce_number(value),
            SettingType::Checkbox => coerce_checkbox(value),
            _ => value,
        };
        values.insert(setting.unique_id.clone(), value);
    }
    values
}

struct Job {
    invocation: u64,
    after: Option<String>,
    limit: usize,
}

/// The instance's query worker: one thread, started on the first query,
/// fed by a bounded queue and joined when the instance is destroyed.
pub(crate) struct QueryWorker {
    data_dir: PathBuf,
    jobs: Option<mpsc::SyncSender<Job>>,
    join: Option<JoinHandle<()>>,
}

impl QueryWorker {
    pub(crate) fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            jobs: None,
            join: None,
        }
    }

    /// Answers `action` exactly once, now (refusals) or from the worker.
    pub(crate) fn invoke<H: HostServices + Clone + Send + 'static>(
        &mut self,
        host: &H,
        action: ActionInvoke,
    ) -> Result<(), PluginError> {
        let invocation = action.invocation;
        let refuse = |code: &str, message: &str| {
            host.send(&Message::ActionResult(error(invocation, code, message)), 0);
            Ok(())
        };
        if action.action != QUERY_ACTION {
            return refuse("unknown_action", "unknown overlay host action");
        }
        if action.caller.kind != CallerKind::Plugin {
            return refuse("permission_denied", "overlay query answers plugins only");
        }
        let Ok(args) = action.args.deserialized::<QueryArgs>() else {
            return refuse("invalid_arguments", "invalid overlay query arguments");
        };
        let job = Job {
            invocation,
            after: args.after,
            limit: args.limit.unwrap_or(DEFAULT_PAGE),
        };
        if self.jobs.is_none() {
            let (jobs, queue) = mpsc::sync_channel(QUEUE);
            let worker = Worker {
                host: host.clone(),
                data_dir: self.data_dir.clone(),
            };
            let join = std::thread::Builder::new()
                .name("overlay-query".into())
                .spawn(move || worker.run(&queue))
                .map_err(|_| PluginError::from(vosu_status::VOSU_ERR_NOT_READY))?;
            self.jobs = Some(jobs);
            self.join = Some(join);
        }
        match self.jobs.as_ref().map(|jobs| jobs.try_send(job)) {
            Some(Ok(())) => Ok(()),
            _ => refuse("busy", "overlay query capacity reached"),
        }
    }
}

impl Drop for QueryWorker {
    fn drop(&mut self) {
        // Closing the queue ends the worker after its current answer.
        self.jobs = None;
        if let Some(join) = self.join.take()
            && join.thread().id() != std::thread::current().id()
        {
            let _ = join.join();
        }
    }
}

struct Worker<H> {
    host: H,
    data_dir: PathBuf,
}

impl<H: HostServices> Worker<H> {
    fn run(&self, queue: &mpsc::Receiver<Job>) {
        while let Ok(job) = queue.recv() {
            let result = self.answer(&job);
            self.host.send(&Message::ActionResult(result), 0);
        }
    }

    fn answer(&self, job: &Job) -> ActionResult {
        let Some((counters, origin)) = bound(&self.data_dir) else {
            return error(
                job.invocation,
                "unavailable",
                "overlay storage is not ready",
            );
        };
        let page = match query_page(&counters, origin(), job.after.as_deref(), job.limit) {
            Ok(page) => page,
            Err(failure) => {
                tracing::warn!(error = %failure, "overlay query failed");
                return error(
                    job.invocation,
                    "query_failed",
                    "installed overlays could not be read",
                );
            }
        };
        match CborValue::serialized(&page) {
            Ok(value) => ActionResult {
                invocation: job.invocation,
                status: ActionStatus::Ok,
                value: Some(value),
                code: None,
                message: None,
            },
            Err(_) => error(
                job.invocation,
                "query_failed",
                "overlay page did not encode",
            ),
        }
    }
}

fn error(invocation: u64, code: &str, message: &str) -> ActionResult {
    ActionResult {
        invocation,
        status: ActionStatus::Error,
        value: None,
        code: Some(code.into()),
        message: Some(message.into()),
    }
}

#[cfg(test)]
#[path = "query/tests.rs"]
mod tests;

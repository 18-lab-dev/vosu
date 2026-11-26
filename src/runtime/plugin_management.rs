//! The dashboard's plugin management (catalog-v2.md "Process-plugin approval
//! and management", A5.5; `docs/v2/plugin-abi.md` §7.6).
//!
//! [`Roster`] lists every plugin composition tried to load, in instance
//! order: the bundled array, then each `--plugin <dir>`, including those
//! that failed to load. [`PluginManagement`] serves the `/api/plugins`
//! routes over it. Routing has already confined them to the dashboard
//! listener and checked dashboard-origin provenance and `X-Vosu-CSRF` on
//! every `POST` and `DELETE` (`server::routes`). Every action names a
//! plugin id that maps to exactly one instance, and restart and stop also
//! name the generation the caller saw, so a stale button never acts on
//! another generation or plugin.
//!
//! Approval writes the store (`vosu_core::plugin::approval`) only when both
//! submitted digests equal the package's current ones, then starts the
//! plugin through the same gate every load and restart uses. There is no
//! CLI, environment or automatic approval.
//!
//! `GET`/`POST /api/plugins/<id>/settings` (A7.1 `PluginSettings`) edit
//! core's per-plugin settings of any loaded plugin, a catalog-installed one
//! included: the schema is the package's validated `settings_schema`, the
//! values core's storage, and a valid write reaches every live instance as
//! one complete `SETTINGS_CHANGED` (plugin-abi.md §6.9).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, Method, Request, StatusCode, header},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use vosu_core::plugin::approval::{ApprovalStore, Digests, Provenance, Sha256Digest};
use vosu_core::plugin::process::supervisor::{FailureReason, ProcessPhase, ProcessStatus};
use vosu_core::plugin::settings::{SettingsChangeDelivery, SettingsError, SettingsSchemaSource};
use vosu_core::plugin::{
    Execution, HostError, LoadedPlugin, PluginHost, PluginInstanceId, ProcessPackage,
};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{Capability, Runtime};

use crate::server::service::PluginManagementRoutes;
use vosu_core::server::routes::ProviderFuture;

/// How a plugin is trusted (`docs/v2/plugin-abi.md` §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Trust {
    /// The explicit `BUNDLED` array.
    Bundled,
    /// A debug build's `--plugin` in-process developer exception.
    Development,
    /// Every other `--plugin` package: `process` only, after approval.
    User,
}

impl Trust {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Development => "development",
            Self::User => "user",
        }
    }

    /// The trust a `--plugin` package receives in this build.
    pub(super) fn development(runtime: Runtime) -> Self {
        match runtime {
            Runtime::InProcess => Self::Development,
            Runtime::Process => Self::User,
        }
    }
}

/// What the prompt and the list show about one package.
struct Package {
    id: String,
    name: String,
    version: String,
    runtime: Runtime,
    provides: Vec<Capability>,
    requires: Vec<Capability>,
    optional: Vec<Capability>,
    process: Option<ProcessPackage>,
    settings_schema: Option<SettingsSchema>,
}

/// One plugin composition tried to load.
pub(super) struct ManagedPlugin {
    instance: PluginInstanceId,
    trust: Trust,
    directory: Option<PathBuf>,
    package: Option<Package>,
    /// Why it never became a supervised instance: a load refusal, or an
    /// in-process `create` failure.
    failure: Option<String>,
    /// Validated but deliberately never created: listed `inactive` with this
    /// reason (A7.3 `dashboard_disabled`).
    inactive: Option<&'static str>,
}

impl Package {
    fn of(plugin: &LoadedPlugin) -> Self {
        let manifest = plugin.manifest();
        Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            runtime: manifest.runtime,
            provides: manifest.provides.clone(),
            requires: manifest.requires.clone(),
            optional: manifest.optional.clone(),
            process: match plugin.execution() {
                Execution::Process(package) => Some(package.clone()),
                Execution::Local(_) => None,
            },
            settings_schema: plugin.settings_schema().cloned(),
        }
    }
}

/// Every plugin composition tried to load, in instance order.
#[derive(Default)]
pub(super) struct Roster(Vec<ManagedPlugin>);

impl Roster {
    /// A package that was validated. `created` is its initial
    /// `create_instance` result: a process package's failure is the
    /// supervisor's to report, an in-process one is final.
    pub(super) fn loaded(
        &mut self,
        instance: PluginInstanceId,
        trust: Trust,
        directory: Option<PathBuf>,
        plugin: &LoadedPlugin,
        created: Result<(), String>,
    ) {
        let package = Package::of(plugin);
        let failure = match (&package.process, created) {
            (None, Err(error)) => Some(error),
            _ => None,
        };
        self.0.push(ManagedPlugin {
            instance,
            trust,
            directory,
            package: Some(package),
            failure,
            inactive: None,
        });
    }

    /// A validated package that is deliberately not created this run.
    pub(super) fn inactive(
        &mut self,
        instance: PluginInstanceId,
        trust: Trust,
        plugin: &LoadedPlugin,
        reason: &'static str,
    ) {
        self.0.push(ManagedPlugin {
            instance,
            trust,
            directory: None,
            package: Some(Package::of(plugin)),
            failure: None,
            inactive: Some(reason),
        });
    }

    /// A package that was refused before it became an instance.
    pub(super) fn refused(
        &mut self,
        instance: PluginInstanceId,
        trust: Trust,
        directory: Option<PathBuf>,
        error: String,
    ) {
        self.0.push(ManagedPlugin {
            instance,
            trust,
            directory,
            package: None,
            failure: Some(error),
            inactive: None,
        });
    }

    /// Whether a loaded plugin already uses `id`.
    pub(super) fn contains(&self, id: &str) -> bool {
        self.0.iter().any(|plugin| {
            plugin
                .package
                .as_ref()
                .is_some_and(|package| package.id == id)
        })
    }

    /// Whether `instance` is already listed.
    pub(super) fn has_instance(&self, instance: PluginInstanceId) -> bool {
        self.0.iter().any(|plugin| plugin.instance == instance)
    }
}

/// The `/api/plugins` routes.
pub(super) struct PluginManagement {
    host: Arc<PluginHost>,
    store: Arc<ApprovalStore>,
    roster: Roster,
    /// One management action at a time.
    actions: Arc<tokio::sync::Mutex<()>>,
    /// Library digests for listing, keyed by path and invalidated by size
    /// or modification time. An approval and every start hash afresh.
    digests: Mutex<HashMap<PathBuf, CachedDigest>>,
    /// Where a saved plugin setting goes: every live instance of the id
    /// (the host in production).
    delivery: Arc<dyn SettingsChangeDelivery>,
}

/// A library's size and modification time when it was hashed, and its
/// digest.
type CachedDigest = (u64, Option<SystemTime>, Sha256Digest);

/// An error response: a stable `code` and a message for people.
fn refusal(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    respond(status, json!({"code": code, "error": message.into()}))
}

/// A restart or stop named a generation that is no longer current.
fn generation_changed() -> Response {
    refusal(
        StatusCode::CONFLICT,
        "generation_changed",
        "the plugin changed since it was shown",
    )
}

fn respond(status: StatusCode, value: Value) -> Response {
    let mut response = Response::new(Body::from(value.to_string()));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// One percent-encoded path segment, decoded once.
fn decode_segment(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let pair = bytes.get(index + 1..index + 3)?;
            let text = std::str::from_utf8(pair).ok()?;
            decoded.push(u8::from_str_radix(text, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .ok()
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalInput {
    manifest_sha256: String,
    library_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationInput {
    generation: u64,
}

/// One submitted value (`POST /api/plugins/<id>/settings`).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingInput {
    #[serde(rename = "uniqueID")]
    unique_id: String,
    value: Value,
}

/// A refused settings read or write: fixed text naming at most a setting's
/// key, never a submitted value or a storage path.
fn settings_refusal(plugin: &str, error: &SettingsError) -> Response {
    match error {
        SettingsError::Schema(error) => refusal(
            StatusCode::BAD_REQUEST,
            "invalid_settings",
            error.to_string(),
        ),
        SettingsError::ManagedElsewhere(_) | SettingsError::NotBridged(_) => refusal(
            StatusCode::CONFLICT,
            "settings_managed_elsewhere",
            "these settings are changed on the Settings page",
        ),
        SettingsError::Storage(detail) => {
            tracing::warn!(%plugin, %detail, "plugin settings storage failed");
            refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings_not_saved",
                "vosu could not read or save this plugin's settings",
            )
        }
    }
}

fn capabilities(list: &[Capability]) -> Value {
    list.iter()
        .map(|capability| json!({"id": capability.id, "version": capability.version}))
        .collect()
}

/// D8: a `requires`/`optional` entry naming `field:vosu.osu.internal` or a
/// descendant (catalog-v2.md install plans).
fn requires_internal_paths(package: &Package) -> bool {
    package
        .requires
        .iter()
        .chain(&package.optional)
        .any(|capability| vosu_plugin_api::covers("field:vosu.osu.internal", &capability.id))
}

fn failure_reason(failure: &FailureReason) -> &'static str {
    match failure {
        FailureReason::Startup(failure) => failure.stage.as_str(),
        FailureReason::Loss(cause) => cause.reason(),
        FailureReason::Refused(_) => "refused",
    }
}

impl PluginManagement {
    pub(super) fn new(host: Arc<PluginHost>, store: Arc<ApprovalStore>, roster: Roster) -> Self {
        Self {
            delivery: Arc::clone(&host) as Arc<dyn SettingsChangeDelivery>,
            host,
            store,
            roster,
            actions: Arc::new(tokio::sync::Mutex::new(())),
            digests: Mutex::new(HashMap::new()),
        }
    }

    /// Tests observe settings deliveries instead of a live instance.
    #[cfg(all(test, feature = "test-harness"))]
    fn with_settings_delivery(mut self, delivery: Arc<dyn SettingsChangeDelivery>) -> Self {
        self.delivery = delivery;
        self
    }

    /// The library digest for listing; `None` when it cannot be read.
    fn listed_digests(&self, package: &ProcessPackage) -> Option<Digests> {
        let library = package.library();
        let metadata = std::fs::metadata(library).ok()?;
        let key = (metadata.len(), metadata.modified().ok());
        let cached = self
            .digests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(library)
            .filter(|(len, modified, _)| (*len, *modified) == key)
            .map(|(_, _, digest)| *digest);
        let library_sha256 = match cached {
            Some(digest) => digest,
            None => {
                let digest = Digests::of(package).ok()?.library_sha256;
                self.digests
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(library.to_owned(), (key.0, key.1, digest));
                digest
            }
        };
        Some(Digests {
            manifest_sha256: Sha256Digest(*package.manifest_sha256()),
            library_sha256,
        })
    }

    /// Whether the store approves `digests` for `id`'s `package`.
    fn approved(&self, id: &str, package: &ProcessPackage, digests: Option<Digests>) -> bool {
        let provenance = Provenance::of(package);
        digests.is_some_and(|digests| {
            self.store.record(id, &provenance) == Ok(Some(digests.record(id, provenance.clone())))
        })
    }

    fn generation(status: Option<&ProcessStatus>) -> u64 {
        status.map_or(0, |status| status.generation)
    }

    /// One plugin's list entry.
    fn view(&self, plugin: &ManagedPlugin, fresh: Option<Digests>) -> Value {
        let provenance = match (plugin.trust, &plugin.directory) {
            (Trust::Bundled, _) => json!({"kind": "bundled"}),
            (_, directory) => json!({
                "kind": "development",
                "path": plugin
                    .package
                    .as_ref()
                    .and_then(|package| package.process.as_ref())
                    .map(|process| process.root().to_owned())
                    .or_else(|| directory.clone()),
            }),
        };
        let mut view = json!({
            "id": null,
            "trust": plugin.trust.as_str(),
            "provenance": provenance,
            "generation": 0,
            "state": "failed",
            "reason": null,
            "detail": null,
            "crash_events": 0,
            "restarts": 0,
            "restart_delay_ms": null,
            "approved": false,
            "approval": null,
        });
        let Some(package) = &plugin.package else {
            view["reason"] = json!("load");
            view["detail"] = json!(plugin.failure);
            return view;
        };
        view["id"] = json!(package.id);
        view["name"] = json!(package.name);
        view["version"] = json!(package.version);
        view["runtime"] = json!(match package.runtime {
            Runtime::InProcess => "in-process",
            Runtime::Process => "process",
        });
        view["provides"] = capabilities(&package.provides);
        view["requires"] = capabilities(&package.requires);
        view["optional"] = capabilities(&package.optional);
        view["requires_internal_paths"] = json!(requires_internal_paths(package));
        if let Some(reason) = plugin.inactive {
            // Validated and listed, never created: no generation, no actions.
            view["state"] = json!("inactive");
            view["reason"] = json!(reason);
            return view;
        }
        let Some(process) = &package.process else {
            // In-process: running from startup until the daemon exits.
            view["generation"] = json!(1);
            match &plugin.failure {
                Some(error) => {
                    view["reason"] = json!("create");
                    view["detail"] = json!(error);
                }
                None => view["state"] = json!("running"),
            }
            return view;
        };
        let status = self.host.process_status(plugin.instance);
        let digests = fresh.or_else(|| self.listed_digests(process));
        let approved = self.approved(&package.id, process, digests);
        view["approved"] = json!(approved);
        view["generation"] = json!(Self::generation(status.as_ref()));
        let Some(status) = status else {
            view["reason"] = json!("create");
            view["detail"] = json!(plugin.failure);
            return view;
        };
        view["crash_events"] = json!(status.crash_events);
        view["restarts"] = json!(status.restarts);
        if let Some(failure) = &status.last_failure {
            view["reason"] = json!(failure_reason(failure));
            view["detail"] = json!(failure.to_string());
        }
        let state = match status.phase {
            ProcessPhase::Starting => "starting",
            ProcessPhase::Running => "running",
            ProcessPhase::Backoff { delay, .. } => {
                view["restart_delay_ms"] = json!(delay.as_millis() as u64);
                "backoff"
            }
            ProcessPhase::Failed | ProcessPhase::Stopped if !approved => {
                if status.last_failure.is_none() {
                    view["reason"] = json!("approval");
                }
                "approval_required"
            }
            ProcessPhase::Failed => "failed",
            ProcessPhase::Stopped => "stopped",
        };
        view["state"] = json!(state);
        if state == "approval_required"
            && let Some(digests) = digests
        {
            view["approval"] = json!({
                "manifest_sha256": digests.manifest_sha256,
                "library_sha256": digests.library_sha256,
            });
        }
        view
    }

    fn list(&self) -> Value {
        json!({
            "plugins": self
                .roster
                .0
                .iter()
                .map(|plugin| self.view(plugin, None))
                .collect::<Vec<_>>(),
        })
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.roster.0.iter().position(|plugin| {
            plugin
                .package
                .as_ref()
                .is_some_and(|package| package.id == id)
        })
    }

    /// The user-trust process package at `index`.
    fn user_process(&self, index: usize) -> Option<(&str, &ProcessPackage)> {
        let plugin = &self.roster.0[index];
        let package = plugin.package.as_ref()?;
        (plugin.trust == Trust::User)
            .then_some(())
            .and(package.process.as_ref())
            .map(|process| (package.id.as_str(), process))
    }

    /// `POST /api/plugins/<id>/approval`: both digests must equal the
    /// package's current ones; then the record is persisted and the plugin
    /// started.
    fn approve(&self, index: usize, body: &[u8]) -> Response {
        let Some((id, process)) = self.user_process(index) else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "approval_not_applicable",
                "only a user-trust process plugin needs approval",
            );
        };
        let Ok(input) = serde_json::from_slice::<ApprovalInput>(body) else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "expected {\"manifest_sha256\", \"library_sha256\"}",
            );
        };
        let (Some(manifest_sha256), Some(library_sha256)) = (
            Sha256Digest::parse_hex(&input.manifest_sha256),
            Sha256Digest::parse_hex(&input.library_sha256),
        ) else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "a digest is 64 lowercase hexadecimal digits",
            );
        };
        let current = match Digests::of(process) {
            Ok(current) => current,
            Err(failure) => {
                return refusal(StatusCode::CONFLICT, "approval_stale", failure.to_string());
            }
        };
        if current
            != (Digests {
                manifest_sha256,
                library_sha256,
            })
        {
            return refusal(
                StatusCode::CONFLICT,
                "approval_stale",
                "the package changed since it was shown; review it again",
            );
        }
        let provenance = Provenance::of(process);
        let record = current.record(id, provenance.clone());
        let unchanged = self.store.record(id, &provenance) == Ok(Some(record.clone()));
        if let Err(error) = self.store.approve(record) {
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "approval_not_saved",
                format!("the approval was not saved: {error}"),
            );
        }
        let instance = self.roster.0[index].instance;
        let live = self.host.process_status(instance).is_some_and(|status| {
            matches!(
                status.phase,
                ProcessPhase::Starting | ProcessPhase::Running | ProcessPhase::Backoff { .. }
            )
        });
        if !(unchanged && live) {
            self.start(instance, id, None);
        }
        respond(
            StatusCode::OK,
            self.view(&self.roster.0[index], Some(current)),
        )
    }

    /// A manual restart (§7.6): its outcome is the plugin's new state.
    /// With `generation`, the host refuses unless it is still current,
    /// under the same lock that begins the next one.
    fn start(
        &self,
        instance: PluginInstanceId,
        id: &str,
        generation: Option<u64>,
    ) -> Option<Response> {
        let started = match generation {
            Some(generation) => self.host.restart_process_generation(instance, generation),
            None => self.host.restart_process_instance(instance),
        };
        match started {
            Ok(()) | Err(HostError::ProcessStartupFailed(_) | HostError::InstanceLost) => None,
            Err(HostError::GenerationChanged) => Some(generation_changed()),
            Err(error @ HostError::State(_)) => {
                Some(refusal(StatusCode::CONFLICT, "busy", error.to_string()))
            }
            Err(error) => {
                tracing::warn!(plugin = %id, %error, "manual plugin start failed");
                Some(refusal(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "start_failed",
                    error.to_string(),
                ))
            }
        }
    }

    /// The controlled teardown of the instance at `index` (§7.6). With
    /// `generation`, the host refuses unless it is still current, under
    /// the same lock that marks it stopped.
    fn tear_down(&self, index: usize, generation: Option<u64>) -> Option<Response> {
        let plugin = &self.roster.0[index];
        let stopped = match generation {
            Some(generation) => self
                .host
                .stop_process_generation(plugin.instance, generation),
            None => self.host.teardown_instance(plugin.instance),
        };
        match stopped {
            Ok(()) | Err(HostError::UnknownInstance) => None,
            Err(HostError::GenerationChanged) => Some(generation_changed()),
            Err(error) => {
                // The teardown still released the instance; only its
                // `destroy` grace ran out or a start was slow to retire.
                tracing::warn!(instance = ?plugin.instance, %error, "plugin teardown");
                None
            }
        }
    }

    /// `DELETE /api/plugins/<id>/approval`: removes the record, then tears
    /// the instance down.
    fn revoke(&self, index: usize) -> Response {
        let Some((id, process)) = self.user_process(index) else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "approval_not_applicable",
                "only a user-trust process plugin has an approval",
            );
        };
        if let Err(error) = self.store.revoke(id, &Provenance::of(process)) {
            return refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "approval_not_saved",
                format!("the approval was not removed: {error}"),
            );
        }
        self.tear_down(index, None);
        respond(StatusCode::OK, self.view(&self.roster.0[index], None))
    }

    /// `POST /api/plugins/<id>/restart` and `/stop`: exactly the generation
    /// the caller saw, of a process instance. The host compares it under
    /// its own lock, so an automatic restart after this request was sent
    /// refuses it instead of letting it act on the new generation.
    fn lifecycle(&self, index: usize, body: &[u8], restart: bool) -> Response {
        let plugin = &self.roster.0[index];
        let Some(package) = &plugin.package else {
            return refusal(
                StatusCode::CONFLICT,
                "not_loaded",
                "the plugin did not load",
            );
        };
        if package.process.is_none() {
            return refusal(
                StatusCode::CONFLICT,
                "requires_daemon_restart",
                "an in-process plugin stops or restarts only with vosu",
            );
        }
        let Ok(input) = serde_json::from_slice::<GenerationInput>(body) else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "expected {\"generation\"}",
            );
        };
        let refused = if restart {
            self.start(plugin.instance, &package.id, Some(input.generation))
        } else {
            self.tear_down(index, Some(input.generation))
        };
        if let Some(refused) = refused {
            return refused;
        }
        respond(StatusCode::OK, self.view(plugin, None))
    }

    fn route(&self, method: &Method, path: &str, body: &[u8]) -> Response {
        if method == Method::GET && path == "/api/plugins" {
            return respond(StatusCode::OK, self.list());
        }
        let Some((id, action)) = path
            .strip_prefix("/api/plugins/")
            .and_then(|rest| rest.split_once('/'))
            .and_then(|(id, action)| Some((decode_segment(id)?, action)))
        else {
            return refusal(StatusCode::NOT_FOUND, "unknown_plugin", "no such plugin");
        };
        if action == "settings" {
            return match self.settings_schema(&id) {
                Some(schema) => self.settings(&id, schema, method, body),
                None => refusal(StatusCode::NOT_FOUND, "unknown_plugin", "no such plugin"),
            };
        }
        let Some(index) = self.find(&id) else {
            return refusal(StatusCode::NOT_FOUND, "unknown_plugin", "no such plugin");
        };
        match (method, action) {
            (&Method::POST, "approval") => self.approve(index, body),
            (&Method::DELETE, "approval") => self.revoke(index),
            (&Method::POST, "restart") => self.lifecycle(index, body, true),
            (&Method::POST, "stop") => self.lifecycle(index, body, false),
            _ => refusal(StatusCode::NOT_FOUND, "unknown_route", "no such route"),
        }
    }

    /// `id`'s validated `settings_schema`: `Some(None)` for a known plugin
    /// that declares none, `None` for an id no listed or loaded plugin has.
    /// A catalog-installed plugin is not in the roster; the host resolves
    /// it while an instance of it is registered.
    fn settings_schema(&self, id: &str) -> Option<Option<SettingsSchema>> {
        if let Some(index) = self.find(id) {
            return self.roster.0[index]
                .package
                .as_ref()
                .map(|package| package.settings_schema.clone());
        }
        if let Some(schema) = self.host.schema_for(id) {
            return Some(Some(schema));
        }
        self.host
            .validation_snapshot()
            .providers
            .iter()
            .any(|provider| provider.id == id)
            .then_some(None)
    }

    /// `GET`/`POST /api/plugins/<id>/settings` (A7.1 `PluginSettings`). A
    /// valid write is stored, then delivered once; an invalid one changes
    /// and delivers nothing.
    fn settings(
        &self,
        id: &str,
        schema: Option<SettingsSchema>,
        method: &Method,
        body: &[u8],
    ) -> Response {
        let service = self.host.settings();
        if method == Method::GET {
            return match service.dashboard_view(id, schema.as_ref()) {
                Ok(view) => respond(StatusCode::OK, view),
                Err(error) => settings_refusal(id, &error),
            };
        }
        let Some(schema) = schema else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "no_settings",
                "this plugin declares no settings",
            );
        };
        let Ok(inputs) = serde_json::from_slice::<Vec<SettingInput>>(body) else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "expected [{\"uniqueID\", \"value\"}]",
            );
        };
        let inputs = inputs
            .into_iter()
            .map(|input| (input.unique_id, input.value))
            .collect();
        let message = match service.submit(id, &schema, inputs) {
            Ok(message) => message,
            Err(error) => return settings_refusal(id, &error),
        };
        self.delivery.settings_changed(id, message);
        match service.dashboard_view(id, Some(&schema)) {
            Ok(view) => respond(StatusCode::OK, view),
            Err(error) => settings_refusal(id, &error),
        }
    }
}

impl PluginManagementRoutes for PluginManagement {
    fn handle(
        self: Arc<Self>,
        request: Request<Body>,
        body: Bytes,
    ) -> ProviderFuture<'static, Response> {
        Box::pin(async move {
            let method = request.method().clone();
            let path = request.uri().path().to_owned();
            // Listing hashes libraries and actions wait for starts and
            // teardowns: never on a runtime worker, one action at a time.
            let _serialized = if method == Method::GET {
                None
            } else {
                Some(Arc::clone(&self.actions).lock_owned().await)
            };
            tokio::task::spawn_blocking(move || self.route(&method, &path, &body))
                .await
                .unwrap_or_else(|error| {
                    refusal(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal",
                        format!("plugin management task failed: {error}"),
                    )
                })
        })
    }
}

#[cfg(all(test, feature = "test-harness"))]
#[path = "tests/plugin_management.rs"]
mod tests;

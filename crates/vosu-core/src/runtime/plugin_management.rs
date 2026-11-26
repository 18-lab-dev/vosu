//! The dashboard's plugin management (catalog-v2.md "Process-plugin approval
//! and management", A5.5; `docs/v2/plugin-abi.md` §7.6).
//!
//! [`Roster`] lists the bundled array and `--plugin <dir>` inputs; the
//! catalog adds and removes its own entries as packages commit.
//! [`PluginManagement`] serves the `/api/plugins`
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

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use super::catalog::RootHandoff;
use crate::catalog::runtime::CatalogRuntime;
use crate::plugin::approval::{ApprovalStore, Digests, Provenance, Sha256Digest};
use crate::plugin::process::supervisor::{FailureReason, ProcessPhase, ProcessStatus};
use crate::plugin::settings::{SettingsChangeDelivery, SettingsError, SettingsSchemaSource};
use crate::plugin::{
    Execution, HostError, LoadedPlugin, PluginHost, PluginInstanceId, ProcessPackage,
};
use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, Method, Request, StatusCode, header},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{Capability, Runtime};

use crate::server::routes::ProviderFuture;
use crate::server::service::PluginManagementRoutes;

/// How a plugin is trusted (`docs/v2/plugin-abi.md` §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Trust {
    /// The explicit `BUNDLED` array.
    Bundled,
    /// A debug build's `--plugin` in-process developer exception.
    Development,
    /// Every other `--plugin` package: `process` only, after approval.
    User,
    Installed,
    /// A debug-only official namespace fixture; still needs process approval.
    #[cfg(feature = "test-harness")]
    FixtureOfficial,
}

impl Trust {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Development => "development",
            Self::User => "user",
            Self::Installed => "user",
            #[cfg(feature = "test-harness")]
            Self::FixtureOfficial => "fixture-official",
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
    linux_user_namespaces: bool,
    provides: Vec<Capability>,
    requires: Vec<Capability>,
    optional: Vec<Capability>,
    process: Option<ProcessPackage>,
    library_digest: Mutex<Option<CachedDigest>>,
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

impl ManagedPlugin {
    pub(super) fn installed(instance: PluginInstanceId, plugin: &LoadedPlugin) -> Self {
        Self {
            instance,
            trust: Trust::Installed,
            directory: None,
            package: Some(Package::of(plugin)),
            failure: None,
            inactive: None,
        }
    }

    pub(super) fn instance(&self) -> PluginInstanceId {
        self.instance
    }
}

impl Package {
    fn of(plugin: &LoadedPlugin) -> Self {
        let manifest = plugin.manifest();
        Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            runtime: manifest.runtime,
            linux_user_namespaces: manifest.linux_user_namespaces(),
            provides: manifest.provides.clone(),
            requires: manifest.requires.clone(),
            optional: manifest.optional.clone(),
            process: match plugin.execution() {
                Execution::Process(package) => Some(package.clone()),
                Execution::Local(_) => None,
            },
            settings_schema: plugin.settings_schema().cloned(),
            library_digest: Mutex::new(None),
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
}

/// The `/api/plugins` routes.
pub(super) struct PluginManagement {
    host: Arc<PluginHost>,
    store: Arc<ApprovalStore>,
    roster: Roster,
    catalog: Option<(Arc<CatalogRuntime>, Arc<RootHandoff>)>,
    /// One management action at a time.
    actions: Arc<tokio::sync::Mutex<()>>,
    /// Where a saved plugin setting goes: every live instance of the id
    /// (the host in production).
    delivery: Arc<dyn SettingsChangeDelivery>,
}

/// A listing hash is scoped to this package and generation. Approval and
/// launch checks never use it.
type CachedDigest = ((u64, Option<SystemTime>, u64, bool), Sha256Digest);

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

fn approval_input(body: &[u8]) -> Result<Digests, &'static str> {
    let input: ApprovalInput = serde_json::from_slice(body)
        .map_err(|_| "expected {\"manifest_sha256\", \"library_sha256\"}")?;
    let parse =
        |value| Sha256Digest::parse_hex(value).ok_or("a digest is 64 lowercase hexadecimal digits");
    Ok(Digests {
        manifest_sha256: parse(&input.manifest_sha256)?,
        library_sha256: parse(&input.library_sha256)?,
    })
}

fn invalid_approval_request(message: &'static str) -> Response {
    refusal(StatusCode::BAD_REQUEST, "invalid_request", message)
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
            catalog: None,
            actions: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(super) fn approvals(&self) -> &ApprovalStore {
        &self.store
    }

    pub(super) fn attach_catalog(
        &mut self,
        runtime: Arc<CatalogRuntime>,
        handoff: Arc<RootHandoff>,
    ) {
        self.catalog = Some((runtime, handoff));
    }

    /// Tests observe settings deliveries instead of a live instance.
    #[cfg(all(test, feature = "test-harness"))]
    fn with_settings_delivery(mut self, delivery: Arc<dyn SettingsChangeDelivery>) -> Self {
        self.delivery = delivery;
        self
    }

    /// The library digest for listing; `None` when it cannot be read.
    fn listed_digests(
        package: &Package,
        process: &ProcessPackage,
        generation: u64,
        approval_failed: bool,
    ) -> Option<Digests> {
        let manifest_sha256 = if process.installed_provenance().is_some() {
            Digests::installed_manifest(process.root()).ok()?
        } else {
            Sha256Digest(*process.manifest_sha256())
        };
        let library = process.library();
        let metadata = std::fs::metadata(library).ok()?;
        let key = (
            metadata.len(),
            metadata.modified().ok(),
            generation,
            approval_failed,
        );
        let mut cached = package
            .library_digest
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let reusable = cached
            .filter(|(stamp, _)| *stamp == key)
            .map(|(_, digest)| digest);
        let library_sha256 = match reusable {
            Some(digest) => digest,
            None => {
                let digest = Digests::files(manifest_sha256, library)
                    .ok()?
                    .library_sha256;
                *cached = Some((key, digest));
                digest
            }
        };
        Some(Digests {
            manifest_sha256,
            library_sha256,
        })
    }

    /// Whether the store approves `digests` for `id`'s `package`.
    fn approved(&self, id: &str, package: &ProcessPackage, digests: Option<Digests>) -> bool {
        digests.is_some_and(|digests| self.store.check_digests(id, package, digests).is_ok())
    }

    fn generation(status: Option<&ProcessStatus>) -> u64 {
        status.map_or(0, |status| status.generation)
    }

    /// One plugin's list entry.
    fn view(&self, plugin: &ManagedPlugin, fresh: Option<Digests>) -> Value {
        let reviewed = if plugin.trust == Trust::Installed {
            self.updated_installed(plugin)
                .map(|loaded| Package::of(&loaded))
        } else {
            None
        };
        let provenance = match (plugin.trust, &plugin.directory) {
            (Trust::Installed, _) => json!(
                plugin
                    .package
                    .as_ref()
                    .and_then(|package| package.process.as_ref())
                    .and_then(ProcessPackage::installed_provenance)
            ),
            (Trust::Bundled, _) => json!({"kind": "bundled"}),
            #[cfg(feature = "test-harness")]
            (Trust::FixtureOfficial, _) => json!({"kind": "fixture-official"}),
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
            "trust": self.installed_trust(plugin).unwrap_or_else(|| plugin.trust.as_str()),
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
        let Some(package) = reviewed.as_ref().or(plugin.package.as_ref()) else {
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
        if package.linux_user_namespaces {
            view["linux_user_namespaces"] = json!(true);
        }
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
        let generation = Self::generation(status.as_ref());
        let approval_failed = status.as_ref().is_some_and(|status| {
            matches!(
                &status.last_failure,
                Some(FailureReason::Startup(failure))
                    if failure.stage == crate::plugin::process::control::StartupStage::Approval
            )
        });
        let digests = fresh
            .or_else(|| Self::listed_digests(package, process, generation, approval_failed))
            .filter(|digests| digests.manifest_sha256.0 == *process.manifest_sha256());
        let approved = self.approved(&package.id, process, digests);
        view["approved"] = json!(approved);
        if plugin.trust == Trust::Installed {
            view["approval_revocable"] = json!(self.installed_approval_revocable(plugin, digests));
        }
        view["generation"] = json!(generation);
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
        let mut plugins: Vec<_> = self
            .roster
            .0
            .iter()
            .map(|plugin| self.view(plugin, None))
            .collect();
        if let Some((_, handoff)) = &self.catalog {
            plugins.extend(
                handoff
                    .processes()
                    .iter()
                    .map(|plugin| self.view(plugin, None)),
            );
        }
        json!({"plugins": plugins})
    }

    fn find(&self, id: &str) -> Option<usize> {
        self.roster.0.iter().position(|plugin| {
            plugin
                .package
                .as_ref()
                .is_some_and(|package| package.id == id)
        })
    }

    /// A validated development package that can receive explicit approval.
    fn user_process(plugin: &ManagedPlugin) -> Option<(&str, &ProcessPackage)> {
        let package = plugin.package.as_ref()?;
        match plugin.trust {
            Trust::User => (),
            #[cfg(feature = "test-harness")]
            Trust::FixtureOfficial => (),
            _ => return None,
        }
        package
            .process
            .as_ref()
            .map(|process| (package.id.as_str(), process))
    }

    /// `POST /api/plugins/<id>/approval`: both digests must equal the
    /// package's current ones; then the record is persisted and the plugin
    /// started.
    fn approve(&self, plugin: &ManagedPlugin, body: &[u8]) -> Response {
        if plugin.trust == Trust::Installed {
            return self.approve_installed(plugin, body);
        }
        let Some((id, process)) = Self::user_process(plugin) else {
            return refusal(
                StatusCode::BAD_REQUEST,
                "approval_not_applicable",
                "only a user-trust process plugin needs approval",
            );
        };
        let requested = match approval_input(body) {
            Ok(requested) => requested,
            Err(message) => return invalid_approval_request(message),
        };
        let current = match Digests::of(process) {
            Ok(current) => current,
            Err(failure) => {
                return refusal(StatusCode::CONFLICT, "approval_stale", failure.to_string());
            }
        };
        if current != requested {
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
        let instance = plugin.instance;
        let live = self.host.process_status(instance).is_some_and(|status| {
            matches!(
                status.phase,
                ProcessPhase::Starting | ProcessPhase::Running | ProcessPhase::Backoff { .. }
            )
        });
        if !(unchanged && live) {
            self.start(plugin, id, None);
        }
        respond(StatusCode::OK, self.view(plugin, Some(current)))
    }

    /// A manual restart (§7.6): its outcome is the plugin's new state.
    /// With `generation`, the host refuses unless it is still current,
    /// under the same lock that begins the next one.
    fn start(&self, plugin: &ManagedPlugin, id: &str, generation: Option<u64>) -> Option<Response> {
        let instance = plugin.instance;
        let started = match generation {
            Some(generation) => self.host.restart_process_generation(instance, generation),
            None => self.host.restart_process_instance(instance),
        };
        if plugin.trust == Trust::Installed
            && !matches!(
                started,
                Err(HostError::GenerationChanged | HostError::State(_))
            )
        {
            self.report_installed_start(id, &started);
        }
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
    fn tear_down(&self, plugin: &ManagedPlugin, generation: Option<u64>) -> Option<Response> {
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
    fn revoke(&self, plugin: &ManagedPlugin) -> Response {
        if plugin.trust == Trust::Installed {
            return self.revoke_installed(plugin);
        }
        let Some((id, process)) = Self::user_process(plugin) else {
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
        self.tear_down(plugin, None);
        respond(StatusCode::OK, self.view(plugin, None))
    }

    /// `POST /api/plugins/<id>/restart` and `/stop`: exactly the generation
    /// the caller saw, of a process instance. The host compares it under
    /// its own lock, so an automatic restart after this request was sent
    /// refuses it instead of letting it act on the new generation.
    fn lifecycle(&self, plugin: &ManagedPlugin, body: &[u8], restart: bool) -> Response {
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
            self.start(plugin, &package.id, Some(input.generation))
        } else {
            self.tear_down(plugin, Some(input.generation))
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
        let _catalog_operation = if method != Method::GET
            && self.find(&id).is_none()
            && let Some((runtime, _)) = &self.catalog
            && runtime
                .installed()
                .is_some_and(|store| store.plugin(&id).is_some())
        {
            match runtime.plans().begin_operation() {
                Ok(operation) => Some(operation),
                Err(error) => return refusal(StatusCode::CONFLICT, "busy", error.to_string()),
            }
        } else {
            None
        };
        if action == "settings" {
            return match self.settings_schema(&id) {
                Some(schema) => self.settings(&id, schema, method, body),
                None => refusal(StatusCode::NOT_FOUND, "unknown_plugin", "no such plugin"),
            };
        }
        let installed;
        let plugin = if let Some(index) = self.find(&id) {
            &self.roster.0[index]
        } else {
            installed = self
                .catalog
                .as_ref()
                .and_then(|(_, handoff)| handoff.process(&id));
            let Some(plugin) = installed.as_deref() else {
                return refusal(StatusCode::NOT_FOUND, "unknown_plugin", "no such plugin");
            };
            plugin
        };
        match (method, action) {
            (&Method::POST, "approval") => self.approve(plugin, body),
            (&Method::DELETE, "approval") => self.revoke(plugin),
            (&Method::POST, "restart") => self.lifecycle(plugin, body, true),
            (&Method::POST, "stop") => self.lifecycle(plugin, body, false),
            _ => refusal(StatusCode::NOT_FOUND, "unknown_route", "no such route"),
        }
    }

    /// `id`'s validated `settings_schema`: `Some(None)` for a known plugin
    /// that declares none, `None` for an id no listed or loaded plugin has.
    /// Live catalog in-process plugins have no lifecycle roster entry.
    fn settings_schema(&self, id: &str) -> Option<Option<SettingsSchema>> {
        if let Some(index) = self.find(id) {
            return self.roster.0[index]
                .package
                .as_ref()
                .map(|package| package.settings_schema.clone());
        }
        if let Some(plugin) = self
            .catalog
            .as_ref()
            .and_then(|(_, handoff)| handoff.process(id))
        {
            return plugin
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

#[path = "plugin_management_catalog.rs"]
mod catalog;

#[cfg(all(test, feature = "test-harness"))]
#[path = "tests/plugin_management.rs"]
mod tests;

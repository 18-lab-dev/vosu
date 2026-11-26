//! Plugins loaded at startup: the explicit `plugins::BUNDLED` array (D13,
//! `docs/v2/plugin-abi.md` §2, §7.1 — granted `in-process` unconditionally)
//! and `--plugin <dir>` development plugins (§7.1, the `--tool <dir>`
//! analog `crate::tools::development_roots`) under catalog-v2.md A5.5's
//! build policy (`vosu_core::plugin::load_development`):
//!
//! - debug builds keep the documented in-process developer exception;
//! - prerelease release builds accept `--plugin` without the
//!   release-forbidden `test-harness` feature, as `user` trust: an
//!   in-process request is refused and left unloaded, a process request
//!   runs only after approval;
//! - stable release builds refuse every `--plugin` package.
//!
//! A `--plugin` process package starts only through the approval gate
//! (`vosu_core::plugin::approval::ApprovalGate`), the one launcher
//! production composition installs, and only when such a package exists.
//! Every load and restart checks the package's approval record; without
//! one it stays unloaded until the dashboard approves it
//! (`plugin_management`). An optional plugin's failure is logged and never
//! fails startup; a required bundled plugin's failure does. Every instance
//! this module creates is destroyed with a grace period at shutdown
//! (`lifecycle::ProductionRuntime::shutdown`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::plugin::approval::{ApprovalGate, ApprovalStore};
use crate::plugin::process::control::StartupStage;
use crate::plugin::process::launcher::ProcessHostLauncher;
use crate::plugin::{
    self, DEFAULT_QUEUE_CAPACITY, Execution, HostError, PluginHost, PluginInstanceId, WorkerPool,
};
use vosu_sys::platform::Paths;

use super::Bundle;
use super::plugin_management::{PluginManagement, Roster, Trust};

/// Matches the listener drain and the shutdown grace documented for plugin
/// `destroy` in `docs/v2/plugin-abi.md` §4.
pub(super) const PLUGIN_DESTROY_GRACE: Duration = Duration::from_secs(5);

/// Worker count for the shared `concurrent_dispatch` pool every loaded
/// instance that opts in shares (`docs/v2/plugin-abi.md` §4): a small
/// fixed-size pool, not one thread per concurrent instance, since sharing
/// it across instances is the whole point of `concurrent_dispatch`.
const PLUGIN_CONCURRENT_WORKERS: usize = 4;


/// Loads required production plugins first, followed by optional bundled
/// fixtures. Required-plugin failure aborts composition; optional fixtures keep
/// warn-and-continue behavior. Array positions remain reserved even on failure.
/// `bundled` is `bundle.plugins` in production; `bundle` names the required
/// identities, the dashboard and the version.
///
/// With `dashboard` false (A7.3), the dashboard host is validated and keeps
/// its position, so the registry still equals `BUNDLED`, but it is never
/// created: the roster lists it `inactive` with reason `dashboard_disabled`.
#[expect(
    clippy::too_many_arguments,
    reason = "explicit startup composition inputs; the dashboard switch is resolved once by run_cli"
)]
pub(super) fn load_bundled(
    bundle: &Bundle,
    host: &Arc<PluginHost>,
    paths: &Paths,
    logger: &crate::logging::Logger,
    pool: &Arc<WorkerPool>,
    bundled: &[plugin::BundledPlugin<'_>],
    development_dirs: &[PathBuf],
    roster: &mut Roster,
    dashboard: bool,
) -> std::io::Result<u64> {
    if bundled.len() < bundle.required.len() {
        return Err(std::io::Error::other(format!(
            "required bundled plugin set is incomplete: {} are required",
            bundle.required.join(", ")
        )));
    }
    let version = bundle.version();
    for (index, entry) in bundled.iter().enumerate() {
        let instance = PluginInstanceId::new(index as u64);
        // Required identities and positions are explicit, not linker inventory.
        let loaded = plugin::load_bundled(
            &version,
            &source_snapshot(host, bundled, development_dirs),
            std::slice::from_ref(entry),
        )
        .map_err(std::io::Error::other)
        .and_then(|mut loaded| {
            let loaded = loaded.remove(0);
            match bundle.required.get(index) {
                Some(required) if loaded.manifest().id != *required => Err(std::io::Error::other(
                    format!("required bundled plugin {required} is missing"),
                )),
                _ => Ok(loaded),
            }
        });
        let result = loaded.and_then(|loaded| {
            if !dashboard && loaded.manifest().id == bundle.dashboard {
                roster.inactive(
                    instance,
                    Trust::Bundled,
                    &loaded,
                    super::dashboard::DISABLED_REASON,
                );
                logger.info(format!(
                    "Dashboard is off: {} is inactive ({}); set ENABLE_DASHBOARD = true in vosu.toml, leave {} unset and restart vosu to turn it on",
                    bundle.dashboard,
                    super::dashboard::DISABLED_REASON,
                    super::dashboard::ENV,
                ));
                return Ok(());
            }
            let created = host
                .create_instance(
                    instance,
                    &loaded,
                    paths.bundled_plugin_data_dir(&loaded.manifest().id),
                    Some(Arc::clone(pool)),
                )
                .map_err(std::io::Error::other);
            let reported = created.as_ref().map(|_| ()).map_err(ToString::to_string);
            roster.loaded(instance, Trust::Bundled, None, &loaded, reported);
            created
        });
        if let Err(error) = result {
            if let Some(required) = bundle.required.get(index) {
                return Err(std::io::Error::other(format!(
                    "required bundled plugin {required} failed: {error}"
                )));
            }
            logger.warn(format!(
                "optional bundled plugin at index {index} failed: {error}"
            ));
            if !roster.has_instance(instance) {
                roster.refused(instance, Trust::Bundled, None, error.to_string());
            }
        }
    }
    Ok(bundled.len() as u64)
}

/// Installs the approval-gated process launcher once. `false` when this
/// executable cannot start plugin hosts; process packages then fail to
/// start, and the daemon keeps running.
fn install_gate(
    host: &PluginHost,
    store: &Arc<ApprovalStore>,
    logger: &crate::logging::Logger,
    identity: &str,
) -> bool {
    let installed = ProcessHostLauncher::start(identity)
        .map_err(HostError::Io)
        .and_then(|launcher| {
            host.install_process_launcher(Arc::new(ApprovalGate::new(Arc::clone(store), launcher)))
        });
    if let Err(error) = &installed {
        logger.warn(format!("process plugins are unavailable: {error}"));
    }
    installed.is_ok()
}

/// Loads and registers every `--plugin <dir>` directory with `host`, in
/// order, numbering instances starting at `id_offset` (past whatever
/// [`load_bundled`] already assigned) so the two groups' ids never
/// collide. A directory that fails to load or start is logged and listed,
/// never fatal: a development plugin must not take the daemon down.
#[expect(
    clippy::too_many_arguments,
    reason = "explicit startup composition inputs, numbered after the bundled set"
)]
pub(super) fn load_development(
    bundle: &Bundle,
    host: &Arc<PluginHost>,
    dirs: Vec<PathBuf>,
    id_offset: u64,
    logger: &crate::logging::Logger,
    pool: &Arc<WorkerPool>,
    store: &Arc<ApprovalStore>,
    roster: &mut Roster,
) {
    let debug_build = cfg!(debug_assertions);
    let version = bundle.version();
    let mut gate = None;
    for (index, dir) in dirs.iter().enumerate() {
        let instance = PluginInstanceId::new(id_offset + index as u64);
        let loaded = plugin::load_development(
            &version,
            &source_snapshot(host, &[], &dirs),
            dir,
            debug_build,
        );
        let loaded = match loaded {
            Ok(loaded) if roster.contains(&loaded.manifest().id) => {
                let error = format!("plugin id {} is already loaded", loaded.manifest().id);
                logger.warn(format!("--plugin {}: {error}", dir.display()));
                roster.refused(instance, Trust::User, Some(dir.clone()), error);
                continue;
            }
            Ok(loaded) => loaded,
            Err(error) => {
                logger.warn(format!("--plugin {}: {error}", dir.display()));
                roster.refused(instance, Trust::User, Some(dir.clone()), error.to_string());
                continue;
            }
        };
        if matches!(loaded.execution(), Execution::Process(_)) {
            gate.get_or_insert_with(|| {
                install_gate(host, store, logger, bundle.plugin_host_identity)
            });
        }
        let created = host.create_instance(instance, &loaded, dir.clone(), Some(Arc::clone(pool)));
        match &created {
            // The gate already logged that approval needs the dashboard.
            Err(HostError::ProcessStartupFailed(failure))
                if failure.stage == StartupStage::Approval => {}
            Err(error) => logger.warn(format!(
                "--plugin {}: create failed: {error}",
                dir.display()
            )),
            Ok(()) => {}
        }
        roster.loaded(
            instance,
            Trust::development(loaded.manifest().runtime),
            Some(dir.clone()),
            &loaded,
            created.map_err(|error| error.to_string()),
        );
    }
}

/// Loads every bundled plugin, then every `--plugin <dir>` development
/// plugin, as one combined, uniquely-numbered set registered with `host`
/// (`lifecycle::ProductionRuntime::plugins`), and returns the dashboard's
/// management of that set. Approvals live under `paths.config()`.
/// `dashboard` is the A7.3 switch `run_cli` resolved for this run.
pub(super) fn load_all(
    bundle: &Bundle,
    host: &Arc<PluginHost>,
    dev_dirs: Vec<PathBuf>,
    paths: &Paths,
    logger: &crate::logging::Logger,
    dashboard: bool,
) -> std::io::Result<PluginManagement> {
    let pool = WorkerPool::new(PLUGIN_CONCURRENT_WORKERS, DEFAULT_QUEUE_CAPACITY);
    let mut roster = Roster::default();
    let offset = load_bundled(
        bundle,
        host,
        paths,
        logger,
        &pool,
        bundle.plugins,
        &dev_dirs,
        &mut roster,
        dashboard,
    )?;
    let store = Arc::new(ApprovalStore::open(paths.config()));
    load_development(
        bundle, host, dev_dirs, offset, logger, &pool, &store, &mut roster,
    );
    Ok(PluginManagement::new(Arc::clone(host), store, roster))
}

/// Rebuild registered source identities directly from the currently enabled
/// inputs. Pending providers are known namespaces, never live capability grants.
pub(super) fn source_snapshot(
    host: &PluginHost,
    bundled: &[plugin::BundledPlugin<'_>],
    development_dirs: &[PathBuf],
) -> plugin::ValidationSnapshot {
    use vosu_plugin_api::manifest::PackageManifest;
    let mut snapshot = host.validation_snapshot();
    let mut register = |text: &str, builtin: bool| {
        if let Ok(PackageManifest::Plugin(manifest)) = PackageManifest::parse(text) {
            snapshot.providers.push(plugin::ProviderRegistration {
                id: manifest.id,
                official: builtin,
                builtin,
                replaces: manifest.replaces,
                // Only loaded/compiled schemas attest authoritative revisions.
                revision: None,
            });
        }
    };
    for entry in bundled {
        register(entry.manifest, true);
    }
    for directory in development_dirs {
        if let Ok(text) = std::fs::read_to_string(directory.join("manifest.toml")) {
            register(&text, false);
        }
    }
    snapshot
}

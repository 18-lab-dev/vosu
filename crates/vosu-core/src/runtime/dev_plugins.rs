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
//! A process package from either origin starts only through the shared
//! approval gate (`vosu_core::plugin::approval::ApprovalGate`).
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
    self, DEFAULT_QUEUE_CAPACITY, HostError, PluginHost, PluginInstanceId, WorkerPool,
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

const OFFICIAL_FIXTURE_ENV: &str = "VOSU_TEST_OFFICIAL_PLUGIN";

#[derive(Default)]
pub(super) struct StartupPlugins {
    development: Vec<PathBuf>,
    official_fixtures: Vec<(PathBuf, plugin::ProviderRegistration)>,
}

impl StartupPlugins {
    pub(super) fn parse(bundle: &Bundle, args: &[std::ffi::OsString]) -> std::io::Result<Self> {
        let paths = official_fixture_paths(
            std::env::var_os(OFFICIAL_FIXTURE_ENV),
            cfg!(debug_assertions),
        )?;
        let mut official_fixtures =
            Vec::<(_, plugin::ProviderRegistration)>::with_capacity(paths.len());
        for path in paths {
            let registration = fixture_registration(&path, bundle.plugin_ids)?;
            if official_fixtures
                .iter()
                .any(|(_, entry)| entry.id == registration.id)
            {
                return Err(std::io::Error::other(format!(
                    "duplicate fixture-official plugin `{}`",
                    registration.id
                )));
            }
            official_fixtures.push((path, registration));
        }
        Ok(Self {
            official_fixtures,
            development: plugin::development_roots(args)?,
        })
    }
}

fn official_fixture_paths(
    value: Option<std::ffi::OsString>,
    debug_build: bool,
) -> std::io::Result<Vec<PathBuf>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if !(debug_build && cfg!(feature = "test-harness")) {
        return Err(std::io::Error::other(format!(
            "{OFFICIAL_FIXTURE_ENV} requires a debug build with test-harness"
        )));
    }
    // Unlike a delimited list, JSON is unambiguous even for paths containing
    // ':' or ';'. A single native path still works without Unicode conversion.
    let paths = match value.to_str().filter(|value| value.starts_with('[')) {
        Some(list) => serde_json::from_str::<Vec<PathBuf>>(list)
            .map_err(|error| std::io::Error::other(format!("{OFFICIAL_FIXTURE_ENV}: {error}")))?,
        None => vec![PathBuf::from(value)],
    };
    if paths.is_empty() || paths.iter().any(|path| !path.is_absolute()) {
        return Err(std::io::Error::other(format!(
            "{OFFICIAL_FIXTURE_ENV} requires an absolute package directory or a nonempty JSON array of them"
        )));
    }
    Ok(paths)
}

fn fixture_registration(
    path: &std::path::Path,
    bundled_ids: &[&str],
) -> std::io::Result<plugin::ProviderRegistration> {
    use vosu_plugin_api::manifest::PackageManifest;
    let text = std::fs::read_to_string(path.join("manifest.toml"))?;
    let package = PackageManifest::parse(&text).map_err(std::io::Error::other)?;
    let PackageManifest::Plugin(manifest) = package else {
        return Err(std::io::Error::other(
            "fixture-official requires a plugin manifest",
        ));
    };
    if bundled_ids.contains(&manifest.id.as_str()) {
        return Err(std::io::Error::other(format!(
            "fixture-official cannot shadow bundled plugin `{}`",
            manifest.id
        )));
    }
    Ok(plugin::ProviderRegistration {
        id: manifest.id,
        official: true,
        builtin: false,
        replaces: manifest.replaces,
        // Only loaded/compiled schemas attest authoritative revisions.
        revision: None,
    })
}

/// Checks the complete ordered inventory before creating any instance.
/// Array positions are stable even for the inactive dashboard.
fn validate_bundled_ids(
    bundle: &Bundle,
    bundled: &[plugin::BundledPlugin<'_>],
) -> std::io::Result<()> {
    use vosu_plugin_api::manifest::PackageManifest;

    if bundled.len() != bundle.plugin_ids.len() {
        return Err(std::io::Error::other(format!(
            "bundled registry has {} entries; expected {} ({})",
            bundled.len(),
            bundle.plugin_ids.len(),
            bundle.plugin_ids.join(", ")
        )));
    }
    for (index, (entry, &expected)) in bundled.iter().zip(bundle.plugin_ids).enumerate() {
        let package = PackageManifest::parse(entry.manifest)
            .and_then(PackageManifest::into_plugin)
            .map_err(|error| {
                std::io::Error::other(format!(
                    "required bundled plugin {expected} failed: {error}"
                ))
            })?;
        let id = package.id;
        if bundle.plugin_ids[..index].contains(&id.as_str()) {
            return Err(std::io::Error::other(format!(
                "duplicate bundled plugin {id}"
            )));
        }
        if id != expected {
            return Err(std::io::Error::other(format!(
                "bundled plugin at index {index} is {id}; expected {expected}"
            )));
        }
    }
    Ok(())
}

/// Loads exactly the executable's bundled inventory. A malformed manifest,
/// unexpected identity or load/create failure aborts startup, including for the
/// echo fixture when the executable includes it.
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
    validate_bundled_ids(bundle, bundled)?;
    let version = bundle.version();
    for (index, (entry, &expected)) in bundled.iter().zip(bundle.plugin_ids).enumerate() {
        let instance = PluginInstanceId::new(index as u64);
        let required_error = |error| {
            std::io::Error::other(format!(
                "required bundled plugin {expected} failed: {error}"
            ))
        };
        let loaded = plugin::load_bundled(
            &version,
            &source_snapshot(host, bundled, development_dirs),
            std::slice::from_ref(entry),
        )
        .map_err(required_error)?
        .remove(0);
        if !dashboard && expected == bundle.dashboard {
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
            continue;
        }
        host.create_instance(
            instance,
            &loaded,
            paths.bundled_plugin_data_dir(expected),
            Some(Arc::clone(pool)),
        )
        .map_err(required_error)?;
        roster.loaded(instance, Trust::Bundled, None, &loaded, Ok(()));
    }
    Ok(bundled.len() as u64)
}

/// Installs the shared approval-gated launcher for both plugin origins.
fn install_gate(
    host: &PluginHost,
    store: &Arc<ApprovalStore>,
    logger: &crate::logging::Logger,
    identity: &str,
) {
    let installed = ProcessHostLauncher::start(identity)
        .map_err(HostError::Io)
        .and_then(|launcher| {
            host.install_process_launcher(Arc::new(ApprovalGate::new(Arc::clone(store), launcher)))
        });
    if let Err(error) = &installed {
        logger.warn(format!("process plugins are unavailable: {error}"));
    }
}

/// Loads and registers every `--plugin <dir>` directory with `host`, in
/// order, numbering instances starting at `id_offset` (past whatever
/// [`load_bundled`] already assigned) so the two groups' ids never
/// collide. A directory that fails to load or start is logged and listed,
/// never fatal: a development plugin must not take the daemon down.
pub(super) fn load_development(
    bundle: &Bundle,
    host: &Arc<PluginHost>,
    inputs: StartupPlugins,
    id_offset: u64,
    logger: &crate::logging::Logger,
    pool: &Arc<WorkerPool>,
    roster: &mut Roster,
) {
    let debug_build = cfg!(debug_assertions);
    let version = bundle.version();
    let dirs = inputs.development;
    for (index, (dir, official)) in dirs
        .iter()
        .map(|dir| (dir, false))
        .chain(inputs.official_fixtures.iter().map(|(dir, _)| (dir, true)))
        .enumerate()
    {
        let instance = PluginInstanceId::new(id_offset + index as u64);
        let mut snapshot = source_snapshot(host, &[], &dirs);
        // Pending fixtures reserve namespace identities, not capabilities.
        snapshot.providers.extend(
            inputs
                .official_fixtures
                .iter()
                .map(|(_, registration)| registration.clone()),
        );
        let (label, trust, loaded) = if official {
            #[cfg(feature = "test-harness")]
            {
                (
                    "fixture-official",
                    Trust::FixtureOfficial,
                    plugin::load_official_test_fixture(&version, &snapshot, dir, debug_build),
                )
            }
            #[cfg(not(feature = "test-harness"))]
            unreachable!("the official fixture input is refused before startup")
        } else {
            (
                "--plugin",
                Trust::User,
                plugin::load_development(&version, &snapshot, dir, debug_build),
            )
        };
        let loaded = match loaded {
            Ok(loaded) if roster.contains(&loaded.manifest().id) => {
                let error = format!("plugin id {} is already loaded", loaded.manifest().id);
                logger.warn(format!("{label} {}: {error}", dir.display()));
                roster.refused(instance, trust, Some(dir.clone()), error);
                continue;
            }
            Ok(loaded) => loaded,
            Err(error) => {
                logger.warn(format!("{label} {}: {error}", dir.display()));
                roster.refused(instance, trust, Some(dir.clone()), error.to_string());
                continue;
            }
        };
        let created = host.create_instance(instance, &loaded, dir.clone(), Some(Arc::clone(pool)));
        match &created {
            // The gate already logged that approval needs the dashboard.
            Err(HostError::ProcessStartupFailed(failure))
                if failure.stage == StartupStage::Approval => {}
            Err(error) => logger.warn(format!("{label} {}: create failed: {error}", dir.display())),
            Ok(()) => {}
        }
        roster.loaded(
            instance,
            if official {
                trust
            } else {
                Trust::development(loaded.manifest().runtime)
            },
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
    inputs: StartupPlugins,
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
        &inputs.development,
        &mut roster,
        dashboard,
    )?;
    let store = Arc::new(ApprovalStore::open(paths.config()));
    install_gate(host, &store, logger, bundle.plugin_host_identity);
    load_development(bundle, host, inputs, offset, logger, &pool, &mut roster);
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

#[cfg(test)]
mod official_switch_tests {
    use super::*;

    #[test]
    fn official_fixture_switch_requires_debug_and_harness() {
        let path = std::env::current_dir().unwrap().join("fixture space");
        let other = path.with_file_name("other");
        let list = serde_json::to_string(&[&path, &other]).unwrap();
        assert!(official_fixture_paths(None, false).unwrap().is_empty());
        assert!(official_fixture_paths(Some(path.clone().into()), false).is_err());
        assert!(official_fixture_paths(Some(list.clone().into()), false).is_err());
        let debug = official_fixture_paths(Some(path.clone().into()), true);
        let plural = official_fixture_paths(Some(list.into()), true);
        if cfg!(feature = "test-harness") {
            assert_eq!(debug.unwrap().as_slice(), std::slice::from_ref(&path));
            assert_eq!(plural.unwrap(), [path, other]);
            for invalid in ["relative", "[]", "[", "[\"relative\"]"] {
                assert!(official_fixture_paths(Some(invalid.into()), true).is_err());
            }
        } else {
            assert!(debug.is_err());
            assert!(plural.is_err());
        }
    }
}

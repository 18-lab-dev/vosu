//! Plugins loaded in-process at startup: the explicit `plugins::BUNDLED`
//! array (D13, `docs/v2/plugin-abi.md` §2, §7.1 — granted `in-process`
//! unconditionally) and `--plugin <dir>` development plugins
//! (`docs/v2/plugin-abi.md` §7.1, the `--tool <dir>` analog
//! `crate::tools::development_roots`), trusted only in debug builds. Every
//! instance this module creates is destroyed with a grace period at
//! shutdown (`lifecycle::ProductionRuntime::shutdown`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use vosu_core::plugin::{self, PluginHostHandler, PluginInstance, PluginInstanceId};
use vosu_plugin_api::{CborValue, Init, Message, encode, vosu_status};
use vosu_sys::platform::Paths;

/// Matches `src/server/runtime.rs::DRAIN_TIMEOUT` and the tool-process
/// 5-second wait in `src/tools/process.rs`: the same shutdown-grace
/// precedent `docs/v2/plugin-abi.md` §4 cites for `destroy`.
const PLUGIN_DESTROY_GRACE: Duration = Duration::from_secs(5);

/// Logs every plugin -> host message instead of acting on it: session
/// lifecycle, state publication, and every other host service are later
/// milestones (out of scope here).
struct LoggingHandler;
impl PluginHostHandler for LoggingHandler {
    fn handle(
        &self,
        instance: PluginInstanceId,
        message: Message,
        correlation_id: u64,
    ) -> vosu_status {
        tracing::debug!(
            ?instance,
            opcode = ?message.opcode(),
            correlation_id,
            "plugin sent a host message"
        );
        vosu_status::VOSU_OK
    }
}

/// A bundled plugin instance has no directory of its own on disk (unlike a
/// `--plugin <dir>` development plugin): its data directory lives under the
/// data root, named by its plugin id.
fn bundled_data_dir(paths: &Paths, plugin_id: &str) -> PathBuf {
    paths.data().join("plugins").join(plugin_id)
}

/// Loads `plugins::BUNDLED` (D13) through the host's registry and `create`s
/// an instance for each entry, in array order. §7.1 grants `in-process`
/// unconditionally to the explicit `BUNDLED` array, so unlike a development
/// plugin there is no trust refusal to handle here — only `create` itself
/// failing, which is logged and skipped rather than failing startup.
pub(super) fn load_bundled(
    paths: &Paths,
    logger: &vosu_core::logging::Logger,
) -> Vec<PluginInstance> {
    let loaded = match plugin::load_bundled(crate::plugins::BUNDLED) {
        Ok(loaded) => loaded,
        Err(error) => {
            logger.warn(format!("bundled plugin registry failed to load: {error}"));
            return Vec::new();
        }
    };
    let mut instances = Vec::with_capacity(loaded.len());
    for (index, loaded) in loaded.iter().enumerate() {
        let plugin_id = &loaded.manifest().id;
        let init = match encode(&Init {
            plugin_id: plugin_id.clone(),
            settings: CborValue::Map(Vec::new()),
            data_dir: bundled_data_dir(paths, plugin_id),
        }) {
            Ok(init) => init,
            Err(error) => {
                logger.warn(format!(
                    "bundled plugin {plugin_id:?}: encoding init failed: {error}"
                ));
                continue;
            }
        };
        match PluginInstance::create(
            PluginInstanceId::new(index as u64),
            loaded,
            &init,
            Arc::new(LoggingHandler),
            None,
        ) {
            Ok(instance) => instances.push(instance),
            Err(error) => logger.warn(format!(
                "bundled plugin {plugin_id:?}: create failed: {error}"
            )),
        }
    }
    instances
}

/// Loads and `create`s every `--plugin <dir>` directory, in order,
/// numbering instances starting at `id_offset` (past whatever
/// [`load_bundled`] already assigned) so the two groups' ids never
/// collide. A directory that fails to load is logged and skipped rather
/// than failing startup: a development flag misusing one plugin should not
/// take the whole daemon down.
pub(super) fn load_development(
    dirs: Vec<PathBuf>,
    id_offset: u64,
    logger: &vosu_core::logging::Logger,
) -> Vec<PluginInstance> {
    let debug_build = cfg!(debug_assertions);
    let mut instances = Vec::with_capacity(dirs.len());
    for (index, dir) in dirs.into_iter().enumerate() {
        let loaded = match plugin::load_development(&dir, debug_build) {
            Ok(loaded) => loaded,
            Err(error) => {
                logger.warn(format!("--plugin {}: {error}", dir.display()));
                continue;
            }
        };
        let init = match encode(&Init {
            plugin_id: loaded.manifest().id.clone(),
            settings: CborValue::Map(Vec::new()),
            data_dir: dir.clone(),
        }) {
            Ok(init) => init,
            Err(error) => {
                logger.warn(format!(
                    "--plugin {}: encoding init failed: {error}",
                    dir.display()
                ));
                continue;
            }
        };
        match PluginInstance::create(
            PluginInstanceId::new(id_offset + index as u64),
            &loaded,
            &init,
            Arc::new(LoggingHandler),
            None,
        ) {
            Ok(instance) => instances.push(instance),
            Err(error) => logger.warn(format!(
                "--plugin {}: create failed: {error}",
                dir.display()
            )),
        }
    }
    instances
}

/// Loads every bundled plugin, then every `--plugin <dir>` development
/// plugin, as one combined, uniquely-numbered set
/// (`ProductionRuntime::plugins`).
pub(super) fn load_all(
    dev_dirs: Vec<PathBuf>,
    paths: &Paths,
    logger: &vosu_core::logging::Logger,
) -> Vec<PluginInstance> {
    let mut instances = load_bundled(paths, logger);
    let offset = instances.len() as u64;
    instances.extend(load_development(dev_dirs, offset, logger));
    instances
}

/// Destroys every loaded plugin instance, each with its own grace period
/// (§4): one plugin's hang does not extend another's.
pub(super) fn destroy_all(instances: Vec<PluginInstance>) {
    for instance in instances {
        if let Err(error) = instance.destroy(PLUGIN_DESTROY_GRACE) {
            tracing::warn!(%error, "plugin did not destroy within its grace period");
        }
    }
}

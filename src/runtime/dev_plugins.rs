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

use vosu_core::plugin::{self, DEFAULT_QUEUE_CAPACITY, PluginHost, PluginInstanceId, WorkerPool};
use vosu_sys::platform::Paths;

/// Matches `src/server/runtime.rs::DRAIN_TIMEOUT` and the tool-process
/// 5-second wait in `src/tools/process.rs`: the same shutdown-grace
/// precedent `docs/v2/plugin-abi.md` §4 cites for `destroy`.
pub(super) const PLUGIN_DESTROY_GRACE: Duration = Duration::from_secs(5);

/// Worker count for the shared `concurrent_dispatch` pool every loaded
/// instance that opts in shares (`docs/v2/plugin-abi.md` §4): a small
/// fixed-size pool, not one thread per concurrent instance, since sharing
/// it across instances is the whole point of `concurrent_dispatch`.
const PLUGIN_CONCURRENT_WORKERS: usize = 4;

/// Loads the production-required osu! entry first, followed by optional bundled
/// fixtures. Required-plugin failure aborts composition; optional fixtures keep
/// warn-and-continue behavior. Array positions remain reserved even on failure.
pub(super) fn load_bundled(
    host: &Arc<PluginHost>,
    paths: &Paths,
    logger: &vosu_core::logging::Logger,
    pool: &Arc<WorkerPool>,
    bundled: &[plugin::BundledPlugin<'_>],
) -> std::io::Result<u64> {
    if bundled.is_empty() {
        return Err(std::io::Error::other(
            "required bundled plugin vosu.osu is missing",
        ));
    }
    for (index, entry) in bundled.iter().enumerate() {
        // BUNDLED's first entry is the production-required plugin. Check the
        // identity after parsing too, so omission/reordering cannot half-start.
        let result = (|| {
            let loaded =
                plugin::load_bundled(std::slice::from_ref(entry)).map_err(std::io::Error::other)?;
            let loaded = &loaded[0];
            let plugin_id = &loaded.manifest().id;
            if index == 0 && plugin_id != "vosu.osu" {
                return Err(std::io::Error::other(
                    "required bundled plugin vosu.osu is missing",
                ));
            }
            host.create_instance(
                PluginInstanceId::new(index as u64),
                loaded,
                paths.bundled_plugin_data_dir(plugin_id),
                Some(Arc::clone(pool)),
            )
            .map_err(std::io::Error::other)
        })();
        if let Err(error) = result {
            if index == 0 {
                return Err(std::io::Error::other(format!(
                    "required bundled plugin vosu.osu failed: {error}"
                )));
            }
            logger.warn(format!(
                "optional bundled plugin at index {index} failed: {error}"
            ));
        }
    }
    Ok(bundled.len() as u64)
}

/// Loads and registers every `--plugin <dir>` directory with `host`, in
/// order, numbering instances starting at `id_offset` (past whatever
/// [`load_bundled`] already assigned) so the two groups' ids never
/// collide. A directory that fails to load is logged and skipped rather
/// than failing startup: a development flag misusing one plugin should not
/// take the whole daemon down.
pub(super) fn load_development(
    host: &Arc<PluginHost>,
    dirs: Vec<PathBuf>,
    id_offset: u64,
    logger: &vosu_core::logging::Logger,
    pool: &Arc<WorkerPool>,
) {
    let debug_build = cfg!(debug_assertions);
    for (index, dir) in dirs.into_iter().enumerate() {
        let loaded = match plugin::load_development(&dir, debug_build) {
            Ok(loaded) => loaded,
            Err(error) => {
                logger.warn(format!("--plugin {}: {error}", dir.display()));
                continue;
            }
        };
        if let Err(error) = host.create_instance(
            PluginInstanceId::new(id_offset + index as u64),
            &loaded,
            dir.clone(),
            Some(Arc::clone(pool)),
        ) {
            logger.warn(format!(
                "--plugin {}: create failed: {error}",
                dir.display()
            ));
        }
    }
}

/// Loads every bundled plugin, then every `--plugin <dir>` development
/// plugin, as one combined, uniquely-numbered set registered with `host`
/// (`lifecycle::ProductionRuntime::plugins`).
pub(super) fn load_all(
    host: &Arc<PluginHost>,
    dev_dirs: Vec<PathBuf>,
    paths: &Paths,
    logger: &vosu_core::logging::Logger,
) -> std::io::Result<()> {
    let pool = WorkerPool::new(PLUGIN_CONCURRENT_WORKERS, DEFAULT_QUEUE_CAPACITY);
    let offset = load_bundled(host, paths, logger, &pool, crate::plugins::BUNDLED)?;
    load_development(host, dev_dirs, offset, logger, &pool);
    Ok(())
}

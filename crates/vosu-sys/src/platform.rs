//! Target-specific filesystem, system, and process integration.

mod channel;
#[cfg(windows)]
mod console;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;
pub use channel::BuildChannel;
#[cfg(windows)]
pub use console::{detach_private_console, open_log_console};

/// Stable Windows volume/file identity for an open file, including across renames.
#[cfg(windows)]
pub fn file_identity(file: &std::fs::File) -> io::Result<(u64, u64)> {
    windows::file_identity(file)
}

use std::ffi::OsString;
#[cfg(not(windows))]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Supplies process path state without requiring callers or tests to mutate
/// process-global environment variables.
pub trait PathResolver {
    fn current_exe(&self) -> io::Result<PathBuf>;
    fn launch_dir(&self) -> io::Result<PathBuf>;
    fn env_var(&self, name: &str) -> Option<OsString>;
}

/// Resolver backed by the current process.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPathResolver;

impl PathResolver for SystemPathResolver {
    fn current_exe(&self) -> io::Result<PathBuf> {
        std::env::current_exe()
    }

    fn launch_dir(&self) -> io::Result<PathBuf> {
        std::env::current_dir()
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

/// Resolved application roots.
///
/// Resolution is pure. Call [`Paths::initialize`] explicitly to create roots
/// and, for release builds only, migrate legacy directories.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paths {
    program: PathBuf,
    launch: PathBuf,
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    channel: BuildChannel,
}

impl Paths {
    pub fn resolve(resolver: &impl PathResolver, channel: BuildChannel) -> io::Result<Self> {
        let executable = resolver.current_exe()?;
        let program = executable.parent().map(Path::to_path_buf).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "current executable has no parent directory",
            )
        })?;
        let launch = resolver.launch_dir()?;
        let (config, data, cache) = platform_roots(resolver, &program)?;
        Ok(Self {
            program,
            launch,
            config: channel.root(config),
            data: channel.root(data),
            cache: channel.root(cache),
            channel,
        })
    }

    /// Creates selected roots; prerelease initialization never migrates normal state.
    pub fn initialize(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if self.channel == BuildChannel::Release {
            for target in [&self.config, &self.data, &self.cache] {
                migrate_legacy_directory(&target.with_file_name("tosu"), target)?;
            }
        }
        for directory in [&self.config, &self.data, &self.cache] {
            std::fs::create_dir_all(directory)?;
        }
        #[cfg(windows)]
        windows::cleanup_stale_replacements(&self.config);
        Ok(())
    }

    pub fn system(channel: BuildChannel) -> io::Result<Self> {
        Self::resolve(&SystemPathResolver, channel)
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    pub fn launch(&self) -> &Path {
        &self.launch
    }

    pub fn config(&self) -> &Path {
        &self.config
    }

    /// Local native-broker credential location (not the Windows program folder).
    pub fn broker_path(&self) -> io::Result<PathBuf> {
        self.broker_path_with(&SystemPathResolver)
    }

    pub fn broker_path_with(&self, resolver: &impl PathResolver) -> io::Result<PathBuf> {
        #[cfg(target_os = "linux")]
        let root = nonempty_env(resolver, "XDG_RUNTIME_DIR")
            .map(|root| self.channel.root(root.join("vosu")))
            .unwrap_or_else(|| self.config.clone());
        #[cfg(windows)]
        let root = nonempty_env(resolver, "LOCALAPPDATA")
            .map(|root| self.channel.root(root.join("vosu")))
            .ok_or_else(|| io::Error::other("LOCALAPPDATA is unavailable"))?;
        if !root.is_absolute() {
            return Err(io::Error::other("broker directory must be absolute"));
        }
        Ok(root.join("broker.json"))
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    /// Data directory passed to a bundled plugin, named by its manifest id.
    pub fn bundled_plugin_data_dir(&self, plugin_id: &str) -> PathBuf {
        self.data.join("plugins").join(plugin_id)
    }

    pub fn cache(&self) -> &Path {
        &self.cache
    }

    /// Resolves and creates the configured static directory.
    ///
    /// Empty values and the two source defaults map to the platform data root.
    /// Every other relative value remains relative to the launch directory.
    pub fn static_path(&self, configured: &str) -> io::Result<PathBuf> {
        let path = if configured.is_empty()
            || configured.eq_ignore_ascii_case("./static")
            || configured.eq_ignore_ascii_case(".\\static")
        {
            self.data.join("static")
        } else {
            let configured = Path::new(configured);
            if configured.is_absolute() {
                configured.to_path_buf()
            } else {
                self.launch.join(configured)
            }
        };

        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Resolves the authoritative per-overlay values TOML path.
    ///
    /// Folder names are otherwise left untouched for source compatibility.
    pub fn settings_path(&self, folder: &str) -> io::Result<Option<PathBuf>> {
        if folder.is_empty() {
            return Ok(None);
        }

        let settings = self.config.join("settings");
        std::fs::create_dir_all(&settings)?;
        Ok(Some(
            settings.join(format!("{folder}.settings.values.toml")),
        ))
    }
}

fn nonempty_env(resolver: &impl PathResolver, name: &str) -> Option<PathBuf> {
    resolver
        .env_var(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "linux")]
pub fn install_supported_capabilities(executable: &Path) -> io::Result<()> {
    linux::install_supported_capabilities(executable)
}

#[cfg(target_os = "linux")]
pub fn cap_sys_ptrace_warning(executable: &Path) -> Option<String> {
    cap_sys_ptrace_warning_for(linux::cap_sys_ptrace_effective(), executable)
}

#[cfg(target_os = "linux")]
fn cap_sys_ptrace_warning_for(capability: io::Result<bool>, executable: &Path) -> Option<String> {
    match capability {
        Ok(true) => None,
        Ok(false) => Some(format!(
            "CAP_SYS_PTRACE is not effective; memory reads may fail. Install this build's required capabilities with `vosu --setup`; setup requests operating-system authentication when needed; current executable: {executable:?}."
        )),
        Err(error) => Some(format!(
            "Could not inspect this process's effective Linux capabilities; memory reads may fail: {error}"
        )),
    }
}
#[cfg(target_os = "linux")]
fn platform_roots(
    resolver: &impl PathResolver,
    program: &Path,
) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
    linux::platform_roots(resolver, program)
}

#[cfg(windows)]
fn platform_roots(
    resolver: &impl PathResolver,
    program: &Path,
) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
    windows::platform_roots(resolver, program)
}

/// Replaces `target` with a fully written same-filesystem temporary file.
///
/// The target is left untouched whenever replacement fails. On Windows this
/// cross-directory `FILE_RENAME_POSIX_SEMANTICS` publish is atomic from the
/// writer's perspective (`writer=Ok(())` only once the name is durably
/// replaced) but is not fully atomic from a concurrent reader's: measured
/// evidence (`windows::publish_file` doc comment, 2026-11-23) found 0 missing-name
/// observations across 320k+ concurrent reads at normal load, but 674 transient
/// `NotFound`s in one in ten runs under extreme contention (16 build jobs plus a
/// second concurrent full build), each within 0.8-40ms of the rename call.
/// [`read_published`]/[`read_published_async`] absorb that window for vosu's
/// own readers; this function itself makes no concurrent-reader guarantee.
pub fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::atomic_replace(temp, target)
    }
    #[cfg(windows)]
    {
        windows::atomic_replace(temp, target)
    }
}

/// Reads a file previously published through [`atomic_replace`].
///
/// On Windows, a `NotFound` result triggers up to 3 reads total (2 retries)
/// spread over roughly 50ms: the measured evidence cited on
/// [`atomic_replace`] shows every observed transient miss resolved within
/// 40ms of the publishing rename. A file that is genuinely absent (e.g.
/// first run, before anything has published it) still surfaces `NotFound`,
/// just after that bounded extra latency. Non-Windows platforms read
/// directly: [`atomic_replace`] there is a same-directory `rename()`, which
/// POSIX never desyncs the name under a concurrent reader.
///
/// This is a blocking call; async callers should use
/// [`read_published_async`] instead.
///
/// ## Which readers go through this helper
///
/// Every vosu read of a file published through [`atomic_replace`] either
/// goes through [`read_published`]/[`read_published_async`], or is one of
/// the documented exclusions below:
/// - **Same-task reads strictly after that task's own publish**: the
///   reading task is the only writer of that specific path for the
///   duration of the read, so there is no concurrent-rename window to
///   absorb. E.g. `updater/prepare.rs`'s `digest_file_bytes_cancelled`,
///   `validate_archive_shape`, `extract_staged_binary_with_limit` (stream
///   the just-downloaded private staging file right after
///   `download_atomic`/`atomic_replace` returned); `updater/release.rs`'s
///   release-metadata read (same pattern); `counters/service.rs`,
///   `tools/catalog.rs`, `tools/catalog_sources.rs` (extract/verify a
///   just-downloaded archive right after their own `download_atomic` call).
/// - **Not `atomic_replace`-published**: files written through tempfile
///   `persist` (`MoveFileExW`, not this crate's cross-directory
///   `FILE_RENAME_POSIX_SEMANTICS` publish) are out of scope for this
///   helper's measured guarantee. E.g. tool settings and `state.json`
///   (`tools/mod.rs`'s `atomic_json`), catalog sources and
///   `overlay-catalog-state.json` (`tools/catalog_sources.rs`'s
///   `atomic_json`).
/// - **Serialized against their own writer instead**: `counters/settings.rs`
///   reads `profiles.json`, a counter's schema/values file, and a profile's
///   values file only while holding its `PROFILE_MUTATIONS` lock, which
///   every in-process writer of those same files already holds while it
///   publishes through `atomic_bytes`/`atomic_replace`. vosu's daemon is the
///   only writer of these files, so holding that lock for the read rules
///   out observing a concurrent publish, with no retry needed.
pub fn read_published(path: &Path) -> io::Result<Vec<u8>> {
    #[cfg(windows)]
    {
        windows::read_published(path)
    }
    #[cfg(not(windows))]
    {
        fs::read(path)
    }
}

/// Async equivalent of [`read_published`] for callers already on a Tokio
/// runtime: the Windows retry delay uses `tokio::time::sleep` instead of
/// blocking the calling thread.
pub async fn read_published_async(path: &Path) -> io::Result<Vec<u8>> {
    #[cfg(windows)]
    {
        windows::read_published_async(path).await
    }
    #[cfg(not(windows))]
    {
        tokio::fs::read(path).await
    }
}

#[cfg(target_os = "linux")]
fn migrate_legacy_directory(legacy: &Path, target: &Path) -> io::Result<()> {
    if !legacy.exists() {
        return Ok(());
    }

    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(legacy)? {
        let entry = entry?;
        let source = entry.path();
        let destination = target.join(entry.file_name());
        if destination.exists() {
            if source.is_dir() && destination.is_dir() {
                migrate_legacy_directory(&source, &destination)?;
            } else if source.is_dir() {
                std::fs::remove_dir_all(&source)?;
            } else {
                std::fs::remove_file(&source)?;
            }
        } else {
            std::fs::rename(&source, &destination)?;
        }
    }
    std::fs::remove_dir(legacy)
}
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{BuildChannel, PathResolver, Paths, cap_sys_ptrace_warning_for};
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::io;
    use std::path::PathBuf;

    #[derive(Clone)]
    struct InjectedResolver {
        executable: PathBuf,
        launch: PathBuf,
        env: BTreeMap<String, OsString>,
    }

    impl InjectedResolver {
        fn new(executable: PathBuf, launch: PathBuf) -> Self {
            Self {
                executable,
                launch,
                env: BTreeMap::new(),
            }
        }

        fn with_env(mut self, name: &str, value: impl Into<OsString>) -> Self {
            self.env.insert(name.to_owned(), value.into());
            self
        }
    }

    impl PathResolver for InjectedResolver {
        fn current_exe(&self) -> io::Result<PathBuf> {
            Ok(self.executable.clone())
        }

        fn launch_dir(&self) -> io::Result<PathBuf> {
            Ok(self.launch.clone())
        }

        fn env_var(&self, name: &str) -> Option<OsString> {
            self.env.get(name).cloned()
        }
    }

    #[test]
    fn capability_warning_is_absent_when_cap_sys_ptrace_is_effective() {
        let warning =
            cap_sys_ptrace_warning_for(Ok(true), PathBuf::from("/opt/vosu/vosu").as_path());

        assert_eq!(warning, None);
    }

    #[test]
    fn capability_warning_is_emitted_when_cap_sys_ptrace_is_missing() {
        let warning =
            cap_sys_ptrace_warning_for(Ok(false), PathBuf::from("/opt/vosu/vosu").as_path());

        assert!(
            warning.is_some(),
            "missing memory-read capability must produce a startup warning"
        );
    }

    #[test]
    fn capability_warning_reports_inspection_failure_without_claiming_capability_is_missing() {
        let warning = cap_sys_ptrace_warning_for(
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "status unavailable",
            )),
            PathBuf::from("/opt/vosu/vosu").as_path(),
        );

        assert_eq!(
            warning.as_deref(),
            Some(
                "Could not inspect this process's effective Linux capabilities; memory reads may fail: status unavailable"
            )
        );
    }

    #[test]
    fn resolves_linux_xdg_roots_and_program_parent() {
        let temp = tempfile::tempdir().expect("temporary root");
        let program = temp.path().join("release");
        let launch = temp.path().join("working");
        let resolver = InjectedResolver::new(program.join("vosu"), launch.clone())
            .with_env("XDG_CONFIG_HOME", temp.path().join("xdg-config"))
            .with_env("XDG_DATA_HOME", temp.path().join("xdg-data"))
            .with_env("XDG_CACHE_HOME", temp.path().join("xdg-cache"));

        let paths =
            Paths::resolve(&resolver, BuildChannel::Release).expect("resolve injected paths");
        paths.initialize().unwrap();

        assert_eq!(paths.program(), program);
        assert_eq!(paths.launch(), launch);
        assert_eq!(paths.config(), temp.path().join("xdg-config").join("vosu"));
        assert_eq!(paths.data(), temp.path().join("xdg-data").join("vosu"));
        assert_eq!(paths.cache(), temp.path().join("xdg-cache").join("vosu"));
        assert!(paths.config().is_dir());
        assert!(paths.data().is_dir());
        assert!(paths.cache().is_dir());
        assert!(!launch.join("static").exists());
    }

    #[test]
    fn broker_paths_require_absolute_nonempty_roots() {
        let temp = tempfile::tempdir().unwrap();
        let resolver = InjectedResolver::new(temp.path().join("vosu"), temp.path().to_path_buf())
            .with_env("HOME", temp.path().to_path_buf());
        let mut paths = Paths::resolve(&resolver, BuildChannel::Release).unwrap();
        let fallback = paths.config.join("broker.json");
        assert_eq!(paths.broker_path_with(&resolver).unwrap(), fallback);
        let empty = resolver.clone().with_env("XDG_RUNTIME_DIR", PathBuf::new());
        assert_eq!(paths.broker_path_with(&empty).unwrap(), fallback);
        let runtime = resolver
            .clone()
            .with_env("XDG_RUNTIME_DIR", temp.path().join("runtime"));
        assert_eq!(
            paths.broker_path_with(&runtime).unwrap(),
            temp.path().join("runtime/vosu/broker.json")
        );
        let relative = resolver
            .clone()
            .with_env("XDG_RUNTIME_DIR", PathBuf::from("relative"));
        assert!(paths.broker_path_with(&relative).is_err());
        paths.config = PathBuf::from("relative-config");
        assert!(paths.broker_path_with(&resolver).is_err());
    }

    #[test]
    fn falls_back_to_home_and_resolves_static_and_settings() {
        let temp = tempfile::tempdir().expect("temporary root");
        let home = temp.path().join("home");
        let launch = temp.path().join("launch");
        std::fs::create_dir_all(&launch).expect("create launch directory");
        let resolver =
            InjectedResolver::new(temp.path().join("program").join("vosu"), launch.clone())
                .with_env("HOME", &home)
                .with_env("XDG_CONFIG_HOME", "")
                .with_env("XDG_DATA_HOME", "")
                .with_env("XDG_CACHE_HOME", "");

        let paths =
            Paths::resolve(&resolver, BuildChannel::Release).expect("resolve fallback paths");

        assert_eq!(paths.config(), home.join(".config/vosu"));
        assert_eq!(paths.data(), home.join(".local/share/vosu"));
        assert_eq!(paths.cache(), home.join(".cache/vosu"));
        assert!(!launch.join("static").exists());

        let default_static = paths
            .static_path(".\\STATIC")
            .expect("resolve default static");
        assert_eq!(default_static, home.join(".local/share/vosu/static"));
        assert!(default_static.is_dir());
        assert!(!launch.join("static").exists());

        let custom_static = paths
            .static_path("custom/counters")
            .expect("resolve custom static");
        assert_eq!(custom_static, launch.join("custom/counters"));
        assert!(custom_static.is_dir());

        assert_eq!(
            paths.settings_path("minimal").expect("settings path"),
            Some(home.join(".config/vosu/settings/minimal.settings.values.toml"))
        );
        assert!(home.join(".config/vosu/settings").is_dir());
        assert_eq!(
            paths.settings_path("").expect("empty settings folder"),
            None
        );
    }

    #[test]
    fn migrates_legacy_xdg_roots_without_overwriting_vosu_state() {
        let temp = tempfile::tempdir().expect("temporary root");
        let config_home = temp.path().join("config");
        let legacy = config_home.join("tosu");
        let current = config_home.join("vosu");
        std::fs::create_dir_all(legacy.join("nested")).expect("create legacy root");
        std::fs::create_dir_all(&current).expect("create current root");
        std::fs::write(legacy.join("nested/legacy.txt"), "legacy").expect("write legacy state");
        std::fs::write(legacy.join("shared.txt"), "old").expect("write legacy conflict");
        std::fs::write(current.join("shared.txt"), "current").expect("write current conflict");
        let resolver = InjectedResolver::new(temp.path().join("vosu"), temp.path().into())
            .with_env("XDG_CONFIG_HOME", &config_home)
            .with_env("XDG_DATA_HOME", temp.path().join("data"))
            .with_env("XDG_CACHE_HOME", temp.path().join("cache"));

        let paths = Paths::resolve(&resolver, BuildChannel::Release).unwrap();
        paths.initialize().expect("migrate legacy roots");
        paths.initialize().expect("repeat migration");

        assert_eq!(
            std::fs::read_to_string(current.join("shared.txt")).expect("read winning state"),
            "current"
        );
        assert_eq!(
            std::fs::read_to_string(current.join("nested/legacy.txt"))
                .expect("read migrated state"),
            "legacy"
        );
        assert!(!legacy.exists());
    }
}

#[cfg(test)]
mod bundled_plugin_paths {
    use super::{BuildChannel, Paths};
    use std::path::PathBuf;

    #[test]
    fn bundled_plugins_use_data_not_cache_or_launch_root() {
        let paths = Paths {
            program: PathBuf::from("program"),
            launch: PathBuf::from("launch"),
            config: PathBuf::from("config"),
            data: PathBuf::from("data"),
            cache: PathBuf::from("cache"),
            channel: BuildChannel::Release,
        };
        assert_eq!(
            paths.bundled_plugin_data_dir("vosu.osu"),
            PathBuf::from("data").join("plugins").join("vosu.osu")
        );
    }
}

//! Target-specific filesystem, system, and process integration.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use std::ffi::OsString;
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
/// Construction creates the config, data, and cache roots, but deliberately
/// does not create a static directory. The latter is created only when
/// [`Paths::static_path`] is requested.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paths {
    program: PathBuf,
    launch: PathBuf,
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
}

impl Paths {
    pub fn resolve(resolver: &impl PathResolver) -> io::Result<Self> {
        let executable = resolver.current_exe()?;
        let program = executable.parent().map(Path::to_path_buf).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "current executable has no parent directory",
            )
        })?;
        let launch = resolver.launch_dir()?;
        let (config, data, cache) = platform_roots(resolver, &program)?;
        // The sibling `tosu` roots are one-time migration inputs from pre-rename installs.
        #[cfg(target_os = "linux")]
        for target in [&config, &data, &cache] {
            migrate_legacy_directory(&target.with_file_name("tosu"), target)?;
        }

        for directory in [&config, &data, &cache] {
            std::fs::create_dir_all(directory)?;
        }

        Ok(Self {
            program,
            launch,
            config,
            data,
            cache,
        })
    }

    pub fn system() -> io::Result<Self> {
        Self::resolve(&SystemPathResolver)
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

    pub fn data(&self) -> &Path {
        &self.data
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

#[cfg(target_os = "linux")]
fn nonempty_env(resolver: &impl PathResolver, name: &str) -> Option<PathBuf> {
    resolver
        .env_var(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(target_os = "linux")]
pub(crate) fn install_supported_capabilities(executable: &Path) -> io::Result<()> {
    linux::install_supported_capabilities(executable)
}

#[cfg(target_os = "linux")]
pub(crate) fn cap_sys_ptrace_warning(executable: &Path) -> Option<String> {
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
/// The target is left untouched whenever replacement fails.
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
    use super::{PathResolver, Paths, cap_sys_ptrace_warning_for};
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::io;
    use std::path::PathBuf;

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
    fn capability_warning_recommends_authenticated_installer_when_cap_sys_ptrace_is_missing() {
        let warning =
            cap_sys_ptrace_warning_for(Ok(false), PathBuf::from("/opt/vosu/vosu").as_path());

        assert_eq!(
            warning.as_deref(),
            Some(
                "CAP_SYS_PTRACE is not effective; memory reads may fail. Install this build's required capabilities with `vosu --setup`; setup requests operating-system authentication when needed; current executable: \"/opt/vosu/vosu\"."
            )
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

        let paths = Paths::resolve(&resolver).expect("resolve injected paths");

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

        let paths = Paths::resolve(&resolver).expect("resolve fallback paths");

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

        Paths::resolve(&resolver).expect("migrate legacy roots");
        Paths::resolve(&resolver).expect("repeat migration");

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

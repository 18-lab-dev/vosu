use crate::config::{Config, ConfigChanged};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

pub const ARCHIVE_WARNING: &str = "It appears you are running %vosu% from an archive. Please extract it to a folder before running.";
pub const ONEDRIVE_WARNING: &str = "%vosu% cannot run from a OneDrive folder due to potential sync conflicts and performance issues.";
pub const ONEDRIVE_MOVE_WARNING: &str = "Please move %vosu% to a different folder.";

/// Startup-relevant environment captured once, before asynchronous work starts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentSnapshot {
    values: BTreeMap<&'static str, OsString>,
}

impl EnvironmentSnapshot {
    pub const NAMES: [&'static str; 9] = [
        "VOSU_OSU_PATH",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "TEMP",
        "OneDrive",
        "NODE_ENV",
        "PATH",
    ];

    pub fn capture() -> Self {
        Self::from_lookup(|name| std::env::var_os(name))
    }

    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<OsString>) -> Self {
        let values = Self::NAMES
            .into_iter()
            .filter_map(|name| lookup(name).map(|value| (name, value)))
            .collect();
        Self { values }
    }

    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.values.get(name).map(OsString::as_os_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &OsStr)> {
        self.values
            .iter()
            .map(|(name, value)| (*name, value.as_os_str()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupPlatform {
    Windows,
    Other,
}

impl StartupPlatform {
    pub const CURRENT: Self = if cfg!(windows) {
        Self::Windows
    } else {
        Self::Other
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupGuard {
    Archive,
    OneDrive,
}

/// The startup flags `App`/`StartupDecision` need: parsed from the osu!
/// client's own argv (`vosu_osu_reader::discovery::StartupArgs`) by the root
/// crate, which owns that osu!-specific parser. Core never depends on an
/// osu! crate (docs/v2/implementation-plan.md section 2 "Layout rules").
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StartupFlags {
    pub update: Option<bool>,
    pub onedrive_bypass: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupDecision {
    Run { update: bool, onedrive_bypass: bool },
    Stop(StartupGuard),
}

impl StartupDecision {
    pub fn evaluate(
        flags: &StartupFlags,
        config: &Config,
        environment: &EnvironmentSnapshot,
        platform: StartupPlatform,
        program_path: &Path,
    ) -> Self {
        let update = flags.update.unwrap_or(config.enable_auto_update);
        let onedrive_bypass = flags.onedrive_bypass;

        if platform == StartupPlatform::Windows {
            if environment
                .get("TEMP")
                .filter(|value| !value.is_empty())
                .is_some_and(|temp| os_starts_with(program_path.as_os_str(), temp))
            {
                return Self::Stop(StartupGuard::Archive);
            }
            if !onedrive_bypass
                && environment
                    .get("OneDrive")
                    .filter(|value| !value.is_empty())
                    .is_some_and(|one_drive| os_starts_with(program_path.as_os_str(), one_drive))
            {
                return Self::Stop(StartupGuard::OneDrive);
            }
        }

        Self::Run {
            update,
            onedrive_bypass,
        }
    }
}

fn os_starts_with(value: &OsStr, prefix: &OsStr) -> bool {
    let value = value.to_string_lossy();
    let prefix = prefix.to_string_lossy();
    let components = |path: &str| {
        path.split(['/', '\\'])
            .filter(|component| !component.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
    };
    let value = components(&value);
    let prefix = components(&prefix);
    value.len() >= prefix.len() && value.iter().zip(prefix.iter()).all(|(a, b)| a == b)
}

pub type HookFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;

/// Replaceable startup effects. There are intentionally no default method
/// bodies: updater and runtime owners must make every decision observable.
pub trait RuntimeHooks: Send {
    fn warn<'a>(&'a mut self, message: &'a str) -> HookFuture<'a, ()>;
    fn apply_update_decision(&mut self, enabled: bool) -> HookFuture<'_, ()>;
    fn start_runtime(&mut self) -> HookFuture<'_, ()>;
    fn next_config_change(&mut self) -> HookFuture<'_, Option<ConfigChanged>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupOutcome {
    Started,
    Guarded(StartupGuard),
}

pub struct App<H> {
    flags: StartupFlags,
    environment: EnvironmentSnapshot,
    program_path: PathBuf,
    platform: StartupPlatform,
    config: Arc<Config>,
    hooks: H,
}

impl<H: RuntimeHooks> App<H> {
    pub fn new(
        flags: StartupFlags,
        environment: EnvironmentSnapshot,
        program_path: PathBuf,
        platform: StartupPlatform,
        config: Arc<Config>,
        hooks: H,
    ) -> Self {
        Self {
            flags,
            environment,
            program_path,
            platform,
            config,
            hooks,
        }
    }

    pub fn hooks(&self) -> &H {
        &self.hooks
    }

    pub async fn run(&mut self) -> io::Result<StartupOutcome> {
        let decision = StartupDecision::evaluate(
            &self.flags,
            &self.config,
            &self.environment,
            self.platform,
            &self.program_path,
        );
        match decision {
            StartupDecision::Stop(StartupGuard::Archive) => {
                self.hooks.warn(ARCHIVE_WARNING).await?;
                return Ok(StartupOutcome::Guarded(StartupGuard::Archive));
            }
            StartupDecision::Stop(StartupGuard::OneDrive) => {
                self.hooks.warn(ONEDRIVE_WARNING).await?;
                self.hooks.warn(ONEDRIVE_MOVE_WARNING).await?;
                return Ok(StartupOutcome::Guarded(StartupGuard::OneDrive));
            }
            StartupDecision::Run { update, .. } => {
                self.hooks.apply_update_decision(update).await?;
            }
        }

        self.hooks.start_runtime().await?;

        while let Some(change) = self.hooks.next_config_change().await? {
            self.config = change.new;
        }
        Ok(StartupOutcome::Started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingHooks {
        events: Vec<String>,
        changes: std::collections::VecDeque<ConfigChanged>,
    }

    impl RuntimeHooks for RecordingHooks {
        fn warn<'a>(&'a mut self, message: &'a str) -> HookFuture<'a, ()> {
            Box::pin(async move {
                self.events.push(format!("warn:{message}"));
                Ok(())
            })
        }

        fn apply_update_decision(&mut self, enabled: bool) -> HookFuture<'_, ()> {
            Box::pin(async move {
                self.events.push(format!("update:{enabled}"));
                Ok(())
            })
        }

        fn start_runtime(&mut self) -> HookFuture<'_, ()> {
            Box::pin(async move {
                self.events.push("runtime".to_owned());
                Ok(())
            })
        }

        fn next_config_change(&mut self) -> HookFuture<'_, Option<ConfigChanged>> {
            Box::pin(async move { Ok(self.changes.pop_front()) })
        }
    }

    fn environment(items: &[(&str, &str)]) -> EnvironmentSnapshot {
        EnvironmentSnapshot::from_lookup(|name| {
            items
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    fn update_flag_falls_back_to_config_only_when_absent() {
        let mut config = Config {
            enable_auto_update: true,
            ..Config::default()
        };
        let env = EnvironmentSnapshot::default();
        let path = Path::new("/app/vosu");

        assert_eq!(
            StartupDecision::evaluate(
                &StartupFlags {
                    update: Some(false),
                    onedrive_bypass: true
                },
                &config,
                &env,
                StartupPlatform::Other,
                path
            ),
            StartupDecision::Run {
                update: false,
                onedrive_bypass: true
            }
        );
        assert_eq!(
            StartupDecision::evaluate(
                &StartupFlags {
                    update: Some(true),
                    onedrive_bypass: false
                },
                &config,
                &env,
                StartupPlatform::Other,
                path
            ),
            StartupDecision::Run {
                update: true,
                onedrive_bypass: false
            }
        );
        config.enable_auto_update = false;
        assert_eq!(
            StartupDecision::evaluate(
                &StartupFlags {
                    update: None,
                    onedrive_bypass: false
                },
                &config,
                &env,
                StartupPlatform::Other,
                path
            ),
            StartupDecision::Run {
                update: false,
                onedrive_bypass: false
            }
        );
    }

    #[test]
    fn captures_only_the_startup_environment_snapshot() {
        let snapshot = EnvironmentSnapshot::from_lookup(|name| match name {
            "VOSU_OSU_PATH" => Some("/osu".into()),
            "XDG_DATA_HOME" => Some("/data".into()),
            "TEMP" => Some("C:/Temp".into()),
            "OneDrive" => Some("C:/Users/me/OneDrive".into()),
            "PATH" => Some("/bin".into()),
            _ => None,
        });
        assert_eq!(snapshot.get("VOSU_OSU_PATH"), Some(OsStr::new("/osu")));
        assert_eq!(snapshot.get("XDG_DATA_HOME"), Some(OsStr::new("/data")));
        assert_eq!(snapshot.get("TEMP"), Some(OsStr::new("C:/Temp")));
        assert_eq!(
            snapshot.get("OneDrive"),
            Some(OsStr::new("C:/Users/me/OneDrive"))
        );
        assert_eq!(snapshot.get("UNRELATED"), None);
    }

    #[test]
    fn windows_path_prefix_is_component_aware_and_case_insensitive() {
        assert!(os_starts_with(
            OsStr::new(r"C:\Users\ME\OneDrive\vosu.exe"),
            OsStr::new("c:/users/me/onedrive")
        ));
        assert!(!os_starts_with(
            OsStr::new(r"C:\Users\me\OneDriveCopy\vosu.exe"),
            OsStr::new("C:/Users/me/OneDrive")
        ));
        assert!(!os_starts_with(
            OsStr::new(r"C:\Users\me\OneDrive-old\vosu.exe"),
            OsStr::new("C:/Users/me/OneDrive")
        ));
    }

    #[tokio::test]
    async fn windows_archive_guard_warns_before_update_and_runtime() {
        let mut app = App::new(
            StartupFlags {
                update: Some(true),
                onedrive_bypass: false,
            },
            environment(&[("TEMP", "C:/Temp")]),
            PathBuf::from("C:/Temp/unpacked/vosu.exe"),
            StartupPlatform::Windows,
            Arc::new(Config::default()),
            RecordingHooks::default(),
        );
        assert_eq!(
            vosu_proptest::bounded!(app.run()).unwrap(),
            StartupOutcome::Guarded(StartupGuard::Archive)
        );
        assert_eq!(app.hooks().events, [format!("warn:{ARCHIVE_WARNING}")]);
    }

    #[tokio::test]
    async fn windows_onedrive_guard_has_exact_messages_and_true_bypass() {
        let env = environment(&[("OneDrive", "C:/Users/me/OneDrive")]);
        let path = PathBuf::from("C:/Users/me/OneDrive/vosu/vosu.exe");
        let mut blocked = App::new(
            StartupFlags {
                update: Some(true),
                onedrive_bypass: false,
            },
            env.clone(),
            path.clone(),
            StartupPlatform::Windows,
            Arc::new(Config::default()),
            RecordingHooks::default(),
        );
        assert_eq!(
            vosu_proptest::bounded!(blocked.run()).unwrap(),
            StartupOutcome::Guarded(StartupGuard::OneDrive)
        );
        assert_eq!(
            blocked.hooks().events,
            [
                format!("warn:{ONEDRIVE_WARNING}"),
                format!("warn:{ONEDRIVE_MOVE_WARNING}")
            ]
        );

        let mut bypassed = App::new(
            StartupFlags {
                update: Some(false),
                onedrive_bypass: true,
            },
            env,
            path,
            StartupPlatform::Windows,
            Arc::new(Config::default()),
            RecordingHooks::default(),
        );
        assert_eq!(
            vosu_proptest::bounded!(bypassed.run()).unwrap(),
            StartupOutcome::Started
        );
        assert_eq!(bypassed.hooks().events, ["update:false", "runtime"]);
    }
}

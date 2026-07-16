use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use indexmap::IndexMap;

use crate::config::{Config, ConfigChanged};

pub const ARCHIVE_WARNING: &str = "It appears you are running %vosu% from an archive. Please extract it to a folder before running.";
pub const ONEDRIVE_WARNING: &str = "%vosu% cannot run from a OneDrive folder due to potential sync conflicts and performance issues.";
pub const ONEDRIVE_MOVE_WARNING: &str = "Please move %vosu% to a different folder.";
pub const OVERLAY_WARNING: &str = "In-game overlay is unavailable in the Rust daemon-only build; ENABLE_INGAME_OVERLAY is ignored.";

/// A value produced by the source application's permissive argument grammar.
#[derive(Clone, Debug, PartialEq)]
pub enum StartupValue {
    Number(f64),
    Boolean(bool),
    String(OsString),
}

impl StartupValue {
    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            Self::Number(_) | Self::String(_) => None,
        }
    }

    #[must_use]
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Boolean(_) | Self::String(_) => None,
        }
    }

    #[must_use]
    pub fn as_os_str(&self) -> Option<&OsStr> {
        match self {
            Self::String(value) => Some(value.as_os_str()),
            Self::Number(_) | Self::Boolean(_) => None,
        }
    }

    /// JavaScript truthiness after the source parser's coercion.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::Boolean(value) => *value,
            Self::String(value) => !value.is_empty(),
        }
    }
}

/// Parsed startup arguments. Unknown entries are deliberately retained so this
/// remains the common source-compatible parser, while startup decisions consult
/// only `update` and `onedrive`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartupArgs {
    raw: Vec<OsString>,
    values: IndexMap<String, StartupValue>,
}

impl StartupArgs {
    /// Parses an argv array, excluding argv[0]. Each remaining array element is
    /// matched independently, like the TypeScript array overload.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Self {
        let mut args = args.into_iter();
        let _ = args.next();
        Self::from_array(args)
    }

    /// Parses argument elements without treating the first element as argv[0].
    pub fn from_array(args: impl IntoIterator<Item = OsString>) -> Self {
        let raw = args.into_iter().collect::<Vec<_>>();
        let mut values = IndexMap::new();
        for argument in &raw {
            // Process argv may contain arbitrary Unix bytes. Preserve it in `raw` and
            // never manufacture flag names or values through replacement characters.
            let Some(argument) = argument.to_str() else {
                continue;
            };
            if let Some((name, value)) = first_match(argument) {
                values.insert(name, coerce(value));
            }
        }
        Self { raw, values }
    }

    /// Parses the string overload globally, accepting `-`/`--` and `=`/one
    /// literal space exactly like the source regular expression.
    pub fn parse_command_line(command_line: &str) -> Self {
        let mut values = IndexMap::new();
        let mut cursor = 0;
        while let Some((start, end, name, value)) = next_match(command_line, cursor) {
            values.insert(name, coerce(value));
            cursor = end.max(start + 1);
        }
        Self {
            raw: vec![OsString::from(command_line)],
            values,
        }
    }

    pub fn raw(&self) -> &[OsString] {
        &self.raw
    }

    pub fn values(&self) -> &IndexMap<String, StartupValue> {
        &self.values
    }

    pub fn get(&self, name: &str) -> Option<&StartupValue> {
        self.values.get(name)
    }
}

fn first_match(input: &str) -> Option<(String, &str)> {
    next_match(input, 0).map(|(_, _, name, value)| (name, value))
}

fn next_match(input: &str, from: usize) -> Option<(usize, usize, String, &str)> {
    let bytes = input.as_bytes();
    let mut start = from;
    while start < bytes.len() {
        if bytes[start] != b'-' {
            start += 1;
            continue;
        }

        let mut name_start = start + 1;
        if bytes.get(name_start) == Some(&b'-') {
            name_start += 1;
        }
        if name_start >= bytes.len()
            || bytes[name_start] == b'='
            || bytes[name_start].is_ascii_whitespace()
        {
            start += 1;
            continue;
        }

        let mut name_end = name_start;
        while name_end < bytes.len()
            && bytes[name_end] != b'='
            && !bytes[name_end].is_ascii_whitespace()
        {
            name_end += 1;
        }
        let name = input[name_start..name_end].to_owned();
        let mut end = name_end;
        let mut value = "";
        if matches!(bytes.get(name_end), Some(b'=') | Some(b' ')) {
            let value_start = name_end + 1;
            if value_start < bytes.len()
                && bytes[value_start] != b'-'
                && !bytes[value_start].is_ascii_whitespace()
            {
                end = value_start;
                while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
                    end += 1;
                }
                value = &input[value_start..end];
            } else {
                end = value_start.min(bytes.len());
            }
        }
        return Some((start, end, name, value));
    }
    None
}

fn coerce(value: &str) -> StartupValue {
    if let Some(number) = parse_float_prefix(value) {
        StartupValue::Number(number)
    } else if value == "true" {
        StartupValue::Boolean(true)
    } else if value == "false" {
        StartupValue::Boolean(false)
    } else {
        StartupValue::String(OsString::from(value))
    }
}

/// JavaScript `parseFloat`'s accepted numeric prefix for the argument inputs.
fn parse_float_prefix(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    if matches!(bytes.first(), Some(b'+') | Some(b'-')) {
        cursor += 1;
    }
    if value.get(cursor..)?.starts_with("Infinity") {
        return Some(if bytes.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }

    let integer_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    let mut has_digit = cursor > integer_start;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        has_digit |= cursor > fraction_start;
    }
    if !has_digit {
        return None;
    }

    let exponent = cursor;
    if matches!(bytes.get(cursor), Some(b'e') | Some(b'E')) {
        cursor += 1;
        if matches!(bytes.get(cursor), Some(b'+') | Some(b'-')) {
            cursor += 1;
        }
        let digits = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == digits {
            cursor = exponent;
        }
    }
    value[..cursor].parse().ok()
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StartupDecision {
    Run { update: bool, onedrive_bypass: bool },
    Stop(StartupGuard),
}

impl StartupDecision {
    pub fn evaluate(
        args: &StartupArgs,
        config: &Config,
        environment: &EnvironmentSnapshot,
        platform: StartupPlatform,
        program_path: &Path,
    ) -> Self {
        let update = args
            .get("update")
            .and_then(StartupValue::as_boolean)
            .unwrap_or(config.enable_auto_update);
        let onedrive_bypass = args.get("onedrive").and_then(StartupValue::as_boolean) == Some(true);

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
    value
        .to_string_lossy()
        .starts_with(prefix.to_string_lossy().as_ref())
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
    args: StartupArgs,
    environment: EnvironmentSnapshot,
    program_path: PathBuf,
    platform: StartupPlatform,
    config: Arc<Config>,
    hooks: H,
}

impl<H: RuntimeHooks> App<H> {
    pub fn new(
        args: StartupArgs,
        environment: EnvironmentSnapshot,
        program_path: PathBuf,
        platform: StartupPlatform,
        config: Arc<Config>,
        hooks: H,
    ) -> Self {
        Self {
            args,
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
            &self.args,
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

        if self.config.enable_ingame_overlay {
            self.hooks.warn(OVERLAY_WARNING).await?;
        }
        self.hooks.start_runtime().await?;

        while let Some(change) = self.hooks.next_config_change().await? {
            if !change.old.enable_ingame_overlay && change.new.enable_ingame_overlay {
                self.hooks.warn(OVERLAY_WARNING).await?;
            }
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

    fn args(items: &[&str]) -> StartupArgs {
        StartupArgs::from_array(items.iter().map(OsString::from))
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
    fn parses_array_and_command_line_source_grammar() {
        let parsed = args(&[
            "ignored prefix --rate=12.5ms",
            "-enabled true",
            "--missing",
            "--next -other=false",
            "--unknown=value",
        ]);
        assert_eq!(parsed.get("rate"), Some(&StartupValue::Number(12.5)));
        assert_eq!(parsed.get("enabled"), Some(&StartupValue::Boolean(true)));
        assert_eq!(
            parsed.get("missing"),
            Some(&StartupValue::String(OsString::new()))
        );
        assert_eq!(
            parsed.get("next"),
            Some(&StartupValue::String(OsString::new()))
        );
        assert_eq!(
            parsed.get("unknown"),
            Some(&StartupValue::String("value".into()))
        );
        assert_eq!(
            parsed.get("other"),
            None,
            "array overload takes one match per element"
        );

        let parsed = StartupArgs::parse_command_line(
            "vosu --rate=+.5e2tail -enabled false --missing --other=3e --truth=TRUE",
        );
        assert_eq!(parsed.get("rate"), Some(&StartupValue::Number(50.0)));
        assert_eq!(parsed.get("enabled"), Some(&StartupValue::Boolean(false)));
        assert_eq!(
            parsed.get("missing"),
            Some(&StartupValue::String(OsString::new()))
        );
        assert_eq!(parsed.get("other"), Some(&StartupValue::Number(3.0)));
        assert_eq!(
            parsed.get("truth"),
            Some(&StartupValue::String("TRUE".into()))
        );
    }

    #[test]
    fn process_argv_skips_executable_and_retains_unknown_map_entries() {
        let parsed = StartupArgs::parse([
            OsString::from("--not-a-flag=1"),
            OsString::from("--update=false"),
            OsString::from("--future=42suffix"),
        ]);
        assert_eq!(parsed.get("not-a-flag"), None);
        assert_eq!(parsed.get("update"), Some(&StartupValue::Boolean(false)));
        assert_eq!(parsed.get("future"), Some(&StartupValue::Number(42.0)));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_process_argv_is_retained_without_lossy_flag_substitution() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let invalid = OsString::from_vec(b"--devserver=\xff".to_vec());
        let parsed = StartupArgs::from_array([invalid.clone()]);
        assert_eq!(parsed.raw()[0].as_bytes(), invalid.as_bytes());
        assert_eq!(parsed.get("devserver"), None);
    }

    #[test]
    fn effective_flags_require_actual_booleans() {
        let mut config = Config {
            enable_auto_update: true,
            ..Config::default()
        };
        let env = EnvironmentSnapshot::default();
        let path = Path::new("/app/vosu");

        assert_eq!(
            StartupDecision::evaluate(
                &args(&["--update=false", "--onedrive=true"]),
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
                &args(&["--update=1", "--onedrive=1"]),
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
                &args(&["--update=yes"]),
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

    #[tokio::test]
    async fn windows_archive_guard_warns_before_update_and_runtime() {
        let mut app = App::new(
            args(&["--update=true"]),
            environment(&[("TEMP", "C:/Temp")]),
            PathBuf::from("C:/Temp/unpacked/vosu.exe"),
            StartupPlatform::Windows,
            Arc::new(Config::default()),
            RecordingHooks::default(),
        );
        assert_eq!(
            app.run().await.unwrap(),
            StartupOutcome::Guarded(StartupGuard::Archive)
        );
        assert_eq!(app.hooks().events, [format!("warn:{ARCHIVE_WARNING}")]);
    }

    #[tokio::test]
    async fn windows_onedrive_guard_has_exact_messages_and_true_bypass() {
        let env = environment(&[("OneDrive", "C:/Users/me/OneDrive")]);
        let path = PathBuf::from("C:/Users/me/OneDrive/vosu/vosu.exe");
        let mut blocked = App::new(
            args(&["--update=true"]),
            env.clone(),
            path.clone(),
            StartupPlatform::Windows,
            Arc::new(Config::default()),
            RecordingHooks::default(),
        );
        assert_eq!(
            blocked.run().await.unwrap(),
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
            args(&["--onedrive=true", "--update=false"]),
            env,
            path,
            StartupPlatform::Windows,
            Arc::new(Config::default()),
            RecordingHooks::default(),
        );
        assert_eq!(bypassed.run().await.unwrap(), StartupOutcome::Started);
        assert_eq!(bypassed.hooks().events, ["update:false", "runtime"]);
    }

    #[tokio::test]
    async fn overlay_warning_is_startup_and_false_to_true_edge_triggered_without_mutation() {
        let initial = Config {
            enable_ingame_overlay: true,
            ..Config::default()
        };
        let mut disabled = initial.clone();
        disabled.enable_ingame_overlay = false;
        let mut enabled_again = disabled.clone();
        enabled_again.enable_ingame_overlay = true;
        let hooks = RecordingHooks {
            events: Vec::new(),
            changes: [
                ConfigChanged {
                    old: Arc::new(initial.clone()),
                    new: Arc::new(initial.clone()),
                },
                ConfigChanged {
                    old: Arc::new(initial.clone()),
                    new: Arc::new(disabled.clone()),
                },
                ConfigChanged {
                    old: Arc::new(disabled),
                    new: Arc::new(enabled_again.clone()),
                },
                ConfigChanged {
                    old: Arc::new(enabled_again.clone()),
                    new: Arc::new(enabled_again),
                },
            ]
            .into(),
        };
        let mut app = App::new(
            StartupArgs::default(),
            EnvironmentSnapshot::default(),
            PathBuf::from("/vosu"),
            StartupPlatform::Other,
            Arc::new(initial),
            hooks,
        );
        assert_eq!(app.run().await.unwrap(), StartupOutcome::Started);
        assert_eq!(
            app.hooks().events,
            [
                "update:true",
                &format!("warn:{OVERLAY_WARNING}"),
                "runtime",
                &format!("warn:{OVERLAY_WARNING}")
            ]
        );
        assert!(
            app.config.enable_ingame_overlay,
            "warning must not coerce persisted config"
        );
    }
}

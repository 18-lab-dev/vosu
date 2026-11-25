//! Every way the plugin host can fail to load or run a plugin
//! (`docs/v2/plugin-abi.md` §1, §2, §5, §7.1).

use std::fmt;

use super::settings::SettingsError;
use vosu_plugin_api::manifest::ValidationError;

use super::matcher::RegisterError;
use super::process::control::StartupFailure;

/// Every way loading, starting, or tearing down a plugin instance can fail.
#[derive(Debug)]
pub enum HostError {
    Manifest(ValidationError),
    Io(std::io::Error),
    Load(libloading::Error),
    /// `vosu_plugin_entry_v1` returned `NULL`.
    MissingEntry,
    /// The manifest has no `[targets.<running-host-triple>]` entry.
    MissingTarget,
    /// The vtable's `abi_version` does not equal `VOSU_PLUGIN_ABI_MAJOR`
    /// (§1: a host never calls a function whose struct layout it cannot
    /// interpret).
    AbiMismatch {
        expected: u32,
        found: u32,
    },
    /// `struct_size` is smaller than this major version's minimum valid
    /// size: `create`, `dispatch`, and `destroy` must all be present (§1).
    DescriptorTooSmall {
        minimum: usize,
        found: usize,
    },
    /// `create`, `dispatch`, or `destroy` is `None`.
    MissingRequiredFunction,
    /// §7.1: `runtime = "in-process"` was requested by a plugin this trust
    /// tier does not grant it to; refused outright rather than silently
    /// downgraded.
    InProcessRefused {
        reason: &'static str,
    },
    /// `--plugin <dir>` in a stable release build (catalog-v2.md A5.5):
    /// only debug and prerelease builds accept development packages.
    DevelopmentRefused,
    /// `create` returned `NULL`.
    CreateRejected,
    /// `concurrent_dispatch = true` but no shared worker pool was supplied.
    ConcurrentPoolMissing,
    /// `destroy` did not return within its grace period (§4): the host
    /// proceeds without it.
    DestroyTimedOut,
    /// A bounded dispatch queue (per-instance serial, or the shared
    /// `concurrent_dispatch` pool) was full.
    DispatchQueueFull,
    /// `create`'s `init.settings`/`init` payload failed to CBOR-encode
    /// (`PluginHost::create_instance`, §6.9's `init.settings`).
    InitEncodeFailed(String),
    /// `PluginHost::dispatch_to_instance` named an id no instance is
    /// currently registered under (never created, or already destroyed).
    UnknownInstance,
    /// `create_instance` raced `destroy_all` (the instance's own `create`
    /// was still running when shutdown started): the instance was
    /// destroyed immediately instead of being published `Live`, so it
    /// never actually starts running.
    ShuttingDown,
    /// `create_instance`'s manifest `[match]` table failed to register
    /// with `MatcherEngine` (§6.1, D18: static overlap, the Linux
    /// pre-filter bound, or the table itself failing to compile).
    MatcherRegister(RegisterError),
    /// `BundledPlugin::settings_schema`/`--plugin <dir>`'s own
    /// `manifest.settings_schema` path failed to parse as a
    /// `settings.schema.json` (§6.9, §8).
    Settings(SettingsError),
    /// `load_bundled`: a manifest's own `settings_schema` key and its
    /// `BundledPlugin::settings_schema` entry disagree about whether this
    /// plugin declares a schema at all (P0 review: a missed
    /// `Some(SETTINGS_SCHEMA)` would otherwise silently load the plugin
    /// with no schema — an empty `init.settings` and empty bridge B2
    /// deliveries, with nothing to catch it).
    SettingsSchemaMismatch {
        plugin_id: String,
        manifest_declares_one: bool,
        entry_declares_one: bool,
    },
    State(String),
    /// A `runtime = "process"` package reached instance creation with no
    /// process launcher installed: core never runs it in-process instead.
    ProcessRuntimeUnavailable,
    /// The plugin-host child for this instance did not become ready: it
    /// exited, broke its channel, or its plugin's `create` failed. The
    /// launcher has already reaped it. The failure keeps its §7.6 stage, so
    /// supervision decides restarts from [`StartupFailure::stage`], never
    /// from formatted text.
    ProcessStartupFailed(StartupFailure),
    /// The instance's process was reported lost before it was published.
    InstanceLost,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(e) => write!(f, "manifest: {e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::Load(e) => write!(f, "{e}"),
            Self::MissingEntry => write!(f, "vosu_plugin_entry_v1 returned NULL"),
            Self::MissingTarget => {
                write!(
                    f,
                    "manifest has no [targets.<host-triple>] entry for this host"
                )
            }
            Self::AbiMismatch { expected, found } => write!(
                f,
                "abi_version {found} does not match VOSU_PLUGIN_ABI_MAJOR {expected}"
            ),
            Self::DescriptorTooSmall { minimum, found } => write!(
                f,
                "struct_size {found} is smaller than the minimum valid size {minimum}"
            ),
            Self::MissingRequiredFunction => write!(f, "create, dispatch, or destroy is missing"),
            Self::InProcessRefused { reason } => write!(f, "in-process runtime refused: {reason}"),
            Self::DevelopmentRefused => write!(
                f,
                "stable release builds refuse `--plugin <dir>`; use a debug or prerelease build"
            ),
            Self::CreateRejected => write!(f, "create returned NULL"),
            Self::ConcurrentPoolMissing => write!(
                f,
                "concurrent_dispatch = true but no worker pool was supplied"
            ),
            Self::DestroyTimedOut => write!(f, "destroy did not return within its grace period"),
            Self::DispatchQueueFull => write!(f, "dispatch queue is full"),
            Self::InitEncodeFailed(error) => write!(f, "encoding init failed: {error}"),
            Self::UnknownInstance => write!(f, "no instance is registered under that id"),
            Self::ShuttingDown => write!(f, "the plugin host is shutting down"),
            Self::MatcherRegister(e) => write!(f, "{e}"),
            Self::Settings(e) => write!(f, "{e}"),
            Self::SettingsSchemaMismatch {
                plugin_id,
                manifest_declares_one,
                entry_declares_one,
            } => write!(
                f,
                "plugin {plugin_id:?}'s manifest {} a settings_schema, but its BundledPlugin entry {} one",
                if *manifest_declares_one {
                    "declares"
                } else {
                    "declares no"
                },
                if *entry_declares_one {
                    "declares"
                } else {
                    "declares no"
                },
            ),
            Self::State(e) => write!(f, "state: {e}"),
            Self::ProcessRuntimeUnavailable => write!(
                f,
                "runtime = \"process\" requires the plugin-host launcher, which is not available"
            ),
            Self::ProcessStartupFailed(failure) => {
                write!(f, "plugin-host process did not start: {failure}")
            }
            Self::InstanceLost => write!(f, "the plugin-host process was lost"),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Load(e) => Some(e),
            Self::MatcherRegister(e) => Some(e),
            Self::Settings(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ValidationError> for HostError {
    fn from(e: ValidationError) -> Self {
        Self::Manifest(e)
    }
}

impl From<std::io::Error> for HostError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<libloading::Error> for HostError {
    fn from(e: libloading::Error) -> Self {
        Self::Load(e)
    }
}

//! Every way the plugin host can fail to load or run a plugin
//! (`docs/v2/plugin-abi.md` §1, §2, §5, §7.1).

use std::fmt;

use vosu_plugin_api::ManifestError;

/// Every way loading, starting, or tearing down a plugin instance can fail.
#[derive(Debug)]
pub enum HostError {
    Manifest(ManifestError),
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
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Manifest(e) => Some(e),
            Self::Io(e) => Some(e),
            Self::Load(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ManifestError> for HostError {
    fn from(e: ManifestError) -> Self {
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

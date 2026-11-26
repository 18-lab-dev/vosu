//! Manifest-first overlay loading. No error permits a legacy metadata fallback.
//! Manifests parse only through the SDK's `PackageManifest` and validate
//! only through its `validate_context` (M6a); no second parser exists here.
use super::*;
use std::io::Read as _;
use vosu_plugin_api::manifest::{
    HostSupport, PackageManifest, Registration, ValidationContext, ValidationError,
};

/// The M6b installed-graph input to overlay host-support validation,
/// injected by composition as a pure validation callback: it returns one
/// result per `discovered` manifest, in order, and carries no core handle,
/// path or credential across the seam (A7.1).
pub trait InstalledPackages: Send + Sync {
    /// Validates each of this scan's overlay manifests against the live
    /// installed providers plus `discovered` itself (the duplicate-identity
    /// input), with source `"overlays"` and `official = false`.
    fn validate_all(
        &self,
        discovered: &[&PackageManifest],
    ) -> Vec<Result<HostSupport, ValidationError>>;
}

/// Validation with no installed provider: only the scan's own overlays are
/// registered. Used until composition supplies the installed graph.
pub(super) struct OverlaysOnly;

impl InstalledPackages for OverlaysOnly {
    fn validate_all(
        &self,
        discovered: &[&PackageManifest],
    ) -> Vec<Result<HostSupport, ValidationError>> {
        let replaces: Vec<Vec<String>> = discovered
            .iter()
            .map(|package| package.fields().replaces.to_vec())
            .collect();
        let registrations: Vec<_> = discovered
            .iter()
            .zip(&replaces)
            .map(|(package, replaces)| Registration {
                kind: package.kind(),
                id: package.fields().id,
                source: "overlays",
                official: false,
                builtin: false,
                replaces,
            })
            .collect();
        let context = ValidationContext {
            application_version: crate::host_version(),
            source: "overlays",
            official: false,
            registrations: &registrations,
            namespace_revisions: &[],
        };
        discovered
            .iter()
            .map(|package| package.validate_context(&context))
            .collect()
    }
}

pub(super) fn read(path: &Path) -> Result<PackageManifest, CounterError> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("overlay manifest must not be a symlink".into()));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_METADATA_BYTES as u64 {
        return Err(invalid(
            "overlay manifest exceeds limit or is not a file".into(),
        ));
    }
    let mut text = String::new();
    file.take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_string(&mut text)?;
    if text.len() > MAX_METADATA_BYTES {
        return Err(invalid("overlay manifest exceeds limit".into()));
    }
    let package = PackageManifest::parse(&text).map_err(|error| invalid(error.to_string()))?;
    if !matches!(package, PackageManifest::Overlay(_)) {
        return Err(invalid("overlay manifest requires kind='overlay'".into()));
    }
    Ok(package)
}

pub(super) fn counter(
    static_root: &Path,
    path: &Path,
    settings_store: &SettingsStore,
    support: Result<HostSupport, ValidationError>,
    package: PackageManifest,
) -> Result<Counter, CounterError> {
    match support.map_err(|error| invalid(error.to_string()))? {
        HostSupport::Supported => {}
        HostSupport::Unsupported => {
            return Err(invalid(
                "overlay manifest does not support this daemon".into(),
            ));
        }
    }
    let PackageManifest::Overlay(overlay) = package else {
        return Err(invalid("overlay manifest requires kind='overlay'".into()));
    };
    let directory = path.parent().ok_or(CounterError::InvalidName)?;
    let folder_name = relative_folder(static_root, directory)?;
    let settings = load_discovered_settings(settings_store, &folder_name)?;
    Ok(Counter {
        downloaded: false,
        updatable: false,
        folder_name,
        name: overlay.shared.name,
        author: overlay.shared.author.unwrap_or_default(),
        version: overlay.shared.version,
        resolution: overlay
            .resolution
            .map(|size| size.into_iter().map(Value::from).collect())
            .unwrap_or_else(|| vec![Value::from("Any"), Value::from("Any")]),
        authorlinks: overlay.shared.homepage.into_iter().collect(),
        settings,
        usecase: None,
        compatiblewith: None,
        assets: None,
        download_link: None,
        version_known: true,
    })
}

fn invalid(message: String) -> CounterError {
    CounterError::Io(io::Error::new(io::ErrorKind::InvalidData, message))
}

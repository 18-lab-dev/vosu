//! Manifest-first overlay loading. No error permits a legacy metadata fallback.
use super::*;
use std::io::Read as _;
use vosu_plugin_api::manifest::PackageManifest;

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
    snapshot: &vosu_core::plugin::ValidationSnapshot,
    package: PackageManifest,
) -> Result<Counter, CounterError> {
    match snapshot
        .validate(&package, crate::application_version(), "overlays", false)
        .map_err(|error| invalid(error.to_string()))?
    {
        vosu_plugin_api::manifest::HostSupport::Supported => {}
        vosu_plugin_api::manifest::HostSupport::Unsupported => {
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
    })
}

fn invalid(message: String) -> CounterError {
    CounterError::Io(io::Error::new(io::ErrorKind::InvalidData, message))
}

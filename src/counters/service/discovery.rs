//! Bounded manifest-first overlay discovery and legacy file I/O.
use super::*;
use std::io::Read as _;
use std::time::SystemTime;

const MAX_DISCOVERY_FILES: usize = 4096;

impl CounterService {
    /// The overlay catalog scan behind [`Self::discover`], recording which
    /// file each entry came from: metadata-only folders are listed (and
    /// served) like any overlay, but only folders with an `index.html` can
    /// be loaded as a page.
    pub(super) fn scan(&self) -> Result<(Vec<Discovered>, Vec<BrokenOverlay>), CounterError> {
        let manifests = recursive_named_files(&self.static_root, "manifest.toml", true)?;
        let metadata = recursive_named_files(&self.static_root, "metadata.txt", true)?;
        let indexes = recursive_named_files(&self.static_root, "index.html", true)?;
        let manifests: Vec<_> = manifests
            .into_iter()
            .map(|path| {
                let package = manifest::read(&path);
                (path, package)
            })
            .collect();
        let mut snapshot = self
            .validation_host
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade()
            .map_or_else(Default::default, |host| host.validation_snapshot());
        snapshot
            .packages
            .extend(manifests.iter().filter_map(|(_, package)| {
                package
                    .as_ref()
                    .ok()
                    .map(|package| vosu_core::plugin::PackageRegistration::new(package, "overlays"))
            }));
        let metadata_dirs: std::collections::HashSet<_> = metadata
            .iter()
            .filter_map(|path| path.parent())
            .map(Path::to_path_buf)
            .collect();
        let index_dirs: std::collections::HashSet<_> = indexes
            .iter()
            .filter_map(|path| path.parent())
            .map(Path::to_path_buf)
            .collect();
        let mut found = Vec::new();
        let mut broken = Vec::new();
        // Settings errors already name their file; other errors are reported
        // against the file being read.
        let mut record =
            |directory: &Path, file: &str, error: CounterError| -> Result<(), CounterError> {
                let folder_name = relative_folder(&self.static_root, directory)?;
                broken.push(BrokenOverlay {
                    name: directory
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| folder_name.clone()),
                    folder_name,
                    error: match error {
                        CounterError::Settings(error) => error.to_string(),
                        error => format!("{file}: {error}"),
                    },
                });
                Ok(())
            };
        for (path, package) in manifests {
            let directory = path.parent().expect("manifest has parent");
            match package.and_then(|package| {
                manifest::counter(&self.static_root, &path, &self.settings, &snapshot, package)
            }) {
                Ok(counter) => found.push(Discovered {
                    counter,
                    metadata: true,
                    index: index_dirs.contains(directory),
                }),
                Err(error) => record(directory, "manifest.toml", error)?,
            }
        }
        for path in metadata {
            let directory = path.parent().expect("metadata has parent");
            if !matches!(fs::symlink_metadata(directory.join("manifest.toml")),
                Err(error) if error.kind() == io::ErrorKind::NotFound)
            {
                continue;
            }
            match parse_metadata(&self.static_root, &path, &self.settings) {
                Ok(counter) if !counter.name.is_empty() => found.push(Discovered {
                    counter,
                    metadata: true,
                    index: index_dirs.contains(directory),
                }),
                Ok(_) => {}
                Err(error) => record(directory, "metadata.txt", error)?,
            }
        }
        for path in indexes {
            let directory = path.parent().expect("index has parent");
            if metadata_dirs.contains(directory)
                || !matches!(fs::symlink_metadata(directory.join("manifest.toml")),
                    Err(error) if error.kind() == io::ErrorKind::NotFound)
            {
                continue;
            }
            let folder_name = relative_folder(&self.static_root, directory)?;
            let settings = match load_discovered_settings(&self.settings, &folder_name) {
                Ok(settings) => settings,
                Err(error) => {
                    record(directory, "settings", error)?;
                    continue;
                }
            };
            found.push(Discovered {
                counter: Counter {
                    downloaded: false,
                    updatable: false,
                    folder_name,
                    name: directory
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_owned(),
                    version: "1.0".into(),
                    author: "local".into(),
                    resolution: vec![Value::from(-2), Value::from("400")],
                    authorlinks: Vec::new(),
                    settings,
                    usecase: None,
                    compatiblewith: None,
                    assets: None,
                    download_link: None,
                },
                metadata: false,
                index: true,
            });
        }
        Ok((found, broken))
    }
}

pub fn parse_metadata(
    static_root: &Path,
    metadata_path: &Path,
    settings_store: &SettingsStore,
) -> Result<Counter, CounterError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(metadata_path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_METADATA_BYTES as u64 {
        return Err(CounterError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "metadata file exceeds limit",
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(CounterError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "metadata file exceeds limit",
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| CounterError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    let metadata = vosu_plugin_api::manifest::legacy_metadata::LegacyMetadata::parse(&text);
    let directory = metadata_path.parent().ok_or(CounterError::InvalidName)?;
    let folder_name = relative_folder(static_root, directory)?;
    let settings = load_discovered_settings(settings_store, &folder_name)?;
    Ok(Counter {
        downloaded: false,
        updatable: false,
        folder_name,
        name: metadata.name,
        author: metadata.author,
        version: metadata.version,
        resolution: metadata.resolution,
        authorlinks: metadata.authorlinks,
        settings,
        usecase: None,
        compatiblewith: None,
        assets: None,
        download_link: None,
    })
}

fn recursive_named_files(
    root: &Path,
    filename: &str,
    immediate_ignore: bool,
) -> Result<Vec<PathBuf>, CounterError> {
    fn walk(
        dir: &Path,
        filename: &str,
        honor_ignore: bool,
        allow_top_level_alias: bool,
        active_directories: &mut std::collections::HashSet<PathBuf>,
        out: &mut Vec<(SystemTime, PathBuf)>,
    ) -> io::Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            if allow_top_level_alias && name.to_string_lossy().eq_ignore_ascii_case("tools") {
                continue;
            }
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let is_symlink = metadata.file_type().is_symlink();
            if filename == "manifest.toml" && name == filename {
                out.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path));
                if out.len() > MAX_DISCOVERY_FILES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "counter discovery limit exceeded",
                    ));
                }
                continue;
            }
            let is_directory = if is_symlink {
                match fs::metadata(&path) {
                    Ok(target_metadata) => target_metadata.is_dir(),
                    Err(_) => false,
                }
            } else {
                metadata.is_dir()
            };
            if is_symlink && !allow_top_level_alias {
                continue;
            }
            if is_directory {
                if name == "node_modules" {
                    continue;
                }
                if honor_ignore && path.join("ignore.txt").exists() {
                    continue;
                }
                let canonical = match fs::canonicalize(&path) {
                    Ok(canonical) => canonical,
                    Err(_) if is_symlink => continue,
                    Err(error) => return Err(error),
                };
                if active_directories.insert(canonical.clone()) {
                    let result = walk(&path, filename, false, false, active_directories, out);
                    active_directories.remove(&canonical);
                    result?;
                }
            } else if !is_symlink && name == filename {
                out.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path));
                if out.len() > MAX_DISCOVERY_FILES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "counter discovery limit exceeded",
                    ));
                }
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    let mut active_directories = std::collections::HashSet::from([fs::canonicalize(root)?]);
    walk(
        root,
        filename,
        immediate_ignore,
        true,
        &mut active_directories,
        &mut found,
    )?;
    found.sort_by(|(left_time, left_path), (right_time, right_path)| {
        left_time
            .cmp(right_time)
            .then_with(|| left_path.cmp(right_path))
    });
    Ok(found.into_iter().map(|(_, path)| path).collect())
}

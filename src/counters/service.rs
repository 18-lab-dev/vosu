use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use arc_swap::ArcSwap;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::model::{Counter, CounterSetting, DownloadRequest, SettingInput};
use super::settings::{SettingsError, SettingsStore, validate_component};

const USER_AGENT: &str = concat!(
    "vosu/",
    env!("CARGO_PKG_VERSION"),
    " (https://vosu.app; i@kotrik.ru)"
);
type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub trait CounterOpener: Send + Sync {
    fn open(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct SystemOpener;
impl CounterOpener for SystemOpener {
    fn open(&self, path: &Path) -> io::Result<()> {
        open::that_detached(path).map_err(io::Error::other)
    }
}

pub trait CounterDownloader: Send + Sync {
    fn download_and_install(
        &self,
        request: DownloadRequest,
        destination: PathBuf,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<(), CounterError>>;
}

#[derive(Clone)]
pub struct HardenedCounterDownloader {
    client: crate::net::streaming::StreamingClient,
    cache_root: PathBuf,
}

impl HardenedCounterDownloader {
    pub fn production(cache_root: PathBuf) -> Self {
        Self {
            client: crate::net::streaming::StreamingClient::production(),
            cache_root,
        }
    }

    #[cfg(feature = "test-harness")]
    pub fn with_client(
        client: crate::net::streaming::StreamingClient,
        cache_root: PathBuf,
    ) -> Self {
        Self { client, cache_root }
    }
}

impl CounterDownloader for HardenedCounterDownloader {
    fn download_and_install(
        &self,
        request: DownloadRequest,
        destination: PathBuf,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<(), CounterError>> {
        let client = self.client.clone();
        let cache_root = self.cache_root.clone();
        Box::pin(async move {
            fs::create_dir_all(&cache_root)?;
            let archive = tempfile::Builder::new()
                .prefix("counter-")
                .suffix(".zip")
                .tempfile_in(&cache_root)?;
            let archive_path = archive.path().to_path_buf();
            client
                .download_atomic(&request.url, USER_AGENT, &archive_path, &cancel)
                .await?;
            tokio::task::spawn_blocking(move || {
                crate::net::streaming::extract_zip_atomic(
                    &archive_path,
                    &destination,
                    crate::net::streaming::ZipLimits::default(),
                    &cancel,
                )
            })
            .await
            .map_err(|error| CounterError::Download(error.to_string()))??;
            Ok(())
        })
    }
}

#[derive(Debug, Error)]
pub enum CounterError {
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error("counter I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("counter JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("counter name must be one normal path component")]
    InvalidName,
    #[error("counter path is a symbolic link")]
    Symlink,
    #[error("Folder already exist")]
    AlreadyExists,
    #[error("Folder doesn't exists")]
    Missing,
    #[error("invalid percent encoding")]
    InvalidEncoding,
    #[error("download failed: {0}")]
    Download(String),
    #[error(transparent)]
    Network(#[from] crate::net::streaming::NetworkError),
    #[error("counter service is shutting down")]
    ShuttingDown,
}

struct ActiveInstall {
    count: Arc<AtomicUsize>,
    done: Arc<tokio::sync::Notify>,
}

impl Drop for ActiveInstall {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
        self.done.notify_waiters();
    }
}

/// Lock-free facade for the counter tree selected by the current configuration.
///
/// Each operation captures one complete `CounterService` generation. A reload
/// therefore affects only subsequent operations; operations already in flight
/// retain their old `Arc` until completion.
pub struct ReloadableCounterService {
    current: ArcSwap<CounterService>,
    retired: std::sync::Mutex<Vec<Arc<CounterService>>>,
    pending: tokio::sync::Mutex<HashMap<String, u64>>,
}

impl ReloadableCounterService {
    pub fn production(static_root: PathBuf, settings_root: PathBuf, cache_root: PathBuf) -> Self {
        Self::from_service(CounterService::production(
            static_root,
            settings_root,
            cache_root,
        ))
    }

    pub fn from_service(service: CounterService) -> Self {
        Self {
            current: ArcSwap::from_pointee(service),
            retired: std::sync::Mutex::new(Vec::new()),
            pending: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn reload_static_root(&self, static_root: PathBuf) {
        let current = self.current.load_full();
        if current.static_root == static_root {
            return;
        }
        let replacement = Arc::new(CounterService::new(
            static_root,
            current.settings.settings_root().to_owned(),
            Arc::clone(&current.opener),
            Arc::clone(&current.downloader),
        ));
        let retired = self.current.swap(replacement);
        self.retired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(retired);
    }

    pub fn current_root(&self) -> PathBuf {
        self.current.load().static_root.clone()
    }
    #[cfg(test)]
    pub async fn pending_save_generation(&self, name: &str) -> Option<u64> {
        self.pending.lock().await.get(name).copied()
    }

    pub fn discover(&self) -> Result<Vec<Counter>, CounterError> {
        self.current.load_full().discover()
    }

    pub async fn download(&self, request: &DownloadRequest) -> Result<(), CounterError> {
        self.current.load_full().download(request).await
    }

    pub fn open_root(&self) -> Result<(), CounterError> {
        self.current.load_full().open_root()
    }

    pub fn open(&self, name: &str) -> Result<(), CounterError> {
        self.current.load_full().open(name)
    }

    pub fn delete(&self, name: &str) -> Result<(), CounterError> {
        self.current.load_full().delete(name)
    }

    pub fn get_settings(
        &self,
        name: &str,
    ) -> Result<(Vec<CounterSetting>, serde_json::Map<String, Value>), CounterError> {
        self.current.load_full().get_settings(name)
    }

    pub fn get_counter_values(
        &self,
        name: &str,
    ) -> Result<serde_json::Map<String, Value>, CounterError> {
        self.current.load_full().get_counter_values(name)
    }

    pub fn save_developer_settings(
        &self,
        name: &str,
        settings: Vec<CounterSetting>,
        update_query: Option<&str>,
    ) -> Result<Vec<CounterSetting>, CounterError> {
        self.current
            .load_full()
            .save_developer_settings(name, settings, update_query)
    }

    pub async fn save_user_settings_debounced(
        &self,
        name: String,
        payload: Vec<SettingInput>,
    ) -> Result<bool, CounterError> {
        validate_component(&name).map_err(|_| CounterError::InvalidName)?;
        let service = self.current.load_full();
        let generation = {
            let mut pending = self.pending.lock().await;
            let generation = pending.get(&name).copied().unwrap_or(0).wrapping_add(1);
            pending.insert(name.clone(), generation);
            generation
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
        {
            let mut pending = self.pending.lock().await;
            if pending.get(&name) != Some(&generation) {
                return Ok(false);
            }
            pending.remove(&name);
        }
        service.settings().save_user_values(&name, &payload)?;
        Ok(true)
    }

    pub fn save_user_values(
        &self,
        name: &str,
        payload: &[SettingInput],
    ) -> Result<serde_json::Map<String, Value>, CounterError> {
        Ok(self
            .current
            .load_full()
            .settings()
            .save_user_values(name, payload)?)
    }

    pub async fn shutdown(&self) {
        let current = self.current.load_full();
        let retired = {
            let mut retired = self
                .retired
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *retired)
        };
        current.shutdown().await;
        for service in retired {
            service.shutdown().await;
        }
    }
}

#[derive(Clone)]
pub struct CounterService {
    static_root: PathBuf,
    settings: SettingsStore,
    opener: Arc<dyn CounterOpener>,
    downloader: Arc<dyn CounterDownloader>,
    pending: Arc<tokio::sync::Mutex<HashMap<String, u64>>>,
    cancel: CancellationToken,
    accepting_downloads: Arc<AtomicBool>,
    install_gate: Arc<tokio::sync::Mutex<()>>,
    active_installs: Arc<AtomicUsize>,
    install_done: Arc<tokio::sync::Notify>,
}

impl CounterService {
    pub fn production(static_root: PathBuf, settings_root: PathBuf, cache_root: PathBuf) -> Self {
        Self::new(
            static_root,
            settings_root,
            Arc::new(SystemOpener),
            Arc::new(HardenedCounterDownloader::production(cache_root)),
        )
    }

    pub fn new(
        static_root: PathBuf,
        settings_root: PathBuf,
        opener: Arc<dyn CounterOpener>,
        downloader: Arc<dyn CounterDownloader>,
    ) -> Self {
        Self {
            settings: SettingsStore::new(static_root.clone(), settings_root),
            static_root,
            opener,
            downloader,
            pending: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cancel: CancellationToken::new(),
            accepting_downloads: Arc::new(AtomicBool::new(true)),
            install_gate: Arc::new(tokio::sync::Mutex::new(())),
            active_installs: Arc::new(AtomicUsize::new(0)),
            install_done: Arc::new(tokio::sync::Notify::new()),
        }
    }

    pub fn settings(&self) -> &SettingsStore {
        &self.settings
    }

    pub fn discover(&self) -> Result<Vec<Counter>, CounterError> {
        let metadata = recursive_named_files(&self.static_root, "metadata.txt", true)?;
        let indexes = recursive_named_files(&self.static_root, "index.html", true)?;
        let metadata_dirs: std::collections::HashSet<_> = metadata
            .iter()
            .filter_map(|path| path.parent())
            .map(Path::to_path_buf)
            .collect();
        let mut counters = Vec::new();
        for path in metadata {
            let counter = parse_metadata(&self.static_root, &path, &self.settings)?;
            if !counter.name.is_empty() {
                counters.push(counter);
            }
        }
        for path in indexes {
            let directory = path.parent().expect("index has parent");
            if metadata_dirs.contains(directory) {
                continue;
            }
            let folder_name = relative_folder(&self.static_root, directory)?;
            let settings = load_discovered_settings(&self.settings, &folder_name)?;
            counters.push(Counter {
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
            });
        }
        Ok(counters)
    }

    pub fn parse_download_request(
        raw_url_capture: &str,
        name_query: Option<&str>,
        update_query: Option<&str>,
    ) -> Result<DownloadRequest, CounterError> {
        let name = name_query.ok_or(CounterError::InvalidName)?;
        validate_component(name).map_err(|_| CounterError::InvalidName)?;
        Ok(DownloadRequest {
            url: percent_decode_once(raw_url_capture)?,
            name: name.to_owned(),
            update: update_query == Some("true"),
        })
    }

    pub async fn download(&self, request: &DownloadRequest) -> Result<(), CounterError> {
        validate_component(&request.name).map_err(|_| CounterError::InvalidName)?;
        let destination = self.safe_counter_path(&request.name, false)?;
        if destination.exists() && !request.update {
            return Err(CounterError::AlreadyExists);
        }
        if !self.accepting_downloads.load(Ordering::Acquire) {
            return Err(CounterError::ShuttingDown);
        }
        let gate = self.install_gate.lock().await;
        if !self.accepting_downloads.load(Ordering::Acquire) {
            return Err(CounterError::ShuttingDown);
        }
        let downloader = self.downloader.clone();
        let request = request.clone();
        let cancel = self.cancel.child_token();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.active_installs.fetch_add(1, Ordering::AcqRel);
        let active = ActiveInstall {
            count: self.active_installs.clone(),
            done: self.install_done.clone(),
        };
        tokio::spawn(async move {
            let _active = active;
            let result = downloader
                .download_and_install(request, destination, cancel)
                .await;
            let _ = result_tx.send(result);
        });
        drop(gate);
        result_rx.await.map_err(|_| CounterError::ShuttingDown)?
    }

    pub async fn shutdown(&self) {
        self.accepting_downloads.store(false, Ordering::Release);
        self.cancel.cancel();
        let gate = self.install_gate.lock().await;
        drop(gate);
        loop {
            let done = self.install_done.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if self.active_installs.load(Ordering::Acquire) == 0 {
                break;
            }
            done.await;
        }
    }

    pub fn open_root(&self) -> Result<(), CounterError> {
        self.opener.open(&self.static_root)?;
        Ok(())
    }

    pub fn open(&self, name: &str) -> Result<(), CounterError> {
        let path = self.openable_counter_path(name)?;
        self.opener.open(&path)?;
        Ok(())
    }

    pub fn delete(&self, name: &str) -> Result<(), CounterError> {
        validate_component(name).map_err(|_| CounterError::InvalidName)?;
        let candidate = self.static_root.join(name);
        if let Ok(metadata) = fs::symlink_metadata(&candidate)
            && metadata.file_type().is_symlink()
        {
            remove_counter_symlink(&candidate, &metadata.file_type())?;
            return Ok(());
        }
        let path = self.safe_counter_path(name, true)?;
        if path.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn get_settings(
        &self,
        name: &str,
    ) -> Result<(Vec<CounterSetting>, serde_json::Map<String, Value>), CounterError> {
        Ok(self.settings.load(name)?)
    }

    pub fn get_counter_values(
        &self,
        name: &str,
    ) -> Result<serde_json::Map<String, Value>, CounterError> {
        Ok(self.settings.counter_values(name)?)
    }

    pub fn save_developer_settings(
        &self,
        name: &str,
        settings: Vec<CounterSetting>,
        update_query: Option<&str>,
    ) -> Result<Vec<CounterSetting>, CounterError> {
        if update_query != Some("yes") {
            return Err(CounterError::InvalidName);
        }
        Ok(self.settings.save_developer_settings(name, settings)?)
    }

    pub async fn save_user_settings_debounced(
        &self,
        name: String,
        payload: Vec<SettingInput>,
    ) -> Result<bool, CounterError> {
        validate_component(&name).map_err(|_| CounterError::InvalidName)?;
        let generation = {
            let mut pending = self.pending.lock().await;
            let generation = pending.get(&name).copied().unwrap_or(0).wrapping_add(1);
            pending.insert(name.clone(), generation);
            generation
        };
        tokio::time::sleep(Duration::from_millis(500)).await;
        {
            let mut pending = self.pending.lock().await;
            if pending.get(&name) != Some(&generation) {
                return Ok(false);
            }
            pending.remove(&name);
        }
        self.settings.save_user_values(&name, &payload)?;
        Ok(true)
    }

    fn openable_counter_path(&self, name: &str) -> Result<PathBuf, CounterError> {
        validate_component(name).map_err(|_| CounterError::InvalidName)?;
        let path = self.static_root.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(CounterError::Missing);
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return if fs::metadata(&path).is_ok_and(|target| target.is_dir()) {
                Ok(path)
            } else {
                Err(CounterError::Symlink)
            };
        }
        self.safe_counter_path(name, true)
    }

    fn safe_counter_path(&self, name: &str, must_exist: bool) -> Result<PathBuf, CounterError> {
        validate_component(name).map_err(|_| CounterError::InvalidName)?;
        let path = self.static_root.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return if must_exist {
                    Err(CounterError::Missing)
                } else {
                    Ok(path)
                };
            }
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(CounterError::Symlink);
        }
        let root = fs::canonicalize(&self.static_root)?;
        let canonical = fs::canonicalize(&path)?;
        if canonical.parent() != Some(root.as_path()) {
            return Err(CounterError::InvalidName);
        }
        Ok(path)
    }
}

#[cfg(not(windows))]
fn remove_counter_symlink(path: &Path, _file_type: &fs::FileType) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_counter_symlink(path: &Path, file_type: &fs::FileType) -> io::Result<()> {
    use std::os::windows::fs::FileTypeExt;

    if file_type.is_symlink_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

pub fn parse_metadata(
    static_root: &Path,
    metadata_path: &Path,
    settings_store: &SettingsStore,
) -> Result<Counter, CounterError> {
    let text = fs::read_to_string(metadata_path)?;
    let mut fields = HashMap::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let value = rest
            .split("##")
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        fields.insert(key.to_lowercase(), value);
    }
    let directory = metadata_path.parent().ok_or(CounterError::InvalidName)?;
    let folder_name = relative_folder(static_root, directory)?;
    let settings = load_discovered_settings(settings_store, &folder_name)?;
    let resolution = fields
        .remove("resolution")
        .map(|value| {
            if dimensions(&value) {
                value
                    .split(['x', 'X'])
                    .map(|part| {
                        part.trim()
                            .parse::<i64>()
                            .map(Value::from)
                            .unwrap_or(Value::Null)
                    })
                    .collect()
            } else {
                vec![Value::from("Any"), Value::from("Any")]
            }
        })
        .unwrap_or_else(|| vec![Value::from("Any"), Value::from("Any")]);
    let authorlinks = fields
        .remove("authorlinks")
        .map(|value| value.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    Ok(Counter {
        downloaded: false,
        updatable: false,
        folder_name,
        name: fields.remove("name").unwrap_or_default(),
        author: fields.remove("author").unwrap_or_default(),
        version: fields
            .remove("version")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "1.0".into()),
        resolution,
        authorlinks,
        settings,
        usecase: None,
        compatiblewith: None,
        assets: None,
        download_link: None,
    })
}

fn dimensions(value: &str) -> bool {
    let Some((left, right)) = value.split_once(['x', 'X']) else {
        return false;
    };
    !left.trim().is_empty()
        && !right.trim().is_empty()
        && left.trim().chars().all(|ch| ch.is_ascii_digit())
        && right
            .trim()
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '-')
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
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            let is_symlink = metadata.file_type().is_symlink();
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

fn relative_folder(root: &Path, directory: &Path) -> Result<String, CounterError> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| CounterError::InvalidName)?;
    Ok(relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn load_discovered_settings(
    store: &SettingsStore,
    folder_name: &str,
) -> Result<Vec<CounterSetting>, CounterError> {
    if validate_component(folder_name).is_err() {
        return Ok(Vec::new());
    }
    match store.load(folder_name) {
        Ok((settings, _)) => Ok(settings),
        Err(SettingsError::UnsafeSettingsFile) => Err(CounterError::Symlink),
        Err(error) => Err(error.into()),
    }
}

fn percent_decode_once(raw: &str) -> Result<String, CounterError> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(CounterError::InvalidEncoding);
            }
            let high = hex(bytes[index + 1]).ok_or(CounterError::InvalidEncoding)?;
            let low = hex(bytes[index + 2]).ok_or(CounterError::InvalidEncoding)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| CounterError::InvalidEncoding)
}
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

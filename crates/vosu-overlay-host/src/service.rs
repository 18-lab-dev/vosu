use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::model::{BrokenOverlay, Counter, CounterSetting, DownloadRequest, SettingInput};
use super::settings::{SettingsError, SettingsStore, validate_component};

mod discovery;
mod manifest;
pub use discovery::parse_metadata;
pub use manifest::InstalledPackages;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

fn user_agent() -> String {
    format!(
        "vosu/{} (+https://github.com/18-lab-dev/vosu)",
        crate::host_version()
    )
}

pub trait CounterDownloader: Send + Sync {
    fn download_and_install(
        &self,
        request: DownloadRequest,
        destination: PathBuf,
        cancel: CancellationToken,
    ) -> BoxFuture<Result<(), CounterError>>;
}

/// Public counter downloads: HTTPS-only production policy, no source
/// credential or private-network exception (ADR 0023), hardened extraction.
#[derive(Clone)]
pub struct HardenedCounterDownloader {
    client: vosu_net::streaming::StreamingClient,
    cache_root: PathBuf,
}

impl HardenedCounterDownloader {
    pub fn production(cache_root: PathBuf) -> Self {
        Self {
            client: vosu_net::streaming::StreamingClient::production(),
            cache_root,
        }
    }

    #[cfg(feature = "test-harness")]
    pub fn with_client(client: vosu_net::streaming::StreamingClient, cache_root: PathBuf) -> Self {
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
            vosu_net::publish::create_directories(&cache_root)?;
            // Scratch archive in the download cache, never under the overlay
            // root; removed when `archive` drops on every path.
            let archive = tempfile::Builder::new()
                .prefix("counter-")
                .suffix(".zip")
                .tempfile_in(&cache_root)?;
            let archive_path = archive.path().to_path_buf();
            client
                .download_atomic(&request.url, &user_agent(), &archive_path, &cancel)
                .await
                .map_err(|error| attribute_to_host(error, &request.url))?;
            // Same-task read of this own just-downloaded archive after our
            // own publish: excluded per `read_published`'s doc comment.
            tokio::task::spawn_blocking(move || {
                vosu_net::streaming::extract_zip_atomic(
                    &archive_path,
                    &destination,
                    vosu_net::streaming::ZipLimits::default(),
                    &cancel,
                )
            })
            .await
            .map_err(|error| CounterError::Download(error.to_string()))?
            .map_err(|error| attribute_to_host(error, &request.url))?;
            Ok(())
        })
    }
}

fn download_host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "the download host".to_owned())
}

/// Names the host for failures it caused, including a body with no ZIP archive
/// in it: osuck.link answers rate limits with an HTTP 200 text page, which
/// users would otherwise report to vosu. Policy rejections (scheme, private
/// destination, downgrade, size), unsafe archives, and local I/O stay vosu's own
/// errors.
fn attribute_to_host(error: vosu_net::streaming::NetworkError, url: &str) -> CounterError {
    use vosu_net::streaming::NetworkError;
    match error {
        NetworkError::Status(_)
        | NetworkError::Timeout
        | NetworkError::Request(_)
        | NetworkError::Resolve(_)
        | NetworkError::RedirectLocation
        | NetworkError::RedirectLimit
        | NetworkError::NotZip => CounterError::Host {
            host: download_host(url),
            reason: error.to_string(),
        },
        other => CounterError::Network(other),
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
    #[error("{host} (not operated by vosu): {reason}")]
    Host { host: String, reason: String },
    #[error(transparent)]
    Network(#[from] vosu_net::streaming::NetworkError),
    #[error("counter install capacity is full")]
    InstallBusy,
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
    pending: tokio::sync::Mutex<HashMap<String, u64>>,
    names: super::names::OverlayNames,
}

impl ReloadableCounterService {
    pub fn production(static_root: PathBuf, settings_root: PathBuf, cache_root: PathBuf) -> Self {
        Self::from_service(CounterService::production(
            static_root,
            settings_root,
            cache_root,
        ))
    }

    /// Overlay names live beside the settings directory, in the config root.
    pub fn from_service(service: CounterService) -> Self {
        let settings_root = service.settings.settings_root();
        let names = super::names::OverlayNames::load(
            settings_root
                .parent()
                .unwrap_or(settings_root)
                .join("overlay-names.json"),
        );
        Self {
            current: ArcSwap::from_pointee(service),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            names,
        }
    }

    pub fn names(&self) -> &super::names::OverlayNames {
        &self.names
    }

    /// Composition's M6b installed-graph validation input (no core handle).
    pub fn set_installed_packages(&self, packages: Arc<dyn InstalledPackages>) {
        *self
            .current
            .load()
            .installed
            .write()
            .unwrap_or_else(|p| p.into_inner()) = packages;
    }

    /// Applies a complete effective settings object (A7.1-OR): the
    /// `static_folder_path` setting selects the root for later operations.
    /// Operations already in flight, including downloads, keep the root they
    /// started with. The schema default (no root yet) keeps the current one.
    pub fn apply_settings(
        &self,
        settings: &vosu_plugin_api::CborValue,
    ) -> Result<(), crate::root::RootSettingError> {
        if let Some(root) = crate::root::from_cbor(settings)? {
            self.reload_static_root(root);
        }
        Ok(())
    }

    /// Points a nickname/alias change at the current static root.
    pub fn set_name(
        &self,
        folder: &str,
        nickname: Option<&str>,
        alias: Option<&str>,
    ) -> Result<super::names::OverlayName, String> {
        self.names
            .set(&self.current.load().static_root, folder, nickname, alias)
    }

    pub fn reload_static_root(&self, static_root: PathBuf) {
        let current = self.current.load_full();
        if current.static_root == static_root {
            return;
        }
        // Root generations change only filesystem selection. Admission,
        // cancellation and join accounting remain daemon-wide, including
        // detached installs whose initiating HTTP request was dropped.
        let replacement = Arc::new(CounterService {
            settings: SettingsStore::new(
                static_root.clone(),
                current.settings.settings_root().to_owned(),
            ),
            static_root,
            ..(*current).clone()
        });
        self.current.store(replacement);
    }

    pub fn current_root(&self) -> PathBuf {
        self.current.load().static_root.clone()
    }
    #[cfg(test)]
    pub async fn pending_save_generation(&self, name: &str) -> Option<u64> {
        self.pending.lock().await.get(name).copied()
    }

    /// Lists readable overlays and drops names of folders that no longer exist.
    pub fn discover(&self) -> Result<Vec<Counter>, CounterError> {
        Ok(self.discover_all()?.0)
    }

    /// Readable overlays plus the folders that failed to parse. Names of
    /// broken folders are kept: fixing the file restores the overlay as it was.
    pub fn discover_all(&self) -> Result<(Vec<Counter>, Vec<BrokenOverlay>), CounterError> {
        let (counters, broken) = self.current.load_full().discover_all()?;
        if let Err(error) = self.names.retain_folders(
            counters
                .iter()
                .map(|counter| counter.folder_name.as_str())
                .chain(broken.iter().map(|overlay| overlay.folder_name.as_str())),
        ) {
            tracing::warn!(%error, "Could not prune overlay names");
        }
        Ok((counters, broken))
    }

    pub async fn download(&self, request: &DownloadRequest) -> Result<(), CounterError> {
        self.current.load_full().download(request).await
    }

    pub fn delete(&self, name: &str) -> Result<(), CounterError> {
        self.current.load_full().delete(name)?;
        if let Err(error) = self.names.remove(name) {
            tracing::warn!(%error, "Could not drop the deleted overlay's name");
        }
        Ok(())
    }

    pub fn details(&self, name: &str) -> Result<Value, CounterError> {
        self.current.load_full().details(name)
    }

    pub fn settings_store(&self) -> SettingsStore {
        self.current.load_full().settings.clone()
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
        validate_component(super::settings::split_counter(&name).0)
            .map_err(|_| CounterError::InvalidName)?;
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
        self.current.load_full().shutdown().await;
    }
}

#[derive(Clone)]
pub struct CounterService {
    static_root: PathBuf,
    settings: SettingsStore,
    downloader: Arc<dyn CounterDownloader>,
    pending: Arc<tokio::sync::Mutex<HashMap<String, u64>>>,
    cancel: CancellationToken,
    accepting_downloads: Arc<AtomicBool>,
    install_gate: Arc<tokio::sync::Mutex<()>>,
    active_installs: Arc<AtomicUsize>,
    install_done: Arc<tokio::sync::Notify>,
    install_slots: Arc<tokio::sync::Semaphore>,
    installed: Arc<std::sync::RwLock<Arc<dyn InstalledPackages>>>,
}

impl CounterService {
    pub fn production(static_root: PathBuf, settings_root: PathBuf, cache_root: PathBuf) -> Self {
        Self::new(
            static_root,
            settings_root,
            Arc::new(HardenedCounterDownloader::production(cache_root)),
        )
    }

    pub fn new(
        static_root: PathBuf,
        settings_root: PathBuf,
        downloader: Arc<dyn CounterDownloader>,
    ) -> Self {
        Self {
            settings: SettingsStore::new(static_root.clone(), settings_root),
            static_root,
            downloader,
            pending: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cancel: CancellationToken::new(),
            accepting_downloads: Arc::new(AtomicBool::new(true)),
            install_gate: Arc::new(tokio::sync::Mutex::new(())),
            active_installs: Arc::new(AtomicUsize::new(0)),
            install_done: Arc::new(tokio::sync::Notify::new()),
            install_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            installed: Arc::new(std::sync::RwLock::new(Arc::new(manifest::OverlaysOnly))),
        }
    }

    pub fn settings(&self) -> &SettingsStore {
        &self.settings
    }

    /// Readable overlays only; [`Self::discover_all`] also reports the
    /// folders whose files could not be read.
    pub fn discover(&self) -> Result<Vec<Counter>, CounterError> {
        Ok(self.scan()?.0)
    }

    /// Readable overlays and, separately, overlay folders whose metadata or
    /// settings failed to parse. One broken folder never hides the others;
    /// only a failure to walk the overlays folder itself is an error.
    pub fn discover_all(&self) -> Result<(Vec<Counter>, Vec<BrokenOverlay>), CounterError> {
        self.scan()
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
        let permit = self
            .install_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| CounterError::InstallBusy)?;
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
            let _permit = permit;
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

    /// Bounded metadata-only scan. Links are never followed, including a linked root.
    pub fn details(&self, name: &str) -> Result<Value, CounterError> {
        validate_component(name).map_err(|_| CounterError::InvalidName)?;
        let path = fs::canonicalize(&self.static_root)?.join(name);
        let metadata = fs::symlink_metadata(&path)?;
        let mut scan = FolderSize::new(10_000, 32);
        if metadata.is_dir() && !metadata_is_link(&metadata) {
            if scan.directory(&path, 0).is_err() {
                scan.complete = false;
            }
        } else {
            scan.bytes = metadata.len();
        }
        Ok(serde_json::json!({
            "path": display_path(&path),
            "bytes": scan.bytes,
            "complete": scan.complete,
            "entries": scan.entries,
        }))
    }

    pub fn delete(&self, name: &str) -> Result<(), CounterError> {
        validate_component(name).map_err(|_| CounterError::InvalidName)?;
        let candidate = self.static_root.join(name);
        if let Ok(metadata) = fs::symlink_metadata(&candidate)
            && metadata.file_type().is_symlink()
        {
            // A linked folder is a separately confined mount: unlink only.
            vosu_net::publish::remove_tree(&candidate)?;
            return Ok(());
        }
        vosu_net::publish::remove_tree(&self.safe_counter_path(name, true)?)?;
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

const MAX_METADATA_BYTES: usize = 64 * 1024;

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

struct FolderSize {
    bytes: u64,
    entries: usize,
    max_entries: usize,
    max_depth: usize,
    deadline: std::time::Instant,
    complete: bool,
}

fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes junctions and other reparse points, not only symbolic links.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// The canonical path without Windows' verbatim prefix (`\\?\C:\…` → `C:\…`,
/// `\\?\UNC\server\share\…` → `\\server\share\…`), which users never type and the
/// dashboard would otherwise show.
pub(super) fn display_path(path: &Path) -> PathBuf {
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_owned();
    };
    let mut plain = match prefix.kind() {
        Prefix::VerbatimDisk(disk) => PathBuf::from(format!("{}:\\", char::from(disk))),
        Prefix::VerbatimUNC(server, share) => {
            let mut root = std::ffi::OsString::from(r"\\");
            root.push(server);
            root.push(r"\");
            root.push(share);
            root.push(r"\");
            PathBuf::from(root)
        }
        _ => return path.to_owned(),
    };
    plain.extend(components.filter(|component| *component != Component::RootDir));
    plain
}

#[cfg(all(test, windows))]
#[test]
fn display_path_drops_the_verbatim_prefix() {
    for (canonical, shown) in [
        (r"\\?\V:\vosu\static\counter", r"V:\vosu\static\counter"),
        (r"\\?\UNC\host\share\static", r"\\host\share\static"),
        (r"C:\already\plain", r"C:\already\plain"),
    ] {
        assert_eq!(display_path(Path::new(canonical)).as_os_str(), shown);
    }
}

impl FolderSize {
    fn new(max_entries: usize, max_depth: usize) -> Self {
        Self {
            bytes: 0,
            entries: 0,
            max_entries,
            max_depth,
            deadline: std::time::Instant::now() + Duration::from_millis(100),
            complete: true,
        }
    }

    fn directory(&mut self, path: &Path, depth: usize) -> io::Result<()> {
        if depth >= self.max_depth
            || self.entries >= self.max_entries
            || std::time::Instant::now() >= self.deadline
        {
            self.complete = false;
            return Ok(());
        }
        // Hold a no-follow directory descriptor while enumerating. Child paths
        // remain anchored to that descriptor even if an installer renames it.
        #[cfg(target_os = "linux")]
        let directory = {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(path)?
        };
        #[cfg(target_os = "linux")]
        let path = {
            use std::os::fd::AsRawFd;
            PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()))
        };
        #[cfg(windows)]
        let directory = {
            use std::os::windows::fs::OpenOptionsExt;
            // Open the reparse point itself, and deny delete/rename sharing
            // while this directory and its descendants are enumerated.
            let directory = OpenOptions::new()
                .read(true)
                .share_mode(0x1 | 0x2)
                .custom_flags(0x0200_0000 | 0x0020_0000)
                .open(path)?;
            if metadata_is_link(&directory.metadata()?) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "linked directory",
                ));
            }
            directory
        };
        #[cfg(windows)]
        let _directory_guard = directory;
        for entry in fs::read_dir(path)? {
            if self.entries >= self.max_entries || std::time::Instant::now() >= self.deadline {
                self.complete = false;
                break;
            }
            self.entries += 1;
            let Ok(entry) = entry else {
                self.complete = false;
                continue;
            };
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                self.complete = false;
                continue;
            };
            if metadata.is_dir() && !metadata_is_link(&metadata) {
                if self.directory(&entry.path(), depth + 1).is_err() {
                    self.complete = false;
                }
            } else {
                self.bytes = self.bytes.saturating_add(metadata.len());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod folder_size_tests {
    use super::*;

    #[test]
    fn scan_reports_bytes_and_entry_depth_and_time_bounds() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("a"), b"12345").unwrap();
        fs::write(root.path().join("b"), b"123").unwrap();
        let mut all = FolderSize::new(10, 10);
        all.directory(root.path(), 0).unwrap();
        assert_eq!(all.bytes, 8);
        assert!(all.complete);
        let mut entries = FolderSize::new(1, 10);
        entries.directory(root.path(), 0).unwrap();
        assert_eq!(entries.entries, 1);
        assert!(!entries.complete);
        let mut depth = FolderSize::new(10, 0);
        depth.directory(root.path(), 0).unwrap();
        assert!(!depth.complete);
        let mut timed = FolderSize::new(10, 10);
        timed.deadline = std::time::Instant::now();
        timed.directory(root.path(), 0).unwrap();
        assert!(!timed.complete);
        assert_eq!(timed.entries, 0);
    }

    #[cfg(unix)]
    #[test]
    fn scan_never_follows_links_to_outside_or_cycles() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), vec![0; 1_000_000]).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("outside")).unwrap();
        std::os::unix::fs::symlink(root.path(), root.path().join("cycle")).unwrap();
        let mut scan = FolderSize::new(10, 10);
        scan.directory(root.path(), 0).unwrap();
        assert!(scan.complete);
        assert_eq!(scan.entries, 2);
        assert!(scan.bytes < 1_000_000);
    }
}

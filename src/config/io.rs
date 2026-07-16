use super::document::ConfigDocument;
use super::schema::Config;
use arc_swap::ArcSwap;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Injectable atomic filesystem operation used by configuration and offset caches.
pub trait AtomicReplace: Send + Sync + 'static {
    fn replace(&self, temp: &Path, target: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct SystemAtomicReplace;

impl AtomicReplace for SystemAtomicReplace {
    fn replace(&self, temp: &Path, target: &Path) -> io::Result<()> {
        crate::platform::atomic_replace(temp, target)
    }
}

#[derive(Clone)]
pub struct ConfigStoreOptions {
    pub debounce: Duration,
    pub atomic_replace: Arc<dyn AtomicReplace>,
}

impl Default for ConfigStoreOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(100),
            atomic_replace: Arc::new(SystemAtomicReplace),
        }
    }
}

pub(super) fn persist_current(
    path: &Path,
    current: &ArcSwap<Config>,
    replacer: &dyn AtomicReplace,
    current_hash: &mut [u8; 32],
) -> io::Result<()> {
    let bytes = ConfigDocument::new((*current.load_full()).clone())
        .serialize()
        .into_bytes();
    atomic_write(path, &bytes, replacer)?;
    *current_hash = hash_bytes(&bytes);
    Ok(())
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn atomic_write(
    path: &Path,
    bytes: &[u8],
    replacer: &dyn AtomicReplace,
) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("vosu.env");
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        replacer.replace(&temp, path)
    })();
    if let Err(cleanup) = std::fs::remove_file(&temp)
        && cleanup.kind() != io::ErrorKind::NotFound
        && result.is_ok()
    {
        return Err(cleanup);
    }
    result
}

pub(super) fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(super) fn notify_error(error: notify::Error) -> io::Error {
    io::Error::other(error)
}

pub(super) fn join_error(error: tokio::task::JoinError) -> io::Error {
    io::Error::other(error)
}

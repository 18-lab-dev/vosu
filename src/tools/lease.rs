//! The read-only `RootLease` core mints for one discovered tool package
//! (A7.1 `ToolHostView`). Every open re-resolves the package directory and
//! opens the child beneath it through `vosu-sys` confinement, so no directory
//! handle is retained between requests and a lease from a replaced or removed
//! package stops opening files.
use std::{
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use vosu_surface::files::{
    DirectoryReader, FileError, FileMetadata, FileReader, OpenedFile, Operation, RelativeChild,
    RootLease, RootOperations,
};
use vosu_sys::fs::{FileIdentity, SafePathError, SafeRelativePath, SafeRoot};

#[derive(Clone)]
pub(super) struct PackageRoot {
    path: PathBuf,
    live: Arc<AtomicBool>,
}
impl PackageRoot {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            live: Arc::new(AtomicBool::new(true)),
        }
    }
    pub(super) fn lease(&self) -> RootLease {
        RootLease::new(self.clone())
    }
    /// Called when the package leaves discovery or is replaced.
    pub(super) fn revoke(&self) {
        self.live.store(false, Ordering::Release);
    }
    pub(super) fn is_live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

fn native(error: SafePathError) -> FileError {
    match error {
        SafePathError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            FileError::NotFound
        }
        SafePathError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            FileError::PermissionDenied
        }
        SafePathError::Io(_) => FileError::Io,
        _ => FileError::InvalidPath,
    }
}

struct Handle {
    file: Mutex<std::fs::File>,
    identity: FileIdentity,
}
struct Reader(Arc<Handle>);
impl FileReader for Reader {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        let handle = Arc::clone(&self.0);
        Box::pin(async move {
            if len > vosu_surface::resources::MAX_READ_BYTES
                || offset > handle.identity.size
                || offset.checked_add(u64::from(len)).is_none()
            {
                return Err(FileError::InvalidPath);
            }
            tokio::task::spawn_blocking(move || {
                let mut file = handle.file.lock().unwrap_or_else(|p| p.into_inner());
                if FileIdentity::of(&file).map_err(|_| FileError::Io)? != handle.identity {
                    return Err(FileError::TargetGone);
                }
                file.seek(SeekFrom::Start(offset))
                    .map_err(|_| FileError::Io)?;
                let size = u64::from(len).min(handle.identity.size - offset) as usize;
                let mut bytes = vec![0; size];
                file.read_exact(&mut bytes).map_err(|_| FileError::Io)?;
                if FileIdentity::of(&file).map_err(|_| FileError::Io)? != handle.identity {
                    return Err(FileError::TargetGone);
                }
                Ok(bytes)
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }
}

impl RootOperations for PackageRoot {
    fn open(&self, path: &RelativeChild) -> Operation<'_, OpenedFile> {
        let child = SafeRelativePath::parse(path.as_str()).map_err(native);
        let root = self.clone();
        Box::pin(async move {
            let child = child?;
            tokio::task::spawn_blocking(move || {
                if !root.live.load(Ordering::Acquire) {
                    return Err(FileError::TargetGone);
                }
                let opened = SafeRoot::open(&root.path)
                    .and_then(|directory| directory.open_child(&child))
                    .map_err(native)?;
                if !opened.metadata.is_file() {
                    return Err(FileError::InvalidPath);
                }
                let identity = FileIdentity::of(&opened.file).map_err(|_| FileError::Io)?;
                Ok(OpenedFile::new(
                    FileMetadata {
                        size: identity.size,
                        modified: opened.metadata.modified().ok(),
                        identity: Arc::from(identity.encode()),
                    },
                    Reader(Arc::new(Handle {
                        file: Mutex::new(opened.file),
                        identity,
                    })),
                ))
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }
    fn directory(&self, _: &RelativeChild) -> Operation<'_, Box<dyn DirectoryReader>> {
        // Tool packages are never listed.
        Box::pin(async { Err(FileError::PermissionDenied) })
    }
}

use super::{PluginHost, RootCapability};
use crate::identity::PluginInstanceId;
use std::{
    io::{Read, Seek, SeekFrom},
    sync::{Arc, Mutex, Weak},
};
use vosu_surface::files::*;
use vosu_sys::fs::{FileIdentity, SafeChild, SafeDirectory, SafePathError, SafeRelativePath};

pub(super) fn native_error(error: SafePathError) -> FileError {
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
pub(super) fn request_error(error: crate::plugin::ResourceRequestError) -> FileError {
    use crate::plugin::ResourceRequestError as E;
    match error {
        E::NotFound => FileError::NotFound,
        E::Cancelled | E::Gone | E::TargetGone => FileError::TargetGone,
        E::InvalidRequest => FileError::InvalidPath,
        E::Busy | E::TimedOut => FileError::NotReady,
        _ => FileError::Io,
    }
}
pub(super) fn open_root(
    root: &RootCapability,
    path: &SafeRelativePath,
) -> Result<SafeChild, FileError> {
    if !root.is_current() {
        return Err(FileError::TargetGone);
    }
    let child = root.root.open_child(path).map_err(native_error)?;
    if !root.is_current() {
        return Err(FileError::TargetGone);
    }
    Ok(child)
}
#[derive(Clone)]
pub(super) struct Authority {
    pub host: Weak<PluginHost>,
    pub requester: PluginInstanceId,
}
impl Authority {
    pub fn check(&self) -> Result<(), FileError> {
        let host = self.host.upgrade().ok_or(FileError::TargetGone)?;
        let instances = host.instances.lock().unwrap();
        match instances.get(&self.requester) {
            Some(super::super::InstanceSlot::Live(entry)) if entry.publisher.bundled => Ok(()),
            _ => Err(FileError::PermissionDenied),
        }
    }
}
pub(super) struct Root {
    pub authority: Authority,
    pub root: RootCapability,
}
impl RootOperations for Root {
    fn open(&self, path: &RelativeChild) -> Operation<'_, OpenedFile> {
        let path = SafeRelativePath::parse(path.as_str()).map_err(native_error);
        let root = self.root.clone();
        let authority = self.authority.clone();
        Box::pin(async move {
            let path = path?;
            tokio::task::spawn_blocking(move || {
                authority.check()?;
                opened(open_root(&root, &path)?)
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }
    fn directory(&self, path: &RelativeChild) -> Operation<'_, Box<dyn DirectoryReader>> {
        let path = SafeRelativePath::parse(path.as_str()).map_err(native_error);
        let root = self.root.clone();
        let authority = self.authority.clone();
        Box::pin(async move {
            let path = path?;
            tokio::task::spawn_blocking(move || {
                authority.check()?;
                if !root.is_current() {
                    return Err(FileError::TargetGone);
                }
                let directory = root.root.directory(&path).map_err(native_error)?;
                if !root.is_current() {
                    return Err(FileError::TargetGone);
                }
                Ok(Box::new(Directory {
                    root,
                    authority,
                    cursor: Arc::new(Mutex::new(Cursor {
                        directory,
                        pending: None,
                        eof: false,
                    })),
                }) as Box<dyn DirectoryReader>)
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }
}
struct File {
    child: Mutex<SafeChild>,
    identity: FileIdentity,
}
struct FileHandle(Arc<File>);
pub(super) fn opened(child: SafeChild) -> Result<OpenedFile, FileError> {
    if !child.metadata.is_file() {
        return Err(FileError::InvalidPath);
    }
    let identity = FileIdentity::of(&child.file).map_err(|_| FileError::Io)?;
    let metadata = FileMetadata {
        size: identity.size,
        modified: child.metadata.modified().ok(),
        identity: Arc::from(identity.encode()),
    };
    Ok(OpenedFile::new(
        metadata,
        FileHandle(Arc::new(File {
            child: Mutex::new(child),
            identity,
        })),
    ))
}
impl FileReader for FileHandle {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        let file = Arc::clone(&self.0);
        Box::pin(async move {
            if len > vosu_surface::resources::MAX_READ_BYTES
                || offset > file.identity.size
                || offset.checked_add(u64::from(len)).is_none()
            {
                return Err(FileError::InvalidPath);
            }
            tokio::task::spawn_blocking(move || {
                let mut child = file.child.lock().unwrap();
                if FileIdentity::of(&child.file).map_err(|_| FileError::Io)? != file.identity {
                    return Err(FileError::TargetGone);
                }
                child
                    .file
                    .seek(SeekFrom::Start(offset))
                    .map_err(|_| FileError::Io)?;
                let mut bytes = vec![0; u64::from(len).min(file.identity.size - offset) as usize];
                child
                    .file
                    .read_exact(&mut bytes)
                    .map_err(|_| FileError::Io)?;
                if FileIdentity::of(&child.file).map_err(|_| FileError::Io)? != file.identity {
                    return Err(FileError::TargetGone);
                }
                Ok(bytes)
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }
}
struct Cursor {
    directory: SafeDirectory,
    pending: Option<std::ffi::OsString>,
    eof: bool,
}
struct Directory {
    root: RootCapability,
    authority: Authority,
    cursor: Arc<Mutex<Cursor>>,
}
impl DirectoryReader for Directory {
    fn next_page(&mut self) -> Operation<'_, DirectoryPage> {
        let root = self.root.clone();
        let authority = self.authority.clone();
        let cursor = Arc::clone(&self.cursor);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                authority.check()?;
                if !root.is_current() {
                    return Err(FileError::TargetGone);
                }
                let mut cursor = cursor.lock().unwrap();
                let mut entries = Vec::new();
                let mut bytes = 0usize;
                let mut examined = 0usize;
                while entries.len() < 16_384 && examined < 16_384 && !cursor.eof {
                    let name = match cursor.pending.take() {
                        Some(name) => name,
                        None => match cursor.directory.next_name().map_err(native_error)? {
                            Some(name) => name,
                            None => {
                                cursor.eof = true;
                                break;
                            }
                        },
                    };
                    examined += 1;
                    let Some(text) = name.to_str() else {
                        continue;
                    };
                    if text.len() > 4096 {
                        return Err(FileError::LimitExceeded);
                    }
                    if bytes + text.len() > vosu_plugin_api::VOSU_FRAME_MAX_BYTES as usize {
                        cursor.pending = Some(name);
                        break;
                    }
                    let path = match SafeRelativePath::parse(text) {
                        Ok(path) => path,
                        Err(_) => continue,
                    };
                    // Metadata is from a retained-directory child, not entry.path().
                    let child = match cursor.directory.open_child(&path) {
                        Ok(child) => child,
                        Err(SafePathError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                            continue;
                        }
                        Err(SafePathError::EscapesRoot) => continue,
                        Err(error) => return Err(native_error(error)),
                    };
                    let kind = if child.metadata.is_file() {
                        EntryKind::File
                    } else if child.metadata.is_dir() {
                        EntryKind::Directory
                    } else {
                        EntryKind::Other
                    };
                    bytes += text.len();
                    entries.push(DirectoryEntry {
                        name: text.to_owned(),
                        kind,
                        size: child.metadata.len(),
                    });
                }
                if !root.is_current() {
                    return Err(FileError::TargetGone);
                }
                Ok(DirectoryPage {
                    entries,
                    truncated: !cursor.eof,
                })
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }
}

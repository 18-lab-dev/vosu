//! Directory cursors enumerate an already-open object, never its old pathname.
use super::{SafeChild, SafePathError, SafeRelativePath, SafeRoot};
use std::{ffi::OsString, fs};

pub struct SafeDirectory {
    root: SafeRoot,
    names: native::Names,
}
impl SafeDirectory {
    pub(super) fn new(child: SafeChild) -> Result<Self, SafePathError> {
        if !child.metadata.is_dir() {
            return Err(SafePathError::InvalidComponent);
        }
        let root = SafeRoot::from_directory(child.file);
        let cursor = root.open_child(&SafeRelativePath::parse("")?)?;
        Ok(Self {
            root,
            names: native::Names::new(cursor.file)?,
        })
    }
    /// A bounded consumer drives this one entry at a time. Names are untrusted;
    /// callers must still use retained-root opens for metadata and file bytes.
    pub fn next_name(&mut self) -> Result<Option<OsString>, SafePathError> {
        self.names.next().map_err(Into::into)
    }
    pub fn open_child(&self, path: &SafeRelativePath) -> Result<SafeChild, SafePathError> {
        self.root.open_child(path)
    }
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{
        ffi::CStr,
        io,
        os::{fd::IntoRawFd, unix::ffi::OsStrExt},
    };
    pub struct Names(*mut libc::DIR);
    // SAFETY: DIR is exclusively owned. All iteration needs &mut self and the
    // cursor is never shared; moving ownership between threads is safe.
    unsafe impl Send for Names {}
    impl Names {
        pub fn new(file: fs::File) -> io::Result<Self> {
            let fd = file.into_raw_fd();
            // SAFETY: fd is an owned open directory descriptor.
            let dir = unsafe { libc::fdopendir(fd) };
            if dir.is_null() {
                let error = io::Error::last_os_error();
                // SAFETY: fdopendir failed and did not assume fd ownership.
                unsafe { libc::close(fd) };
                return Err(error);
            }
            Ok(Self(dir))
        }
        pub fn next(&mut self) -> io::Result<Option<OsString>> {
            loop {
                // SAFETY: errno is thread-local; self exclusively owns the DIR.
                let entry = unsafe {
                    *libc::__errno_location() = 0;
                    libc::readdir(self.0)
                };
                if entry.is_null() {
                    let error = io::Error::last_os_error();
                    return if error.raw_os_error() == Some(0) {
                        Ok(None)
                    } else {
                        Err(error)
                    };
                }
                // SAFETY: readdir returned a NUL-terminated name valid until the
                // next call. Copy it before advancing the exclusively owned DIR.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
                if name != b"." && name != b".." {
                    return Ok(Some(std::ffi::OsStr::from_bytes(name).to_owned()));
                }
            }
        }
    }
    impl Drop for Names {
        fn drop(&mut self) {
            // SAFETY: self uniquely owns this successful fdopendir result.
            unsafe { libc::closedir(self.0) };
        }
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        io,
        os::windows::{ffi::OsStringExt, io::AsRawHandle},
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_MORE_FILES, HANDLE},
        Storage::FileSystem::{
            FILE_FULL_DIR_INFO, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
            GetFileInformationByHandleEx,
        },
    };
    pub struct Names {
        file: fs::File,
        restart: bool,
        pending: Vec<OsString>,
    }
    impl Names {
        pub fn new(file: fs::File) -> io::Result<Self> {
            Ok(Self {
                file,
                restart: true,
                pending: Vec::new(),
            })
        }
        pub fn next(&mut self) -> io::Result<Option<OsString>> {
            loop {
                if let Some(name) = self.pending.pop() {
                    return Ok(Some(name));
                }
                // u64 storage ensures native structure alignment; allocation is
                // fixed at 64 KiB regardless of the number of directory entries.
                let mut buffer = vec![0u64; 8192];
                let class = if self.restart {
                    FileFullDirectoryRestartInfo
                } else {
                    FileFullDirectoryInfo
                };
                // SAFETY: file is a live directory handle; buffer is aligned,
                // writable for the supplied length, and no pointers are retained.
                let ok = unsafe {
                    GetFileInformationByHandleEx(
                        self.file.as_raw_handle() as HANDLE,
                        class,
                        buffer.as_mut_ptr().cast(),
                        (buffer.len() * 8) as u32,
                    )
                };
                if ok == 0 {
                    let error = io::Error::last_os_error();
                    return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                        Ok(None)
                    } else {
                        Err(error)
                    };
                }
                self.restart = false;
                let mut offset = 0usize;
                loop {
                    if offset + std::mem::size_of::<FILE_FULL_DIR_INFO>() > buffer.len() * 8 {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    // SAFETY: offsets start aligned and are validated within the
                    // native output buffer; the kernel initializes this record.
                    let entry = unsafe {
                        &*buffer
                            .as_ptr()
                            .cast::<u8>()
                            .add(offset)
                            .cast::<FILE_FULL_DIR_INFO>()
                    };
                    let start = offset + std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
                    let len = entry.FileNameLength as usize;
                    if !len.is_multiple_of(2) || start + len > buffer.len() * 8 {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    // SAFETY: the validated UTF-16 name lies within buffer.
                    let wide = unsafe {
                        std::slice::from_raw_parts(
                            buffer.as_ptr().cast::<u8>().add(start).cast::<u16>(),
                            len / 2,
                        )
                    };
                    let name = OsString::from_wide(wide);
                    if name != "." && name != ".." {
                        self.pending.push(name);
                    }
                    if entry.NextEntryOffset == 0 {
                        break;
                    }
                    if !(entry.NextEntryOffset as usize)
                        .is_multiple_of(std::mem::align_of::<FILE_FULL_DIR_INFO>())
                    {
                        return Err(io::ErrorKind::InvalidData.into());
                    }
                    offset += entry.NextEntryOffset as usize;
                }
                self.pending.reverse();
            }
        }
    }
}

use super::{SafeChild, SafePathError};
use std::os::windows::{
    ffi::OsStrExt,
    fs::OpenOptionsExt,
    io::{AsRawHandle, FromRawHandle},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use windows_sys::Win32::Foundation::{HANDLE, UNICODE_STRING};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};

fn final_path(file: &fs::File) -> io::Result<PathBuf> {
    let handle = file.as_raw_handle() as HANDLE;
    let required = unsafe {
        // SAFETY: null buffer with zero length is the documented size query.
        GetFinalPathNameByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u16; required as usize + 1];
    let written = unsafe {
        // SAFETY: buffer is writable for the reported size.
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(written as usize);
    let path = String::from_utf16(&buffer)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid final path"))?;
    Ok(PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(&path)))
}

pub(super) fn open_root(path: &Path) -> Result<fs::File, SafePathError> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "configured root is not a directory",
        )
        .into());
    }
    Ok(file)
}

pub(super) fn open_child(root: &fs::File, relative: &Path) -> Result<SafeChild, SafePathError> {
    use windows_sys::Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile},
    };
    use windows_sys::Win32::{
        Foundation::RtlNtStatusToDosError,
        Storage::FileSystem::{
            FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        },
        System::IO::IO_STATUS_BLOCK,
    };
    // Native relative opens bind lookup to the retained directory object, even
    // if its original pathname has since been replaced by another directory.
    // NT names use backslash separators and do not perform Win32 path cleanup.
    // An empty name reopens RootDirectory itself, rather than looking up ".".
    let mut wide: Vec<u16> = relative
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == b'/' as u16 {
                b'\\' as u16
            } else {
                unit
            }
        })
        .collect();
    let length = u16::try_from(
        wide.len()
            .checked_mul(2)
            .ok_or(SafePathError::InvalidComponent)?,
    )
    .map_err(|_| SafePathError::InvalidComponent)?;
    let mut name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: root.as_raw_handle() as HANDLE,
        ObjectName: &mut name,
        Attributes: 0x40, // OBJ_CASE_INSENSITIVE
        SecurityDescriptor: std::ptr::null_mut(),
        SecurityQualityOfService: std::ptr::null_mut(),
    };
    let mut handle = std::ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: wide owns name.Buffer; name owns attributes.ObjectName's storage;
    // both and the borrowed root handle remain live and unmodified through this
    // synchronous call. All output pointers are initialized writable storage.
    // NtCreateFile retains no pointers and returns an owned handle on success.
    // Like the previous CreateFile open, omit FILE_OPEN_REPARSE_POINT so aliases
    // resolve normally; containment is checked on the resulting opened object.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            FILE_GENERIC_READ,
            &attributes,
            &mut status_block,
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: RtlNtStatusToDosError accepts any NTSTATUS value.
        return Err(
            io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32).into(),
        );
    }
    // SAFETY: the successful native open returned a unique owned handle.
    let file = unsafe { fs::File::from_raw_handle(handle) };
    if !windows_path_is_within(&final_path(root)?, &final_path(&file)?) {
        return Err(SafePathError::EscapesRoot);
    }
    let metadata = file.metadata()?;
    Ok(SafeChild { file, metadata })
}

fn windows_path_is_within(root: &Path, child: &Path) -> bool {
    let root: Vec<_> = root
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let child: Vec<_> = child
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    child.starts_with(&root)
}

pub(super) fn root_path(file: &fs::File) -> Result<PathBuf, SafePathError> {
    Ok(final_path(file)?)
}
pub(super) fn strip_root<'a>(root: &Path, child: &'a Path) -> Option<&'a Path> {
    let text = child.to_str()?;
    let child = Path::new(text.strip_prefix(r"\\?\").unwrap_or(text));
    if !windows_path_is_within(root, child) {
        return None;
    }
    let mut components = child.components();
    for _ in root.components() {
        components.next()?;
    }
    Some(components.as_path())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_final_path_check_rejects_junction_escape_and_prefix_collision() {
        assert!(windows_path_is_within(
            Path::new(r"C:\static"),
            Path::new(r"c:\static\counter")
        ));
        assert!(!windows_path_is_within(
            Path::new(r"C:\static"),
            Path::new(r"C:\static-escape\counter")
        ));
        assert!(!windows_path_is_within(
            Path::new(r"C:\static"),
            Path::new(r"D:\outside\counter")
        ));
    }
}

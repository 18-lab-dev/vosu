use super::{SafeChild, SafePathError};
use std::{fs, io, path::Path};

pub(super) fn open_root(path: &Path) -> Result<fs::File, SafePathError> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let path =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| SafePathError::InvalidComponent)?;
    // SAFETY: path is NUL terminated; the returned descriptor is uniquely owned below.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: open returned a unique owned descriptor.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}

pub(super) fn open_child(
    root_file: &fs::File,
    relative: &Path,
) -> Result<SafeChild, SafePathError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_BENEATH: u64 = 0x08;

    let relative = if relative.as_os_str().is_empty() {
        CString::new(".").expect("static string")
    } else {
        CString::new(relative.as_os_str().as_bytes())
            .map_err(|_| SafePathError::InvalidComponent)?
    };
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
    };
    let fd = unsafe {
        // SAFETY: arguments point to initialized data and openat2 does not retain them.
        libc::syscall(
            libc::SYS_openat2,
            root_file.as_raw_fd(),
            relative.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        ) as i32
    };
    let file = if fd >= 0 {
        unsafe {
            // SAFETY: openat2 returned a unique owned descriptor.
            fs::File::from_raw_fd(fd)
        }
    } else {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EXDEV | libc::ELOOP)) {
            return Err(SafePathError::EscapesRoot);
        }
        if !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) {
            return Err(error.into());
        }

        open_fallback(root_file, relative.as_c_str())?
    };
    let metadata = file.metadata()?;
    Ok(SafeChild { file, metadata })
}
pub(super) fn open_fallback(
    root_file: &fs::File,
    relative: &std::ffi::CStr,
) -> Result<fs::File, SafePathError> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let fd = unsafe {
        // SAFETY: relative is NUL terminated; a successful descriptor is owned below.
        libc::openat(
            root_file.as_raw_fd(),
            relative.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let file = unsafe {
        // SAFETY: openat returned a unique owned descriptor.
        fs::File::from_raw_fd(fd)
    };
    let root_final = fs::read_link(format!("/proc/self/fd/{}", root_file.as_raw_fd()))?;
    let target_final = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    if !target_final.starts_with(&root_final) {
        return Err(SafePathError::EscapesRoot);
    }
    Ok(file)
}

pub(super) fn root_path(file: &fs::File) -> Result<std::path::PathBuf, SafePathError> {
    use std::os::fd::AsRawFd;
    Ok(fs::read_link(format!(
        "/proc/self/fd/{}",
        file.as_raw_fd()
    ))?)
}
pub(super) fn strip_root<'a>(root: &Path, child: &'a Path) -> Option<&'a Path> {
    child.strip_prefix(root).ok()
}

//! Windows-native platform integration.

use super::PathResolver;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_WRITE_THROUGH,
    ReplaceFileW,
};

pub(super) fn platform_roots(
    _resolver: &impl PathResolver,
    program: &Path,
) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
    Ok((
        program.to_path_buf(),
        program.to_path_buf(),
        program.join(".cache"),
    ))
}

pub(super) fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    let temp = wide(temp);
    let target = wide(target);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut delay = Duration::from_millis(10);

    loop {
        let replaced = unsafe {
            ReplaceFileW(
                target.as_ptr(),
                temp.as_ptr(),
                std::ptr::null_mut(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if replaced != 0 {
            return Ok(());
        }

        let replace_error = io::Error::last_os_error();
        let code = replace_error.raw_os_error().map(|value| value as u32);
        if code == Some(ERROR_FILE_NOT_FOUND) || code == Some(ERROR_PATH_NOT_FOUND) {
            let moved = unsafe {
                MoveFileExW(
                    temp.as_ptr(),
                    target.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if moved != 0 {
                return Ok(());
            }
        }

        let error = io::Error::last_os_error();
        if error.raw_os_error().map(|value| value as u32) != Some(ERROR_SHARING_VIOLATION)
            || Instant::now() >= deadline
        {
            return Err(error);
        }
        thread::sleep(delay.min(deadline.saturating_duration_since(Instant::now())));
        delay = (delay * 2).min(Duration::from_millis(250));
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;

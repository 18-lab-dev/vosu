//! Optional desktop integration; headless telemetry does not depend on a desktop session.

mod handoff;
#[cfg(target_os = "linux")]
pub mod login;
pub mod tray;

pub use handoff::{dashboard_url, existing_dashboard};

pub const MANAGED_UPDATE_ERROR: &str = "vosu is managed by your package manager; update it there";

/// A failed permission probe is conservative: never mutate an install we cannot inspect.
pub fn externally_managed() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(directory_writable))
        != Some(true)
}

pub fn directory_writable(directory: &std::path::Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let Ok(path) = std::ffi::CString::new(directory.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: path is a live NUL-terminated string; access does not retain it.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Unlike readonly metadata, creation checks the current user's Windows ACL.
        // NamedTempFile removes its file on drop, including the error paths.
        tempfile::Builder::new()
            .prefix(".vosu-write-probe-")
            .tempfile_in(directory)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writable_directory_and_missing_directory() {
        let root = tempfile::tempdir().unwrap();
        assert!(directory_writable(root.path()));
        assert!(!directory_writable(&root.path().join("missing")));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readonly_directory_follows_access_semantics() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root can write regardless of mode bits; the daemon is never run as root.
        // SAFETY: geteuid has no pointer arguments or memory preconditions.
        let root_user = unsafe { libc::geteuid() == 0 };
        assert_eq!(directory_writable(root.path()), root_user);
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

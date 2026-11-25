//! Startup step 6 (`docs/v2/plugin-abi.md` §7.6): open the plugin library
//! once, hash it through that open object against the approved digest, and
//! load exactly that object.
//!
//! - Linux: the library is loaded through `/proc/self/fd/<n>` of the
//!   descriptor that was hashed, so a rename or replacement of the path
//!   between the check and the load cannot substitute other bytes.
//! - Windows: the file is opened without write or delete sharing, and that
//!   handle stays open across `LoadLibrary`, so nobody can modify, replace
//!   or delete it until loading finishes.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::super::control::StartupStage;

/// One opened library whose bytes matched the approved digest. Keep it
/// alive until the load returns.
pub(super) struct VerifiedLibrary {
    _file: File,
    load_path: PathBuf,
}

impl VerifiedLibrary {
    /// The path the loader opens: the verified object itself.
    pub(super) fn load_path(&self) -> &Path {
        &self.load_path
    }
}

/// A refused library: stage `library` (cannot open or read it) or
/// `approval` (its digest differs).
#[derive(Debug)]
pub(super) struct LibraryFailure {
    pub stage: StartupStage,
    pub code: &'static str,
    pub errno: Option<i32>,
    pub message: String,
}

impl LibraryFailure {
    fn io(code: &'static str, error: &io::Error) -> Self {
        Self {
            stage: StartupStage::Library,
            code,
            errno: error.raw_os_error(),
            message: error.to_string(),
        }
    }
}

/// Opens `path`, which must be absolute, and checks its SHA-256 against
/// `expected` by reading that open object.
pub(super) fn open_verified(
    path: &Path,
    expected: &[u8; 32],
) -> Result<VerifiedLibrary, LibraryFailure> {
    if !path.is_absolute() {
        return Err(LibraryFailure {
            stage: StartupStage::Library,
            code: "not_absolute",
            errno: None,
            message: format!("library path {} is not absolute", path.display()),
        });
    }
    let mut file = open(path).map_err(|error| LibraryFailure::io("open_failed", &error))?;
    let actual = sha256(&mut file).map_err(|error| LibraryFailure::io("read_failed", &error))?;
    if actual != *expected {
        return Err(LibraryFailure {
            stage: StartupStage::Approval,
            code: "digest_mismatch",
            errno: None,
            message: format!(
                "library {} does not match its approved SHA-256",
                path.display()
            ),
        });
    }
    let load_path = load_path(&file, path);
    Ok(VerifiedLibrary {
        _file: file,
        load_path,
    })
}

/// The library's SHA-256, streamed through a fixed buffer.
pub(in crate::plugin::process) fn sha256(reader: &mut impl Read) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 64 * 1024];
    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&chunk[..read]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(target_os = "linux")]
fn open(path: &Path) -> io::Result<File> {
    // std opens close-on-exec, so a process the plugin starts never
    // inherits it.
    File::open(path)
}

#[cfg(target_os = "linux")]
fn load_path(file: &File, _path: &Path) -> PathBuf {
    use std::os::fd::AsRawFd;
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(windows)]
fn open(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    // Read sharing only: no writer, renamer or deleter can open the file
    // while this handle lives, and the loader's own read-only open still
    // succeeds.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
}

#[cfg(windows)]
fn load_path(_file: &File, path: &Path) -> PathBuf {
    path.to_owned()
}

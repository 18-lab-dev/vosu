//! Core desktop integration behind `POST /api/counters/open-root` and
//! `POST /api/counters/open/<folder>` (A7.1): opens the effective overlay
//! root, or one normal component of it, in the system file manager. The
//! overlay host never opens folders; it only stores and serves overlays.

use std::io;
use std::path::{Path, PathBuf};

pub trait FolderOpener: Send + Sync {
    fn open(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct SystemOpener;
impl FolderOpener for SystemOpener {
    fn open(&self, path: &Path) -> io::Result<()> {
        vosu_sys::process_spawn::open_detached(path)
    }
}

/// The 1.x error texts the dashboard shows for a refused open.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("counter name must be one normal path component")]
    InvalidName,
    #[error("counter path is a symbolic link")]
    Symlink,
    #[error("Folder doesn't exists")]
    Missing,
    #[error("counter I/O failed: {0}")]
    Io(#[from] io::Error),
}

pub fn open_root(opener: &dyn FolderOpener, root: &Path) -> Result<(), OpenError> {
    Ok(opener.open(root)?)
}

pub fn open_folder(opener: &dyn FolderOpener, root: &Path, name: &str) -> Result<(), OpenError> {
    Ok(opener.open(&openable_folder(root, name)?)?)
}

/// One normal component of `root`: a real child directory confined to the
/// root, or a top-level link to a directory (a separately configured mount).
fn openable_folder(root: &Path, name: &str) -> Result<PathBuf, OpenError> {
    vosu_overlay_host::validate_component(name).map_err(|_| OpenError::InvalidName)?;
    let path = root.join(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(OpenError::Missing),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return if std::fs::metadata(&path).is_ok_and(|target| target.is_dir()) {
            Ok(path)
        } else {
            Err(OpenError::Symlink)
        };
    }
    let canonical_root = std::fs::canonicalize(root)?;
    if std::fs::canonicalize(&path)?.parent() != Some(canonical_root.as_path()) {
        return Err(OpenError::InvalidName);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingOpener(Mutex<Vec<PathBuf>>);
    impl FolderOpener for RecordingOpener {
        fn open(&self, path: &Path) -> io::Result<()> {
            self.0.lock().unwrap().push(path.to_owned());
            Ok(())
        }
    }

    #[test]
    fn open_follows_directory_symlink_and_rejects_traversal_and_file_links() {
        let root = tempfile::tempdir().unwrap();
        let statics = root.path().join("static");
        let counter = statics.join("counter");
        std::fs::create_dir_all(&counter).unwrap();
        let opener = RecordingOpener::default();
        open_root(&opener, &statics).unwrap();
        assert!(open_folder(&opener, &statics, "../outside").is_err());
        assert!(open_folder(&opener, &statics, "a/b").is_err());
        assert!(matches!(
            open_folder(&opener, &statics, "missing"),
            Err(OpenError::Missing)
        ));
        open_folder(&opener, &statics, "counter").unwrap();
        assert_eq!(
            opener.0.lock().unwrap().as_slice(),
            &[statics.clone(), counter.clone()]
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = root.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            symlink(&outside, statics.join("link")).unwrap();
            open_folder(&opener, &statics, "link").unwrap();
            assert_eq!(opener.0.lock().unwrap().last(), Some(&statics.join("link")));
            let outside_file = root.path().join("outside-file");
            std::fs::write(&outside_file, "").unwrap();
            symlink(&outside_file, statics.join("file-link")).unwrap();
            assert!(matches!(
                open_folder(&opener, &statics, "file-link"),
                Err(OpenError::Symlink)
            ));
            symlink(root.path().join("gone"), statics.join("dangling")).unwrap();
            assert!(matches!(
                open_folder(&opener, &statics, "dangling"),
                Err(OpenError::Symlink)
            ));
        }
    }
}

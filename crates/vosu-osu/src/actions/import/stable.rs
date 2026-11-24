use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

use super::{ImportError, ImportResult, ValidatedArchive, archive::copy_bounded, check_cancel};

/// Same-filesystem preparation. Dropping a stage before commit removes it.
/// The caller supplies the selected session's Songs root, never an action arg.
pub struct StableStage {
    file: NamedTempFile,
    destination: PathBuf,
}
impl StableStage {
    /// Blocking, cancellable. Never creates the Songs directory or overwrites
    /// another installation: unavailable/stale selected roots fail closed.
    pub fn prepare(
        archive: &mut ValidatedArchive,
        songs: &Path,
        cancel: &CancellationToken,
    ) -> Result<Self, ImportError> {
        check_cancel(cancel)?;
        if !songs.is_absolute() || !songs.is_dir() {
            return Err(ImportError::InvalidTarget);
        }
        let mut file = tempfile::Builder::new()
            .prefix(".vosu-import-")
            .suffix(".part")
            .tempfile_in(songs)
            .map_err(|_| ImportError::Io)?;
        let destination = file.path().with_extension("osz");
        archive
            .snapshot
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|_| ImportError::Io)?;
        let size = archive
            .snapshot
            .as_file()
            .metadata()
            .map_err(|_| ImportError::Io)?
            .len();
        copy_bounded(
            archive.snapshot.as_file_mut(),
            file.as_file_mut(),
            size,
            cancel,
        )?;
        file.as_file().sync_all().map_err(|_| ImportError::Io)?;
        check_cancel(cancel)?;
        Ok(Self { file, destination })
    }

    /// Blocking. Revalidate selected session liveness immediately before this
    /// call. Cancellation after the last check may lose to the no-clobber commit.
    pub fn commit(self, cancel: &CancellationToken) -> Result<ImportResult, ImportError> {
        check_cancel(cancel)?;
        self.file
            .persist_noclobber(&self.destination)
            .map_err(|error| {
                if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                    ImportError::DestinationExists
                } else {
                    ImportError::Io
                }
            })?;
        #[cfg(unix)]
        std::fs::File::open(
            self.destination
                .parent()
                .ok_or(ImportError::InvalidTarget)?,
        )
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ImportError::Io)?;
        Ok(ImportResult::queued())
    }

    #[cfg(test)]
    pub(super) fn destination(&self) -> &Path {
        &self.destination
    }
}

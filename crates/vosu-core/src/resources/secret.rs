use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::Path,
};

/// The supplied directory must be private to the daemon user (including its
/// inherited Windows ACL). No storage path is inferred here. Call on a blocking
/// initialization worker, never a Tokio worker. Debug deliberately omits bytes.
pub struct ResourceSecret(hmac::Key);

impl ResourceSecret {
    /// Atomically publishes a complete secret without replacing an existing one.
    /// Existing malformed/unreadable files fail closed, never rotate silently.
    pub fn open_or_create(path: &Path) -> io::Result<Self> {
        match Self::read(path) {
            Ok(secret) => return Ok(secret),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "secret needs parent directory")
        })?;
        let mut bytes = [0; 32];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| io::Error::other("secure randomness unavailable"))?;
        // NamedTempFile uses 0600 on Unix; the caller supplies a private Windows
        // directory so its inherited ACL also protects the unpublished file.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(path) {
            Ok(_) => {
                #[cfg(unix)]
                File::open(parent)?.sync_all()?;
                Ok(Self(hmac::Key::new(hmac::HMAC_SHA256, &bytes)))
            }
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Self::read(path),
            Err(error) => Err(error.error),
        }
    }

    fn read(path: &Path) -> io::Result<Self> {
        if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "secret must not be a symlink",
            ));
        }
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 32 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed resource secret",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "resource secret is not private",
                ));
            }
        }
        let mut bytes = [0; 32];
        file.read_exact(&mut bytes)?;
        Ok(Self(hmac::Key::new(hmac::HMAC_SHA256, &bytes)))
    }

    pub(super) fn key(&self) -> &hmac::Key {
        &self.0
    }

    #[cfg(test)]
    pub(super) fn fixture(bytes: [u8; 32]) -> Self {
        Self(hmac::Key::new(hmac::HMAC_SHA256, &bytes))
    }
}

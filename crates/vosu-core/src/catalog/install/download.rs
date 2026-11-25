//! Public-only, credential-free package retrieval and same-handle integrity.
//!
//! Package downloads never receive a source policy, private-host exception or
//! credential, even when the package URL shares the private source's host:
//! `PackageTransport` has no parameter that could carry one, and the
//! production transport uses `StreamingClient::download_atomic`, whose request
//! chain is `RequestPurpose::Package` with no scoped credential.
use super::InstallError;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    future::Future,
    io::{Read, Seek, SeekFrom},
    path::Path,
    pin::Pin,
};
use tokio_util::sync::CancellationToken;
use vosu_net::{Deadlines, DownloadLimits, NetworkError, StreamingClient};

pub type DownloadFuture<'a> = Pin<Box<dyn Future<Output = Result<(), NetworkError>> + Send + 'a>>;

/// Streams one package to a private path with `max_bytes` as the hard cap.
pub trait PackageTransport: Send + Sync {
    fn download<'a>(
        &'a self,
        url: &'a str,
        max_bytes: u64,
        destination: &'a Path,
        cancel: &'a CancellationToken,
    ) -> DownloadFuture<'a>;
}

/// Production strict transport: HTTPS, public destinations only, manual
/// bounded redirects, no proxy, no credential and no ambient headers.
pub struct PublicPackageTransport {
    client: StreamingClient,
}
impl PublicPackageTransport {
    pub fn new(client: StreamingClient) -> Self {
        Self { client }
    }
}
impl PackageTransport for PublicPackageTransport {
    fn download<'a>(
        &'a self,
        url: &'a str,
        max_bytes: u64,
        destination: &'a Path,
        cancel: &'a CancellationToken,
    ) -> DownloadFuture<'a> {
        let client = self.client.clone().with_deadlines_and_limits(
            Deadlines::default(),
            DownloadLimits {
                max_bytes,
                max_redirects: 5,
            },
        );
        Box::pin(async move {
            client
                .download_atomic(url, "vosu-catalog", destination, cancel)
                .await
        })
    }
}

/// Exact compressed size and SHA-256 from the retained handle that extraction
/// will consume; the archive path is never reopened after this check.
pub(super) fn verify(
    file: &mut File,
    size: u64,
    sha256: &str,
    cancel: &CancellationToken,
) -> Result<(), InstallError> {
    let expected = decode_sha256(sha256).ok_or(InstallError::HashMismatch)?;
    if file.metadata().map_err(InstallError::io)?.len() != size {
        return Err(InstallError::SizeMismatch);
    }
    file.seek(SeekFrom::Start(0)).map_err(InstallError::io)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(InstallError::Cancelled);
        }
        let count = file.read(&mut buffer).map_err(InstallError::io)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        digest.update(&buffer[..count]);
    }
    if total != size {
        return Err(InstallError::SizeMismatch);
    }
    if digest.finalize().as_slice() != expected {
        return Err(InstallError::HashMismatch);
    }
    file.seek(SeekFrom::Start(0)).map_err(InstallError::io)?;
    Ok(())
}

pub(super) fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut hash = [0; 32];
    for (byte, pair) in hash.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        *byte = digit(pair[0])? << 4 | digit(pair[1])?;
    }
    Some(hash)
}

/// Download into a fresh private directory and open the result exactly once.
pub(super) async fn fetch(
    transport: &dyn PackageTransport,
    downloads: &Path,
    url: &str,
    size: u64,
    cancel: &CancellationToken,
) -> Result<(tempfile::TempDir, File), InstallError> {
    std::fs::create_dir_all(downloads).map_err(InstallError::io)?;
    let directory = tempfile::Builder::new()
        .prefix(".package-")
        .tempdir_in(downloads)
        .map_err(InstallError::io)?;
    let path = directory.path().join("package.zip");
    transport
        .download(url, size, &path, cancel)
        .await
        .map_err(|error| match error {
            NetworkError::Cancelled => InstallError::Cancelled,
            NetworkError::TooLarge(_) => InstallError::SizeMismatch,
            // Redacted: never upstream URLs, bodies or presigned queries.
            _ => InstallError::Download,
        })?;
    // Same-task read of this task's own just-downloaded private archive,
    // strictly after its own `download_atomic` returned: excluded from
    // `vosu_sys::platform::read_published` (no other writer of this path).
    let file = File::open(&path).map_err(InstallError::io)?;
    Ok((directory, file))
}

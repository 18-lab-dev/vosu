//! Capability-checked resource bytes, independent of HTTP and OS file types.
use crate::files::{FileError, OpenedFile};

pub const MAX_CHART_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_READ_BYTES: u32 = vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES;

pub struct PublishedResource {
    pub identity: String,
    pub media_type: &'static str,
    pub file: OpenedFile,
}
impl PublishedResource {
    /// Consumes the reader so all success/error/cancellation paths release it.
    pub async fn read_chart(self) -> Result<Vec<u8>, FileError> {
        read_chart(self.file).await
    }
}

/// Shared chart/calculation input budget, including explicit calculation paths.
pub async fn read_chart(file: OpenedFile) -> Result<Vec<u8>, FileError> {
    let size = file.metadata.size;
    if size > MAX_CHART_BYTES {
        return Err(FileError::LimitExceeded);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    loop {
        // Detect limit + 1 before appending, even if a provider lies about size.
        let len = (MAX_CHART_BYTES + 1 - bytes.len() as u64).min(u64::from(MAX_READ_BYTES)) as u32;
        let chunk = file.read(bytes.len() as u64, len).await?;
        if chunk.len() > len as usize {
            return Err(FileError::Io);
        }
        if bytes.len() as u64 + chunk.len() as u64 > MAX_CHART_BYTES {
            return Err(FileError::LimitExceeded);
        }
        let eof = chunk.len() < len as usize || bytes.len() as u64 + chunk.len() as u64 == size;
        bytes.extend_from_slice(&chunk);
        if eof {
            return Ok(bytes);
        }
    }
}

pub trait ResourceRequester: Send + Sync {
    fn open_published_resource(
        &self,
        session: &crate::state::IdentifiedSession,
        token: &str,
    ) -> crate::files::Operation<'_, PublishedResource>;
}

use crate::plugin::StreamLease;
use crate::plugin::{ResourceReply, ResourceRequest, ResourceRequestError};
use vosu_plugin_api::{StreamChunk, StreamRead, VOSU_STREAM_CHUNK_MAX_BYTES};

struct CancelRead<'a> {
    lease: &'a StreamLease,
    armed: bool,
}
impl Drop for CancelRead<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.lease.close();
        }
    }
}
impl StreamLease {
    /// One bounded, offset-addressed read. A short chunk is EOF, including when
    /// the provider advertised a larger size. No padding or whole-body buffering.
    /// Dropping this read future closes its lease and cancels its pending slot.
    pub async fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, ResourceRequestError> {
        if !self.is_current() {
            return Err(ResourceRequestError::Cancelled);
        }
        if len > VOSU_STREAM_CHUNK_MAX_BYTES
            || offset.checked_add(u64::from(len)).is_none()
            || offset > self.size()
        {
            return Err(ResourceRequestError::InvalidRequest);
        }
        let mut cancel = CancelRead {
            lease: self,
            armed: true,
        };
        let len = u64::from(len).min(self.size() - offset) as u32;
        let reply = self
            .0
            .requests
            .request(
                self.0.owner,
                ResourceRequest::Read(StreamRead {
                    stream: self.0.stream,
                    offset,
                    len,
                }),
            )
            .await?;
        if !self.is_current() {
            return Err(ResourceRequestError::Cancelled);
        }
        match reply {
            ResourceReply::Chunk(StreamChunk::Data { data }) if data.0.len() <= len as usize => {
                if data.0.len() < len as usize {
                    self.close();
                }
                cancel.armed = false;
                Ok(data.0)
            }
            _ => Err(ResourceRequestError::InvalidReply),
        }
    }
}

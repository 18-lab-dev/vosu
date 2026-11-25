//! In-process bytes use the same resolver and request-owned leases as API v1.
use super::{ResolvedResource, ResourceResolver};
use crate::plugin::SurfaceRequester;
use std::{sync::Arc, time::Duration};
use vosu_surface::{
    files::*,
    resources::{PublishedResource, ResourceRequester},
    state::IdentifiedSession,
};

fn error(error: crate::plugin::ResourceRequestError) -> FileError {
    use crate::plugin::ResourceRequestError as E;
    match error {
        E::NotFound => FileError::NotFound,
        E::Cancelled | E::Gone | E::TargetGone => FileError::TargetGone,
        E::InvalidRequest => FileError::InvalidPath,
        E::Busy | E::TimedOut => FileError::NotReady,
        _ => FileError::Io,
    }
}
impl ResourceRequester for SurfaceRequester {
    fn open_published_resource(
        &self,
        selected: &IdentifiedSession,
        token: &str,
    ) -> Operation<'_, PublishedResource> {
        let captured = selected.clone();
        let token = token.to_owned();
        Box::pin(async move {
            let (host, owner, identity) = self.selected_resource(&captured, &token)?;
            let resolver = ResourceResolver::new(&host, Duration::from_secs(5));
            let resource = resolver.resolve(owner, &identity).await.map_err(error)?;
            // A provider callback may race a publication, detach or root epoch.
            // Recheck before handing any bytes to the adapter; stream RAII closes
            // a reply that has lost its selected membership in the meantime.
            let (_, current_owner, _) = self.selected_resource(&captured, &token)?;
            if current_owner != owner {
                return Err(FileError::TargetGone);
            }
            let (size, modified, validator): (_, _, Arc<[u8]>) = match &resource {
                ResolvedResource::File(file) => (
                    file.size(),
                    file.modified(),
                    Arc::from(file.identity_bytes()),
                ),
                ResolvedResource::Stream(stream) => (stream.size(), None, Arc::from([])),
            };
            // Reject oversized metadata before allocating any content. Dropping
            // resource also closes an oversized generated stream exactly once.
            if size > vosu_surface::resources::MAX_CHART_BYTES {
                return Err(FileError::LimitExceeded);
            }
            Ok(PublishedResource {
                identity: identity.id.as_str().to_owned(),
                media_type: identity.media_type,
                file: OpenedFile::new(
                    FileMetadata {
                        size,
                        modified,
                        identity: validator,
                    },
                    Reader(resource),
                ),
            })
        })
    }
}
struct Reader(ResolvedResource);
impl FileReader for Reader {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        Box::pin(async move {
            match &self.0 {
                ResolvedResource::File(file) => file.read(offset, len).await,
                ResolvedResource::Stream(stream) => stream.read(offset, len).await,
            }
            .map_err(error)
        })
    }
}

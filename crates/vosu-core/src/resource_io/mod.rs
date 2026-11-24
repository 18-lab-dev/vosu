//! Lazy byte access composes the pure identity domain with host capabilities.
//! Registry membership must be checked by the serving layer before each resolve.
mod resolve;
mod stream;

pub use resolve::{MAX_CACHED_FILE_RESOLUTIONS, ResolvedFile, ResolvedResource, ResourceResolver};

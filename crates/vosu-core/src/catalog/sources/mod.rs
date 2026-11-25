//! Exact source models, settings and shared contextual validation.
mod context;
mod detect;
mod parse;
mod snapshot;
pub use detect::{DetectedSource, DetectionError, MAX_PROBE_BYTES};
pub use snapshot::{
    CandidateSnapshot, FetchedCatalog, MAX_SNAPSHOT_CANDIDATES, SnapshotError, SourceCandidate,
    SourceFailure, SourceFetchTicket, SourceSnapshot,
};
mod store;
pub use context::{SourceCatalog, validate_catalogs};
pub use parse::{
    Forge, MAX_SOURCE_BYTES, MAX_SOURCES, OFFICIAL_SOURCES, ParsedSource, SourceIdentity,
    SourceLocation, SourceParseError,
};
pub use store::{
    SourceGeneration, SourceInput, SourceLifecycle, SourceRecord, SourceStore, SourceStoreError,
};
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wave2_tests;

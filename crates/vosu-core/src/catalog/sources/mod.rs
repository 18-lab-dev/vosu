//! Exact source models, settings and shared contextual validation.
mod context;
mod parse;
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

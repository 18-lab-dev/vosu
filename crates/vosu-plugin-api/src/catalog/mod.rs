//! Pure catalog-v2 validation. No fetching, installation, trust inference or filesystem access.
//! Intrinsic validation, contextual registration, host/target support and asset-backed
//! mirror comparison are deliberately separate operations. Comparison requires loaded
//! schema metadata produced from the package's actual bytes, never catalog-derived data.
mod mirror;
mod types;
mod validate;
pub use mirror::compare_package;
pub use types::{Catalog, CatalogEntry, CatalogProvide, Package};
pub use validate::EntrySupport;
#[cfg(test)]
mod mirror_tests;
#[cfg(test)]
mod tests;

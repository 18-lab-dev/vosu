//! Pure validated tree-index API; byte resolution lives in `resource_io`.
#[cfg(test)]
use super::TreeIndexError as ResourceRequestError;
pub use super::index::{TreeIndex, TreeIndexEntry};

#[cfg(test)]
mod tests;

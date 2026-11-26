//! The legacy wire filter plan shared by core's latest-only WebSocket
//! transport and the adapter projections (`Projection<Filter, _>`). Only the
//! value type lives here: the JavaScript-compatible `applyFilters` parser and
//! the projection semantics stay adapter-owned (`vosu-adapter-support`), and
//! the transport only stores, compares and hands plans back to projections.
use serde::{Deserialize, Serialize};

/// One selected property, optionally narrowed to some of its children.
/// Serialized untagged: `"field"` or `{"field": ..., "keys": [...]}`.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Filter {
    Field(String),
    Nested { field: String, keys: Vec<Filter> },
}

/// Most filter nodes one plan may hold, counting nested keys.
pub const MAX_FILTER_NODES: usize = 256;
/// Deepest nesting one plan may hold; a top-level filter has depth zero.
pub const MAX_FILTER_DEPTH: usize = 8;

/// Whether a decoded plan respects [`MAX_FILTER_NODES`] and
/// [`MAX_FILTER_DEPTH`], the bounds the legacy parser enforces.
pub fn within_limits(filters: &[Filter]) -> bool {
    fn walk(filters: &[Filter], depth: usize, nodes: &mut usize) -> bool {
        filters.iter().all(|filter| {
            *nodes += 1;
            if *nodes > MAX_FILTER_NODES || depth > MAX_FILTER_DEPTH {
                return false;
            }
            match filter {
                Filter::Field(_) => true,
                Filter::Nested { keys, .. } => walk(keys, depth + 1, nodes),
            }
        })
    }
    walk(filters, 0, &mut 0)
}

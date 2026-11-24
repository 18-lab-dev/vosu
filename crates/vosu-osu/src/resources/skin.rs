//! Plugin-owned skin case fallback. Exact spelling wins; otherwise the first
//! lexicographically sorted case-folded indexed member wins. Lazer mappings
//! retain their logical names while resolution returns content-store paths.
use super::set::Tree;
use vosu_plugin_api::ResourceResolved;

pub(super) fn lookup(tree: &Tree, path: &str) -> Option<ResourceResolved> {
    // Unlike legacy folder browsing, a truncated index never permits a disk
    // scan fallback: only canonical indexed members can be authorized by core.
    tree.lookup(path, true)
}

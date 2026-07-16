mod cache;
mod model;
mod objects;
mod syntax;
mod timing;

pub use cache::BeatmapCache;
pub use model::*;

#[cfg(test)]
mod tests {
    use super::model::*;
    use super::objects::*;
    use super::syntax::*;
    use super::timing::*;
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    include!("beatmap/tests_support.rs");
    include!("beatmap/tests_parsing.rs");
    include!("beatmap/tests_objects.rs");
    include!("beatmap/tests_events.rs");
    include!("beatmap/tests_cache_graph.rs");
}

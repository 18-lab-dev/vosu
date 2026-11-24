//! Shared state-bus services owned by one plugin host.
use super::{SchemaRegistry, StateStore};
use std::sync::{Arc, RwLock};

pub struct StateRuntime {
    pub store: StateStore,
    pub registry: Arc<RwLock<SchemaRegistry>>,
}
impl StateRuntime {
    pub fn new() -> Self {
        let registry = Arc::new(RwLock::new(
            SchemaRegistry::with_core().expect("embedded core schemas are valid"),
        ));
        Self {
            store: StateStore::new(registry.clone()),
            registry,
        }
    }
}
impl Default for StateRuntime {
    fn default() -> Self {
        Self::new()
    }
}

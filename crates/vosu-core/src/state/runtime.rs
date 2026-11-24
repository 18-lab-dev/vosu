//! Shared state-bus services owned by one plugin host.
use super::{SchemaRegistry, StateStore, SubscriptionRegistry};
use std::sync::{Arc, Mutex, RwLock};

pub struct StateRuntime {
    pub store: StateStore,
    pub registry: Arc<RwLock<SchemaRegistry>>,
    pub subscriptions: Mutex<SubscriptionRegistry>,
}
impl StateRuntime {
    pub fn new() -> Self {
        let registry = Arc::new(RwLock::new(
            SchemaRegistry::with_core().expect("embedded core schemas are valid"),
        ));
        Self {
            store: StateStore::new(registry.clone()),
            registry,
            subscriptions: Mutex::new(SubscriptionRegistry::default()),
        }
    }
}
impl Default for StateRuntime {
    fn default() -> Self {
        Self::new()
    }
}

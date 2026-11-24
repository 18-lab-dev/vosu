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
        Self::with_resources(Arc::new(super::ReferenceStore::ephemeral()))
    }

    pub fn with_resources(resources: Arc<super::ReferenceStore>) -> Self {
        let registry = Arc::new(RwLock::new(
            SchemaRegistry::with_core().expect("embedded core schemas are valid"),
        ));
        Self {
            store: StateStore::with_resources(registry.clone(), resources),
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

//! Public API composition owns only a Weak-backed source and an event-driven hub.
use crate::server::api_v1::{router::ApiV1Router, subscriptions::ApiV1SubscriptionHub};
use std::sync::Arc;
use vosu_core::{config::Config, plugin::PluginHost};

pub(super) struct ApiV1Service {
    pub(super) router: ApiV1Router,
    hub: ApiV1SubscriptionHub,
}

impl ApiV1Service {
    pub(super) fn new(host: &Arc<PluginHost>, config: &Config) -> Self {
        let source = host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(
            source.clone(),
            config.api_v1_subscription_debounce_duration(),
        );
        Self {
            router: ApiV1Router::new(source, hub.clone()),
            hub,
        }
    }

    pub(super) fn reload(&self, config: &Config) {
        self.hub
            .set_debounce(config.api_v1_subscription_debounce_duration());
    }

    // Called after listeners stop accepting and before plugin destruction.
    pub(super) fn shutdown(&self) {
        self.hub.shutdown();
    }
}

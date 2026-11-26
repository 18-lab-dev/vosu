//! Public API composition owns only a Weak-backed source and an event-driven hub.
use crate::server::api_v1::{
    resources::ResourceHttp, router::ApiV1Router, subscriptions::ApiV1SubscriptionHub,
};
use std::{sync::Arc, time::Duration};

/// Applies independently to resolve/list/lookup and every stream-read reply,
/// never to the lifetime of a download.
const RESOURCE_OPERATION_DEADLINE: Duration = Duration::from_secs(5);
use crate::{config::Config, plugin::PluginHost};

pub(in crate::runtime) async fn resource_host(
    logger: crate::logging::Logger,
    settings: Arc<crate::plugin::settings::PluginSettingsService>,
    data: &std::path::Path,
) -> std::io::Result<Arc<PluginHost>> {
    let resources = resource_store(data.to_path_buf()).await?;
    Ok(PluginHost::new_with_resources(
        logger,
        settings,
        tokio::runtime::Handle::current(),
        resources,
    ))
}

/// Required before loading resource-producing plugins. Corrupt/unreadable
/// secrets abort startup; public resource identity must never fall back to an
/// ephemeral registry.
pub(super) async fn resource_store(
    data: std::path::PathBuf,
) -> std::io::Result<Arc<crate::state::ReferenceStore>> {
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&data)?;
        let secret =
            crate::resources::ResourceSecret::open_or_create(&data.join("resource-secret"))?;
        Ok(Arc::new(crate::state::ReferenceStore::new(
            secret,
            Default::default(),
        )))
    })
    .await
    .map_err(std::io::Error::other)?
}

pub(super) struct ApiV1Service {
    pub(super) router: ApiV1Router,
    hub: ApiV1SubscriptionHub,
    jobs: crate::actions::JobRegistry,
}

impl ApiV1Service {
    pub(super) fn action_router(
        &self,
        host: &Arc<PluginHost>,
        tools: Arc<crate::tools::ToolRegistry>,
    ) -> ApiV1Router {
        tools.set_action_providers(
            host.clone(),
            Arc::new(InstalledProviders(Arc::downgrade(host))),
        );
        tools.set_jobs(self.jobs.clone());
        let actions = crate::server::api_v1::actions::ActionRoutes::new(
            host.clone(),
            host.invocation_executor(),
            self.jobs.clone(),
            tools,
        );
        self.router.clone().with_actions(actions)
    }
    pub(super) fn new(host: &Arc<PluginHost>, config: &Config) -> Self {
        let source = host.api_state_source();
        let hub = ApiV1SubscriptionHub::new(
            source.clone(),
            config.api_v1_subscription_debounce_duration(),
        );
        Self {
            router: ApiV1Router::new(source, hub.clone()).with_resources(Arc::new(
                ResourceHttp::new(host, RESOURCE_OPERATION_DEADLINE),
            )),
            hub,
            jobs: crate::actions::JobRegistry::new(),
        }
    }

    pub(super) fn reload(&self, config: &Config) {
        self.hub
            .set_debounce(config.api_v1_subscription_debounce_duration());
    }

    // Seal new subscription workers and send 1001 to the ones already admitted.
    pub(super) fn shutdown(&self) {
        self.hub.shutdown();
    }
    pub(super) async fn join(&self) {
        self.hub.join().await;
    }

    #[cfg(feature = "test-harness")]
    pub(super) fn subscription_hub(&self) -> ApiV1SubscriptionHub {
        self.hub.clone()
    }
}

struct InstalledProviders(std::sync::Weak<PluginHost>);
impl crate::tools::capabilities::InstalledCapabilities for InstalledProviders {
    fn version(&self, capability: &str) -> Option<semver::Version> {
        self.0.upgrade()?.installed_capability_version(capability)
    }

    fn validation_snapshot(&self) -> crate::plugin::ValidationSnapshot {
        self.0
            .upgrade()
            .map_or_else(Default::default, |host| host.validation_snapshot())
    }

    fn provider(&self, capability: &str) -> Option<String> {
        self.0.upgrade()?.installed_capability_provider(capability)
    }
}
#[cfg(test)]
#[path = "tests/api_v1.rs"]
mod tests;

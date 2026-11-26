use super::*;
use serde_json::json;

/// Process-wide resource owner used by [`App`].
///
#[derive(Clone)]
pub(super) struct RuntimeBeatmap {
    pub(super) counters: Arc<ReloadableCounterService>,
}

impl CurrentBeatmapProvider for RuntimeBeatmap {
    fn files(&self) -> BeatmapFiles {
        BeatmapFiles {
            static_root: Some(self.counters.current_root()),
            ..BeatmapFiles::default()
        }
    }
}

#[derive(Clone)]
pub(super) struct RuntimeCounterFacade {
    pub(super) counters: Arc<ReloadableCounterService>,
}

impl SettingsProvider for RuntimeCounterFacade {
    fn get(&self, overlay: &str) -> Result<serde_json::Value, String> {
        self.counters
            .get_counter_values(overlay)
            .map(serde_json::Value::Object)
            .map_err(|error| error.to_string())
    }

    fn save(&self, overlay: &str, body: serde_json::Value) -> Result<(), String> {
        let payload: Vec<crate::counters::SettingInput> =
            serde_json::from_value(body).map_err(|error| error.to_string())?;
        self.counters
            .save_user_values(overlay, &payload)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub(super) struct RuntimePages {
    pub(super) counters: Arc<ReloadableCounterService>,
    pub(super) catalog: CatalogService,
    pub(super) config: Arc<ConfigStore>,
    pub(super) shutdown: CancellationToken,
}

/// One scan of the overlays folder: readable overlays and the folders that
/// failed to parse.
struct LocalScan {
    counters: Vec<crate::counters::Counter>,
    broken: Vec<crate::counters::BrokenOverlay>,
}

impl RuntimePages {
    /// `Err` only when the overlays folder itself cannot be walked; a broken
    /// overlay is reported in [`LocalScan::broken`] instead.
    async fn local_scan(&self) -> Result<LocalScan, String> {
        // Acquire before spawning so concurrent requests cannot grow the blocking
        // pool without bound. A cancelled request leaves its worker holding its slot.
        static DISCOVERY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let permit = DISCOVERY
            .acquire()
            .await
            .expect("discovery semaphore is never closed");
        let counters = Arc::clone(&self.counters);
        let (counters, broken) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            counters.discover_all()
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        Ok(LocalScan { counters, broken })
    }

    /// Declared preview colour scheme of each local overlay, read from its
    /// files off the async runtime.
    async fn preview_schemes(
        &self,
        local: &[crate::counters::Counter],
    ) -> Vec<super::preview_scheme::PreviewScheme> {
        let root = self.counters.current_root();
        let folders = local
            .iter()
            .map(|counter| counter.folder_name.clone())
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            folders
                .iter()
                .map(|folder| super::preview_scheme::overlay_preview_scheme(&root.join(folder)))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Update annotations from the last fetched catalog only: local overlays
    /// never wait on the catalog host. The page asks `/api/counters/updates`
    /// for fresh annotations afterwards.
    fn annotate_cached_updates(&self, local: &mut [crate::counters::Counter]) {
        let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
        if let Some(remote) = self.catalog.cached(&installed) {
            annotate_installed_updates(local, &remote);
        }
    }

    async fn installed_html(
        &self,
        scan: LocalScan,
        query: Option<&str>,
        authority: &str,
    ) -> String {
        let mut local = scan.counters;
        self.annotate_cached_updates(&mut local);
        let schemes = self.preview_schemes(&local).await;
        let names = self.counters.names().snapshot();
        let mut html = render_local_counters(&local, &schemes, &names, query, authority);
        html.push_str(&render_broken_overlays(&scan.broken, &names, query));
        html
    }
}

impl PageDataProvider for RuntimePages {
    fn search_counters(
        &self,
        query: &str,
        external: bool,
        authority: Option<String>,
    ) -> crate::server::routes::ProviderFuture<'_, Result<Bytes, String>> {
        let query = normalize_catalog_query(query);
        Box::pin(async move {
            let config = self.config.current();
            let authority = authority.unwrap_or_else(|| configured_authority(&config));
            let html = if external {
                let installed = self
                    .local_scan()
                    .await
                    .map(|scan| scan.counters.iter().map(local_overlay).collect::<Vec<_>>())
                    .unwrap_or_default();
                let remote = self
                    .catalog
                    .fetch(&installed, &self.shutdown)
                    .await
                    .map_err(|error| error.to_string())?;
                render_catalog_counters(&remote, Some(&query), &authority)
            } else {
                let scan = self.local_scan().await?;
                self.installed_html(scan, Some(&query), &authority).await
            };
            Ok(Bytes::from(if html.is_empty() {
                empty_counter_search()
            } else {
                html
            }))
        })
    }

    fn counter_updates(
        &self,
    ) -> crate::server::routes::ProviderFuture<'_, Result<serde_json::Value, String>> {
        Box::pin(async move {
            let mut local = self.local_scan().await?.counters;
            let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
            let remote = self
                .catalog
                .fetch(&installed, &self.shutdown)
                .await
                .map_err(|error| error.to_string())?;
            annotate_installed_updates(&mut local, &remote);
            Ok(json!({
                "updates": local
                    .iter()
                    .filter(|counter| counter.updatable)
                    .filter_map(|counter| Some(json!({
                        "folder": counter.folder_name,
                        "download_url": counter.download_link.as_deref()?,
                    })))
                    .collect::<Vec<_>>(),
            }))
        })
    }
}

pub(super) fn annotate_installed_updates(
    local: &mut [crate::counters::Counter],
    remote: &[CatalogOverlay],
) {
    let updates = remote
        .iter()
        .filter(|overlay| overlay.updatable)
        .map(|overlay| ((overlay.name.as_str(), overlay.author.as_str()), overlay))
        .collect::<std::collections::HashMap<_, _>>();
    for counter in local {
        if let Some(remote) = updates.get(&(counter.name.as_str(), counter.author.as_str())) {
            counter.updatable = true;
            counter.download_link.clone_from(&remote.download_link);
        }
    }
}

pub(super) fn configured_authority(config: &Config) -> String {
    let host = match config.server_ip.trim() {
        "" | "0.0.0.0" | "::" => "127.0.0.1",
        host => host,
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("{host}:{}", format_number(config.server_port))
}

pub(super) fn json_number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

pub(super) fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

//! Overlay presentation for the dashboard listener: the installed and
//! community lists, `/api/counters/search/<query>` and
//! `/api/counters/updates` (A7.1: the overlay host owns these views; the
//! dashboard page shells fetch and render them).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::cards::{
    INSTALLED_EMPTY_STATE, empty_counter_search, escape_html, local_overlay,
    normalize_catalog_query, render_broken_overlays, render_catalog_counters,
    render_local_counters, render_overlays_unreadable,
};
use crate::catalog::{CatalogOverlay, CatalogService};
use crate::preview_scheme::{PreviewScheme, overlay_preview_scheme};
use crate::{BrokenOverlay, Counter, ReloadableCounterService};

/// One scan of the overlays folder: readable overlays and the folders that
/// failed to parse.
pub struct LocalScan {
    pub counters: Vec<Counter>,
    pub broken: Vec<BrokenOverlay>,
}

#[derive(Clone)]
pub struct OverlayPresenter {
    counters: Arc<ReloadableCounterService>,
    catalog: CatalogService,
    shutdown: CancellationToken,
}

impl OverlayPresenter {
    pub fn new(
        counters: Arc<ReloadableCounterService>,
        catalog: CatalogService,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            counters,
            catalog,
            shutdown,
        }
    }

    /// `Err` only when the overlays folder itself cannot be walked; a broken
    /// overlay is reported in [`LocalScan::broken`] instead.
    pub async fn local_scan(&self) -> Result<LocalScan, String> {
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
    async fn preview_schemes(&self, local: &[Counter]) -> Vec<PreviewScheme> {
        let root = self.counters.current_root();
        let folders = local
            .iter()
            .map(|counter| counter.folder_name.clone())
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            folders
                .iter()
                .map(|folder| overlay_preview_scheme(&root.join(folder)))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Update annotations from the last fetched catalog only: local overlays
    /// never wait on the catalog host. The page asks `/api/counters/updates`
    /// for fresh annotations afterwards.
    fn annotate_cached_updates(&self, local: &mut [Counter]) {
        let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
        if let Some(remote) = self.catalog.cached(&installed) {
            annotate_installed_updates(local, &remote);
        }
    }

    pub async fn installed_html(&self, scan: LocalScan, query: Option<&str>) -> String {
        let mut local = scan.counters;
        self.annotate_cached_updates(&mut local);
        let schemes = self.preview_schemes(&local).await;
        let names = self.counters.names().snapshot();
        let mut html = render_local_counters(&local, &schemes, &names, query);
        html.push_str(&render_broken_overlays(&scan.broken, &names, query));
        html
    }

    /// The installed list: its cards, the empty state, or the unreadable-root
    /// alert.
    pub async fn installed_list(&self, scan: Result<LocalScan, String>) -> String {
        match scan {
            Ok(scan) => {
                let html = self.installed_html(scan, None).await;
                if html.is_empty() {
                    INSTALLED_EMPTY_STATE.to_owned()
                } else {
                    html
                }
            }
            Err(error) => render_overlays_unreadable(&self.counters.current_root(), &error),
        }
    }

    /// The community list and its entry count. An unreadable overlays folder
    /// leaves the catalog browsable.
    pub async fn available_list(&self, scan: Result<LocalScan, String>) -> (String, Option<usize>) {
        let installed = scan
            .map(|scan| scan.counters.iter().map(local_overlay).collect::<Vec<_>>())
            .unwrap_or_default();
        match self.catalog.fetch(&installed, &self.shutdown).await {
            Ok(remote) => {
                let html = render_catalog_counters(&remote, None);
                (
                    if html.is_empty() {
                        r#"<div class="empty-state"><h2>Everything is installed</h2><a class="button" href="/">View installed overlays</a></div>"#.to_owned()
                    } else {
                        html
                    },
                    Some(remote.len()),
                )
            }
            Err(error) => (
                format!(
                    r#"<div class="empty-state catalog-unavailable" role="alert"><h2>Couldn't reach the community catalog</h2><p>{}</p><a class="button" href="/available">Retry</a></div>"#,
                    escape_html(&error.to_string())
                ),
                None,
            ),
        }
    }

    /// `GET /api/counters/search/<query>?source=installed|available`.
    pub async fn search(&self, query: &str, external: bool) -> Result<Bytes, String> {
        let query = normalize_catalog_query(query);
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
            render_catalog_counters(&remote, Some(&query))
        } else {
            let scan = self.local_scan().await?;
            self.installed_html(scan, Some(&query)).await
        };
        Ok(Bytes::from(if html.is_empty() {
            empty_counter_search()
        } else {
            html
        }))
    }

    /// `GET /api/counters/updates`.
    pub async fn updates(&self) -> Result<serde_json::Value, String> {
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
    }
}

pub fn annotate_installed_updates(local: &mut [Counter], remote: &[CatalogOverlay]) {
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

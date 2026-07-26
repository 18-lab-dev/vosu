use super::*;

/// Process-wide resource owner used by [`App`].
///
#[derive(Clone)]
pub(super) struct RuntimeBeatmap {
    pub(super) snapshots: Arc<SnapshotManager>,
    pub(super) counters: Arc<ReloadableCounterService>,
}

pub(super) fn current_file_relative(
    folder: Option<&str>,
    filename: Option<&str>,
) -> Option<PathBuf> {
    let filename = filename.filter(|value| !value.is_empty())?;
    let mut relative = PathBuf::new();
    if let Some(folder) = folder.filter(|value| !value.is_empty()) {
        relative.push(folder);
    }
    relative.push(filename);
    Some(relative)
}

pub(super) fn stable_skin_root(game_folder: &str, skin_name: &str) -> Option<PathBuf> {
    let game_folder = (!game_folder.is_empty()).then(|| PathBuf::from(game_folder))?;
    let mut components = Path::new(skin_name).components();
    let std::path::Component::Normal(name) = components.next()? else {
        return None;
    };
    if components.next().is_some() {
        return None;
    }
    Some(game_folder.join("Skins").join(name))
}

pub(super) fn runtime_beatmap_files(
    regular: &crate::state::RegularState,
    source: ClientType,
    static_root: &Path,
) -> BeatmapFiles {
    let songs = PathBuf::from(&regular.global.songs_folder);
    let songs_root = (!songs.as_os_str().is_empty()).then_some(songs);
    let ready = songs_root.is_some();
    let folder = regular.menu.folder.as_deref();
    BeatmapFiles {
        beatmap_file: current_file_relative(folder, regular.menu.filename.as_deref()),
        background_file: current_file_relative(folder, regular.menu.background_filename.as_deref()),
        audio_file: current_file_relative(folder, regular.menu.audio_filename.as_deref()),
        songs_root,
        skin_root: stable_skin_root(&regular.global.game_folder, &regular.global.skin_folder),
        static_root: Some(static_root.to_owned()),
        lazer: source == ClientType::Lazer,
        ready,
    }
}

impl CurrentBeatmapProvider for RuntimeBeatmap {
    fn files(&self) -> BeatmapFiles {
        let manager = self.snapshots.load();
        let static_root = self.counters.current_root();
        let Some(focused) = manager.focused() else {
            return BeatmapFiles {
                static_root: Some(static_root),
                ..BeatmapFiles::default()
            };
        };
        runtime_beatmap_files(&focused.regular, focused.source, &static_root)
    }

    fn current_beatmap_bytes(&self) -> Result<Option<Bytes>, String> {
        use std::io::Read as _;

        let files = self.files();
        let (Some(root), Some(relative)) =
            (files.songs_root.as_deref(), files.beatmap_file.as_deref())
        else {
            return Ok(None);
        };
        let Some(relative) = relative.to_str() else {
            return Ok(None);
        };
        match crate::server::files::resolve_resource(
            Some(root),
            relative,
            relative,
            crate::server::files::MissingKind::NotFound,
        )
        .map_err(|error| error.to_string())?
        {
            crate::server::files::ResourceOutcome::File(mut target) => {
                let mut bytes = Vec::with_capacity(target.length as usize);
                target
                    .file
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                Ok(Some(Bytes::from(bytes)))
            }
            _ => Ok(None),
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
    pub(super) pages: PageService,
    pub(super) counters: Arc<ReloadableCounterService>,
    pub(super) catalog: CatalogService,
    pub(super) config: Arc<ConfigStore>,
    pub(super) shutdown: CancellationToken,
    pub(super) update_version: Option<String>,
}

impl PageDataProvider for RuntimePages {
    fn render(
        &self,
        page: DynamicPage,
        authority: Option<String>,
    ) -> crate::server::routes::ProviderFuture<'_, Result<EmbeddedResponse, String>> {
        Box::pin(async move {
            let config = self.config.current();
            let authority = authority.unwrap_or_else(|| configured_authority(&config));
            let local = self.counters.discover().unwrap_or_default();
            let local_count = Some(local.len());
            let (list, available_count) = match page {
                DynamicPage::Dashboard => {
                    let html = render_local_counters(&local, None, &authority);
                    (
                        if html.is_empty() {
                            r#"<div class="empty-state"><h2>No local overlays yet</h2><p>Install an overlay from the Vosu catalog or add one from this computer.</p><a class="button button-primary" href="/available">Browse available overlays</a></div>"#.to_owned()
                        } else {
                            html
                        },
                        None,
                    )
                }
                DynamicPage::Settings => (render_runtime_settings(&self.config.current()), None),
                DynamicPage::EmptySettings => (String::new(), None),
                DynamicPage::LocalOverlays => (local_overlay_instructions(), None),
                DynamicPage::Available => {
                    let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
                    match self.catalog.fetch(&installed, &self.shutdown).await {
                        Ok(remote) => {
                            let count = remote.len();
                            let html = render_catalog_counters(&remote, None, &authority);
                            (
                                if html.is_empty() {
                                    r#"<div class="empty-state"><h2>Everything is installed</h2><p>This Vosu instance already has every overlay currently listed in the catalog.</p></div>"#.to_owned()
                                } else {
                                    html
                                },
                                Some(count),
                            )
                        }
                        Err(error) => (
                            format!("Error: {}", escape_html(&error.to_string())),
                            Some(0),
                        ),
                    }
                }
            };
            Ok(render_homepage(
                self.pages.dashboard_template(),
                local_count,
                available_count,
                !matches!(
                    page,
                    DynamicPage::Settings | DynamicPage::EmptySettings | DynamicPage::LocalOverlays
                ),
                &list,
                self.update_version.as_deref(),
            ))
        })
    }

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
            let local = self.counters.discover().unwrap_or_default();
            let html = if external {
                let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
                let remote = self
                    .catalog
                    .fetch(&installed, &self.shutdown)
                    .await
                    .map_err(|error| error.to_string())?;
                render_catalog_counters(&remote, Some(&query), &authority)
            } else {
                render_local_counters(&local, Some(&query), &authority)
            };
            Ok(Bytes::from(if html.is_empty() {
                empty_counter_search()
            } else {
                html
            }))
        })
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

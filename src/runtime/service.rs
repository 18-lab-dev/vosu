use super::*;
use serde_json::{Value, json};

/// Process-wide resource owner used by [`App`].
///
#[derive(Clone)]
pub(super) struct RuntimeBeatmap {
    pub(super) snapshots: Arc<SnapshotManager>,
    pub(super) counters: Arc<ReloadableCounterService>,
}

impl RuntimeBeatmap {
    pub(super) fn tool_context(&self) -> Value {
        let manager = self.snapshots.load();
        let Some(focused) = manager.focused() else {
            return json!({"client":null,"songs_dir":null,"osu_dir":null,"skin":null,"beatmap":null});
        };
        let regular = &focused.regular;
        let files = runtime_beatmap_files(regular, focused.source, &self.counters.current_root());
        let root = files.songs_root.and_then(|p| std::fs::canonicalize(p).ok());
        let resolve = |relative: &Option<PathBuf>| -> Option<PathBuf> {
            let root = root.as_ref()?;
            let relative = relative.as_ref()?;
            let path = std::fs::canonicalize(root.join(relative)).ok()?;
            path.starts_with(root).then_some(path)
        };
        let file = resolve(&files.beatmap_file);
        let mode = match regular.global.status {
            2 => Some(regular.gameplay.mode),
            7 => Some(regular.result_screen.raw.mode),
            _ => regular.menu.gamemode,
        };
        let beatmap = file.map(|file| json!({
            "checksum": regular.menu.checksum,
            "mode": match mode { Some(1) => "taiko", Some(2) => "fruits", Some(3) => "mania", _ => "osu" },
            "folder": file.parent(),
            "file": file,
            "audio": resolve(&files.audio_file),
            "background": resolve(&files.background_file),
        }));
        let stable = focused.source != ClientType::Lazer;
        let osu_dir = stable
            .then(|| PathBuf::from(&regular.global.game_folder))
            .filter(|path| path.is_absolute());
        let skin_folder = stable
            .then(|| stable_skin_root(&regular.global.game_folder, &regular.global.skin_folder))
            .flatten()
            .filter(|path| path.is_absolute());
        let skin = regular.global.current_skin.as_deref();
        json!({"client": if stable { "stable" } else { "lazer" }, "songs_dir":root,
            "osu_dir":osu_dir,"skin":{"name":skin.map(|s| s.name.as_str()).unwrap_or(&regular.global.skin_folder),
                "folder":skin_folder,"files":if stable { skin_folder.as_ref().is_some_and(|p| p.is_dir()) } else { skin.is_some_and(|s| !s.files.is_empty()) },
                "builtin":skin.and_then(|s| s.builtin.as_deref())},"beatmap":beatmap})
    }
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
        skin_root: if source == ClientType::Lazer {
            None
        } else {
            stable_skin_root(&regular.global.game_folder, &regular.global.skin_folder)
        },
        skin: regular.global.current_skin.clone(),
        skin_client: Some(if source == ClientType::Lazer {
            "lazer"
        } else {
            "stable"
        }),
        skin_name: Some(
            regular
                .global
                .current_skin
                .as_ref()
                .map(|s| &s.name)
                .unwrap_or(&regular.global.skin_folder)
                .clone(),
        ),
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

        const MAX_BEATMAP_BYTES: u64 = 64 * 1024 * 1024;
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
                if target.length > MAX_BEATMAP_BYTES {
                    return Err("beatmap exceeds the 64 MiB input limit".into());
                }
                let mut bytes = Vec::with_capacity(target.length as usize);
                target
                    .file
                    .by_ref()
                    .take(MAX_BEATMAP_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                if bytes.len() as u64 > MAX_BEATMAP_BYTES {
                    return Err("beatmap exceeds the 64 MiB input limit".into());
                }
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

impl RuntimePages {
    async fn local_counters(&self) -> Result<Vec<crate::counters::Counter>, String> {
        // Acquire before spawning so concurrent requests cannot grow the blocking
        // pool without bound. A cancelled request leaves its worker holding its slot.
        static DISCOVERY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let permit = DISCOVERY
            .acquire()
            .await
            .expect("discovery semaphore is never closed");
        let counters = Arc::clone(&self.counters);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            counters.discover().unwrap_or_default()
        })
        .await
        .map_err(|error| error.to_string())
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

    async fn annotate_updates(&self, local: &mut [crate::counters::Counter]) {
        let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
        if let Ok(remote) = self.catalog.fetch(&installed, &self.shutdown).await {
            annotate_installed_updates(local, &remote);
        }
    }
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
            let local = self.local_counters().await?;
            let local_count = Some(local.len());
            let (list, available_count) = match page {
                DynamicPage::Dashboard => {
                    let mut local = local;
                    self.annotate_updates(&mut local).await;
                    let schemes = self.preview_schemes(&local).await;
                    let names = self.counters.names().snapshot();
                    let html = render_local_counters(&local, &schemes, &names, None, &authority);
                    (
                        if html.is_empty() {
                            r#"<div class="empty-state"><h2>No local overlays yet</h2><p>Install an overlay from the catalog, or copy an overlay folder into the overlays directory on this computer; vosu picks it up without a restart.</p><div class="counter-actions"><a class="button button-primary" href="/available">Browse available overlays</a><button class="button" type="button" data-action="open-overlays-root">Open overlays folder</button></div></div>"#.to_owned()
                        } else {
                            html
                        },
                        None,
                    )
                }
                DynamicPage::Settings => (render_runtime_settings(&self.config.current()), None),
                DynamicPage::EmptySettings => (String::new(), None),
                DynamicPage::Tools => (
                    r#"<div id="tools-root" class="tools-root"></div>"#.to_owned(),
                    None,
                ),
                DynamicPage::Available => {
                    let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
                    match self.catalog.fetch(&installed, &self.shutdown).await {
                        Ok(remote) => {
                            let count = remote.len();
                            let html = render_catalog_counters(&remote, None, &authority);
                            (
                                if html.is_empty() {
                                    r#"<div class="empty-state"><h2>Everything is installed</h2><p>This vosu instance already has every overlay currently listed in the catalog.</p><a class="button" href="/">View installed overlays</a></div>"#.to_owned()
                                } else {
                                    html
                                },
                                Some(count),
                            )
                        }
                        Err(error) => (
                            format!(
                                r#"<div class="empty-state" role="alert"><h2>Catalog unavailable</h2><p>Error: {}</p><p>Installed overlays keep working. Reload this page to try the catalog again.</p></div>"#,
                                escape_html(&error.to_string())
                            ),
                            Some(0),
                        ),
                    }
                }
            };
            Ok(render_homepage(
                self.pages.dashboard_template(),
                local_count,
                available_count,
                !matches!(page, DynamicPage::EmptySettings),
                if matches!(page, DynamicPage::Available) {
                    CATALOG_NOTICE
                } else {
                    ""
                },
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
            let local = self.local_counters().await?;
            let html = if external {
                let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
                let remote = self
                    .catalog
                    .fetch(&installed, &self.shutdown)
                    .await
                    .map_err(|error| error.to_string())?;
                render_catalog_counters(&remote, Some(&query), &authority)
            } else {
                let mut local = local;
                self.annotate_updates(&mut local).await;
                let schemes = self.preview_schemes(&local).await;
                let names = self.counters.names().snapshot();
                render_local_counters(&local, &schemes, &names, Some(&query), &authority)
            };
            Ok(Bytes::from(if html.is_empty() {
                empty_counter_search()
            } else {
                html
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

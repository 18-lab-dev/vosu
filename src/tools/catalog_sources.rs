use super::*;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Source {
    source: String,
    enabled: bool,
}
fn resolve(source: &str) -> Result<String, String> {
    if source.contains("://") {
        validate_url(source)?;
        let url = url::Url::parse(source).map_err(|e| e.to_string())?;
        if !url.path().ends_with("/catalog.json") || url.fragment().is_some() {
            return Err("source URL must name catalog.json without a fragment".into());
        }
        return Ok(source.into());
    }
    let (repository, tag) = source
        .split_once('@')
        .map_or((source, None), |(repo, tag)| (repo, Some(tag)));
    let (owner, repo) = repository
        .split_once('/')
        .ok_or("source must be owner/repo[@tag] or an HTTPS catalog.json URL")?;
    if [owner, repo].iter().any(|s| {
        s.is_empty()
            || s.len() > 100
            || *s == "."
            || *s == ".."
            || !s
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_ .".contains(&b) && b != b' ')
    }) {
        return Err("invalid GitHub source".into());
    }
    let mut url = url::Url::parse("https://github.com/").unwrap();
    {
        let mut path = url.path_segments_mut().unwrap();
        path.pop_if_empty().push(owner).push(repo).push("releases");
        if let Some(tag) = tag {
            if tag.is_empty() || tag.len() > 200 || tag.chars().any(char::is_control) {
                return Err("invalid GitHub release tag".into());
            }
            path.push("download").push(tag);
        } else {
            path.push("latest").push("download");
        }
        path.push("catalog.json");
    }
    Ok(url.into())
}
fn default_sources() -> Vec<Source> {
    [OFFICIAL_SOURCE, VOSULAY_SOURCE]
        .into_iter()
        .map(|source| Source {
            source: source.into(),
            enabled: true,
        })
        .collect()
}
pub(super) fn validate_overlay(overlay: &Overlay) -> Result<(), String> {
    if !manifest::valid_id(&overlay.id) || crate::counters::validate_component(&overlay.id).is_err()
    {
        return Err("invalid overlay id".into());
    }
    semver::Version::parse(&overlay.version).map_err(|e| e.to_string())?;
    validate_url(&overlay.package.url)?;
    for url in overlay.icon_url.iter().chain(&overlay.banner_url) {
        validate_url(url)?;
    }
    let package = &overlay.package;
    if package.size == 0
        || package.size > MAX_PACKAGE
        || package.sha256.len() != 64
        || !package.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("invalid overlay package size/digest".into());
    }
    for requirement in &overlay.requires_tools {
        if !manifest::valid_id(&requirement.id) {
            return Err("invalid overlay tool requirement".into());
        }
        semver::VersionReq::parse(&requirement.version).map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[derive(Clone, Deserialize, Serialize)]
struct InstalledOverlay {
    source: String,
    version: String,
}
impl ToolHost {
    pub fn set_counters(&self, counters: Arc<crate::counters::ReloadableCounterService>) {
        *self.counters.lock() = Some(counters);
    }
    fn sources_path(&self) -> PathBuf {
        self.config.join("catalog-sources.json")
    }
    fn sources_migration_path(&self) -> PathBuf {
        self.config.join("catalog-sources-v2.json")
    }
    pub(in crate::tools) fn migrate_catalog_state(&self) -> Result<(), String> {
        self.load_sources()?;
        self.overlay_state()?;
        Ok(())
    }
    fn load_sources(&self) -> Result<Vec<Source>, String> {
        match fs::read(self.sources_path()) {
            Ok(bytes) => {
                let mut sources: Vec<Source> =
                    serde_json::from_slice(&bytes).map_err(|e| format!("catalog sources: {e}"))?;
                // Former organization defaults are migration inputs only.
                // A v2 marker distinguishes an explicitly edited two-source list.
                let legacy_two = ["shiruviaa/vosu-tools", "shiruviaa/vosu-overlays"];
                let legacy_three = [
                    "shiruviaa/vosu-tools",
                    "shiruviaa/vosulay",
                    "shiruviaa/vosu-overlays",
                ];
                let is_default = |identities: &[&str]| {
                    sources.len() == identities.len()
                        && sources
                            .iter()
                            .zip(identities)
                            .all(|(source, identity)| source.enabled && source.source == *identity)
                };
                if is_default(&legacy_three)
                    || is_default(&legacy_two) && !self.sources_migration_path().exists()
                {
                    let sources = default_sources();
                    atomic_json(&self.sources_path(), &sources).map_err(|e| e.to_string())?;
                    atomic_json(&self.sources_migration_path(), &true)
                        .map_err(|e| e.to_string())?;
                    return Ok(sources);
                }
                let mut changed = false;
                for source in &mut sources {
                    if let Some(current) = migrated_source(&source.source) {
                        source.source = current.into();
                        changed = true;
                    }
                }
                if changed {
                    atomic_json(&self.sources_path(), &sources).map_err(|e| e.to_string())?;
                }
                Ok(sources)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(default_sources()),
            Err(error) => Err(error.to_string()),
        }
    }
    pub(in crate::tools) fn source_view(&self) -> Result<Value, String> {
        let cached = self.catalog.lock().sources.clone();
        let sources=self.load_sources()?.into_iter().map(|source| {
            let status=cached.iter().find(|value|value["source"]==source.source);
            json!({"source":source.source,"enabled":source.enabled,"url":resolve(&source.source).ok(),
                "status":if !source.enabled { "disabled" } else {status.and_then(|v|v["status"].as_str()).unwrap_or("not fetched")},
                "last_error":status.and_then(|v|v.get("last_error")),"fetched_at":status.and_then(|v|v.get("fetched_at"))})
        }).collect::<Vec<_>>();
        Ok(json!({"sources":sources}))
    }
    pub(in crate::tools) async fn save_sources(&self, body: &[u8]) -> Result<Value, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            sources: Vec<Source>,
        }
        let input: Input = serde_json::from_slice(body).map_err(|e| e.to_string())?;
        if input.sources.len() > 32 {
            return Err("at most 32 catalog sources are allowed".into());
        }
        let mut seen = HashSet::new();
        for source in &input.sources {
            resolve(&source.source)?;
            if !seen.insert(&source.source) {
                return Err("duplicate source".into());
            }
        }
        let _operation = self.catalog_operation.lock().await;
        // An explicit edit must never be mistaken for untouched legacy defaults.
        atomic_json(&self.sources_migration_path(), &true).map_err(|e| e.to_string())?;
        atomic_json(&self.sources_path(), &input.sources).map_err(|e| e.to_string())?;
        *self.catalog.lock() = Catalog::default();
        self.source_view()
    }
    pub(super) async fn fetch_sources(&self) -> Result<Catalog, String> {
        let mut merged = Catalog {
            catalog_version: 1,
            ..Catalog::default()
        };
        let mut tool_ids = HashSet::new();
        let mut overlay_ids = HashSet::new();
        for source in self.load_sources()? {
            let url = resolve(&source.source)?;
            if !source.enabled {
                merged.sources.push(json!({"source":source.source,"enabled":false,"url":url,"status":"disabled","last_error":null}));
                continue;
            }
            let result = async {
                let catalog: Catalog = client(2 * 1024 * 1024)
                    .request_json(&url, "vosu-tool-catalog", &self.cancel)
                    .await
                    .map_err(|e| e.to_string())?;
                catalog.validate()?;
                let mut ids = HashSet::new();
                for overlay in &catalog.overlays {
                    validate_overlay(overlay)?;
                    if !ids.insert(&overlay.id) {
                        return Err("duplicate overlay id in catalog".into());
                    }
                }
                Ok::<_, String>(catalog)
            }
            .await;
            match result {
                Err(error)=>merged.sources.push(json!({"source":source.source,"enabled":true,"url":url,"status":"failed","last_error":error,"fetched_at":chrono::Utc::now().to_rfc3339()})),
                Ok(mut catalog)=>{
                    merged.published=catalog.published;
                    merged.sources.push(json!({"source":source.source,"enabled":true,"url":url,"status":"ok","last_error":null,"fetched_at":chrono::Utc::now().to_rfc3339()}));
                    for mut tool in catalog.tools.drain(..) {
                        tool.source=source.source.clone();
                        if tool_ids.insert(tool.id.clone()) { merged.tools.push(tool); }
                        else { let mut value=json!(tool); value["kind"]=json!("tool"); value["shadowed"]=json!(true); merged.shadowed.push(value); }
                    }
                    for mut overlay in catalog.overlays.drain(..) {
                        overlay.source=source.source.clone();
                        if overlay_ids.insert(overlay.id.clone()) {merged.overlays.push(overlay);}
                        else { let mut value=json!(overlay); value["kind"]=json!("overlay"); value["shadowed"]=json!(true); merged.shadowed.push(value); }
                    }
                }
            }
        }
        Ok(merged)
    }
    fn overlay_state(&self) -> Result<BTreeMap<String, InstalledOverlay>, String> {
        match fs::read(self.config.join("overlay-catalog-state.json")) {
            Ok(bytes) => {
                let mut state: BTreeMap<String, InstalledOverlay> =
                    serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                let mut changed = false;
                for overlay in state.values_mut() {
                    if let Some(source) = migrated_source(&overlay.source) {
                        overlay.source = source.into();
                        changed = true;
                    }
                }
                if changed {
                    atomic_json(&self.config.join("overlay-catalog-state.json"), &state)
                        .map_err(|e| e.to_string())?;
                }
                Ok(state)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error.to_string()),
        }
    }
    fn overlay_directory(&self) -> Result<PathBuf, String> {
        self.counters
            .lock()
            .as_ref()
            .map(|counters| counters.current_root())
            .ok_or_else(|| "overlay root is unavailable".into())
    }
    pub(super) fn overlay_views(&self, overlays: &[Overlay]) -> Result<Vec<Value>, String> {
        if overlays.is_empty() {
            return Ok(Vec::new());
        }
        let root = self.overlay_directory()?;
        let state = self.overlay_state()?;
        Ok(overlays
            .iter()
            .map(|overlay| {
                let installed = root.join(&overlay.id).join("index.html").is_file();
                let prior = state.get(&overlay.id);
                let version = prior.map(|entry| entry.version.as_str());
                let update = installed
                    && version
                        .and_then(|v| semver::Version::parse(v).ok())
                        .is_some_and(|v| {
                            semver::Version::parse(&overlay.version).is_ok_and(|latest| latest > v)
                        });
                let mut value = json!(overlay);
                value["installed"] = json!(installed);
                value["installed_version"] = json!(version);
                value["update_available"] = json!(update);
                value["managed"] = json!(prior.is_some_and(|entry| entry.source == overlay.source));
                value
            })
            .collect())
    }
    pub(in crate::tools) async fn install_overlay(
        self: &Arc<Self>,
        id: &str,
        action: &str,
        install_tools: bool,
        job: &str,
    ) -> Result<(), String> {
        self.catalog_view().await?;
        let overlay = self
            .catalog
            .lock()
            .overlays
            .iter()
            .find(|overlay| overlay.id == id)
            .cloned()
            .ok_or("unknown catalog overlay")?;
        for requirement in &overlay.requires_tools {
            let satisfied = self
                .entry(&requirement.id)
                .ok()
                .and_then(|entry| entry.manifest.as_ref().map(|m| m.version.clone()))
                .is_some_and(|version| {
                    semver::Version::parse(&version).is_ok_and(|version| {
                        semver::VersionReq::parse(&requirement.version)
                            .is_ok_and(|req| req.matches(&version))
                    })
                });
            if !satisfied {
                if !install_tools {
                    return Err(format!(
                        "overlay requires tool {}; approve dependency installation",
                        requirement.id
                    ));
                }
                let available = self
                    .catalog
                    .lock()
                    .tools
                    .iter()
                    .find(|tool| tool.id == requirement.id)
                    .cloned()
                    .ok_or("required tool is missing from enabled sources")?;
                if !semver::VersionReq::parse(&requirement.version)
                    .map_err(|e| e.to_string())?
                    .matches(
                        &semver::Version::parse(&available.version).map_err(|e| e.to_string())?,
                    )
                {
                    return Err("catalog tool does not satisfy overlay requirement".into());
                }
                let action = if self.entry(&requirement.id).is_ok() {
                    "update"
                } else {
                    "install"
                };
                self.install_catalog(&requirement.id, action, job).await?;
            }
        }
        let _operation = self.catalog_operation.lock().await;
        let root = self.overlay_directory()?;
        let destination = root.join(id);
        let mut state = self.overlay_state()?;
        if destination.exists()
            && (action == "overlay-install"
                || state
                    .get(id)
                    .is_none_or(|entry| entry.source != overlay.source))
        {
            return Err("overlay exists; use update from its original catalog source".into());
        }
        if !destination.exists() && action == "overlay-update" {
            return Err("overlay is not installed".into());
        }
        fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        let download = tempfile::tempdir_in(&self.cache).map_err(|e| e.to_string())?;
        let archive = download.path().join("overlay.zip");
        self.phase(job, &format!("downloading overlay {id}"));
        client(overlay.package.size)
            .download_atomic(
                &overlay.package.url,
                "vosu-tool-catalog",
                &archive,
                &self.cancel,
            )
            .await
            .map_err(|e| e.to_string())?;
        verify_package(&archive, &overlay.package)?;
        let cancel = self.cancel.clone();
        self.phase(job, &format!("installing overlay {id}"));
        tokio::task::spawn_blocking(move || {
            crate::net::streaming::extract_zip_atomic_validated(
                &archive,
                &destination,
                ZipLimits {
                    max_entries: 10_000,
                    max_expanded_bytes: MAX_PACKAGE,
                },
                &cancel,
                |root| {
                    if !root.join("index.html").is_file() {
                        return Err(crate::net::NetworkError::UnsafeZip(
                            "overlay index.html missing at archive root".into(),
                        ));
                    }
                    Ok(())
                },
            )
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        state.insert(
            id.into(),
            InstalledOverlay {
                source: overlay.source,
                version: overlay.version,
            },
        );
        atomic_json(&self.config.join("overlay-catalog-state.json"), &state)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_resolution() {
        assert_eq!(
            resolve("owner/repo").unwrap(),
            "https://github.com/owner/repo/releases/latest/download/catalog.json"
        );
        assert_eq!(
            resolve("owner/repo@v1.2.3").unwrap(),
            "https://github.com/owner/repo/releases/download/v1.2.3/catalog.json"
        );
        assert_eq!(
            resolve("https://example.com/catalog.json").unwrap(),
            "https://example.com/catalog.json"
        );
        for source in [
            "owner",
            "owner/repo/extra",
            "owner/repo@",
            "http://example.com/catalog.json",
            "https://example.com/not-a-catalog",
            "https://user:pass@example.com/catalog.json",
        ] {
            assert!(resolve(source).is_err(), "{source}");
        }
    }
}

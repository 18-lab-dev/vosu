use super::*;
#[derive(Deserialize)]
struct Release {
    #[serde(deserialize_with = "assets")]
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    #[serde(deserialize_with = "text")]
    name: String,
    #[serde(deserialize_with = "text")]
    browser_download_url: String,
}
pub fn release_url(source: &DetectedSource) -> Result<Url, FetchError> {
    let (origin, project) = repository(source)?;
    let (owner, repo) = project.split_once('/').ok_or(FetchError::InvalidMetadata)?;
    if repo.contains('/') {
        return Err(FetchError::InvalidMetadata);
    }
    endpoint(
        origin,
        &["api", "v1", "repos", owner, repo, "releases", "latest"],
    )
}
pub fn catalog_asset(body: &[u8], source: &DetectedSource) -> Result<Url, FetchError> {
    let release: Release = metadata(body)?;
    select(
        release
            .assets
            .iter()
            .map(|a| (a.name.as_str(), a.browser_download_url.as_str())),
        source,
    )
}

use super::*;
#[derive(Deserialize)]
struct Release {
    assets: Assets,
}
#[derive(Deserialize)]
struct Assets {
    #[serde(deserialize_with = "assets")]
    links: Vec<Link>,
}
#[derive(Deserialize)]
struct Link {
    #[serde(deserialize_with = "text")]
    name: String,
    #[serde(deserialize_with = "text")]
    direct_asset_url: String,
}
pub fn release_url(source: &DetectedSource) -> Result<Url, FetchError> {
    let (origin, project) = repository(source)?;
    endpoint(
        origin,
        &[
            "api",
            "v4",
            "projects",
            project,
            "releases",
            "permalink",
            "latest",
        ],
    )
}
pub fn catalog_asset(body: &[u8], source: &DetectedSource) -> Result<Url, FetchError> {
    let release: Release = metadata(body)?;
    select(
        release
            .assets
            .links
            .iter()
            .map(|a| (a.name.as_str(), a.direct_asset_url.as_str())),
        source,
    )
}

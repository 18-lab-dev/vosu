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
    url: String,
}
pub fn release_url(source: &DetectedSource) -> Result<Url, FetchError> {
    let (owner, repo, tag) = match &source.parsed.location {
        SourceLocation::GitHub { owner, repo, tag } => {
            (owner.as_str(), repo.as_str(), tag.as_deref())
        }
        SourceLocation::Repository {
            origin,
            project_path,
        } if origin.origin()
            == Url::parse("https://github.com")
                .expect("fixed origin")
                .origin() =>
        {
            let (owner, repo) = project_path
                .split_once('/')
                .ok_or(FetchError::InvalidMetadata)?;
            if repo.contains('/') {
                return Err(FetchError::InvalidMetadata);
            }
            (owner, repo, None)
        }
        _ => return Err(FetchError::InvalidMetadata),
    };
    let origin = Url::parse("https://api.github.com").expect("fixed API origin");
    match tag {
        Some(tag) => endpoint(&origin, &["repos", owner, repo, "releases", "tags", tag]),
        None => endpoint(&origin, &["repos", owner, repo, "releases", "latest"]),
    }
}
pub fn catalog_asset(body: &[u8], source: &DetectedSource) -> Result<Url, FetchError> {
    let release: Release = metadata(body)?;
    select(
        release
            .assets
            .iter()
            .map(|a| (a.name.as_str(), a.url.as_str())),
        source,
    )
}

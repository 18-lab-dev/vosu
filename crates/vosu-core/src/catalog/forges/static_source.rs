use super::*;
pub fn catalog_url(source: &DetectedSource) -> Result<Url, FetchError> {
    match &source.parsed.location {
        SourceLocation::Static { url } => Ok(url.clone()),
        _ => Err(FetchError::InvalidMetadata),
    }
}

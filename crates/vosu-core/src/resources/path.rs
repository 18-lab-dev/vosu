use super::TreeIndexError as ResourceRequestError;

/// Decoded logical paths use one portable spelling. No percent decoding occurs
/// here: a literal `%2e` names a literal file, not another traversal attempt.
pub fn validate(path: &str) -> Result<(), ResourceRequestError> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains(['\\', ':', '\0'])
        || path.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.ends_with(['.', ' '])
        })
    {
        return Err(ResourceRequestError::InvalidReply);
    }
    Ok(())
}

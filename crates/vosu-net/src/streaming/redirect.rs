use super::{NetworkError, Url, policy};
pub(super) fn redirect_target(
    current: &Url,
    location: Option<&reqwest::header::HeaderValue>,
    redirects: usize,
    maximum: usize,
) -> Result<Url, NetworkError> {
    if redirects >= maximum {
        return Err(NetworkError::RedirectLimit);
    }
    let location = location
        .and_then(|value| value.to_str().ok())
        .ok_or(NetworkError::RedirectLocation)?;
    let next = current.join(location)?;
    policy::reject_userinfo(&next)?;
    if current.scheme() == "https" && next.scheme() != "https" {
        return Err(NetworkError::Downgrade);
    }
    Ok(next)
}

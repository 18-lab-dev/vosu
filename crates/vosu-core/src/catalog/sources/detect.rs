//! Event-driven detection. No credentials, periodic retry, or cross-origin inference.
use super::{Forge, ParsedSource, SourceGeneration, SourceIdentity, SourceLocation, SourceStore};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use vosu_net::streaming::{
    Deadlines, DownloadLimits, RequestPolicy, RequestPurpose, SourcePolicy, StreamingClient,
};

pub const MAX_PROBE_BYTES: u64 = 16 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DetectionError {
    #[error("source unavailable")]
    Unavailable,
    #[error("forge not detected")]
    Undetected,
    #[error("source changed during detection")]
    Stale,
    #[error("detection cancelled")]
    Cancelled,
}

/// Immutable, secret-free F0 input. The source stamp must still match at publication.
pub struct DetectedSource {
    pub parsed: ParsedSource,
    pub forge: Forge,
    pub generation: SourceGeneration,
    pub policy: SourcePolicy,
}
impl DetectedSource {
    pub fn configured_host(&self) -> &str {
        match &self.parsed.location {
            SourceLocation::GitHub { .. } => "github.com",
            SourceLocation::Static { url } => url.host_str().expect("parsed source host"),
            SourceLocation::Repository { origin, .. } => {
                origin.host_str().expect("parsed source host")
            }
        }
    }
    pub fn credential_origin(&self) -> &vosu_net::streaming::Origin {
        self.policy.credential_origin()
    }
}

impl SourceStore {
    pub async fn detect_source(
        &self,
        identity: &SourceIdentity,
        client: &StreamingClient,
        refresh: bool,
        cancel: &CancellationToken,
    ) -> Result<DetectedSource, DetectionError> {
        let (generation, private) = {
            let state = self.state.lock().map_err(|_| DetectionError::Unavailable)?;
            if !state.available {
                return Err(DetectionError::Unavailable);
            }
            let record = state
                .records
                .iter()
                .find(|r| r.enabled && r.identity == *identity)
                .ok_or(DetectionError::Unavailable)?;
            (state.generation, record.private_network)
        };
        let forge = self.detect(identity, client, refresh, cancel).await?;
        {
            let state = self.state.lock().map_err(|_| DetectionError::Unavailable)?;
            if !state.available || state.generation != generation {
                return Err(DetectionError::Stale);
            }
        }
        let parsed =
            SourceIdentity::parse(identity.as_str()).map_err(|_| DetectionError::Unavailable)?;
        let policy = match &parsed.location {
            SourceLocation::Static { url } => SourcePolicy::static_source(url, private),
            SourceLocation::Repository { origin, .. } => SourcePolicy::repository(origin, private),
            SourceLocation::GitHub { .. } => SourcePolicy::repository(
                &url::Url::parse("https://github.com/").expect("fixed GitHub origin"),
                private,
            ),
        }
        .map_err(|_| DetectionError::Unavailable)?;
        Ok(DetectedSource {
            parsed,
            forge,
            generation,
            policy,
        })
    }
    /// Refresh retries failures only. Successful identities remain cached until removal.
    /// The generation check prevents an old request publishing after any source edit.
    pub async fn detect(
        &self,
        identity: &SourceIdentity,
        client: &StreamingClient,
        refresh: bool,
        cancel: &CancellationToken,
    ) -> Result<Forge, DetectionError> {
        let (generation, parsed, private) = {
            let mut state = self.state.lock().map_err(|_| DetectionError::Unavailable)?;
            if !state.available {
                return Err(DetectionError::Unavailable);
            }
            let record = state
                .records
                .iter()
                .find(|r| r.identity == *identity && r.enabled)
                .ok_or(DetectionError::Unavailable)?;
            let private = record.private_network;
            if let Some(result) = state.detections.get(identity).copied()
                && (result.is_ok() || !refresh)
            {
                return result;
            }
            if refresh {
                state.detections.remove(identity);
            }
            let parsed = SourceIdentity::parse(identity.as_str())
                .map_err(|_| DetectionError::Unavailable)?;
            (state.generation, parsed, private)
        };
        let result = detect_uncached(&parsed, private, client, cancel).await;
        self.finish_detection(identity, generation, result)
    }

    pub(super) fn finish_detection(
        &self,
        identity: &SourceIdentity,
        generation: SourceGeneration,
        result: Result<Forge, DetectionError>,
    ) -> Result<Forge, DetectionError> {
        let mut state = self.state.lock().map_err(|_| DetectionError::Unavailable)?;
        if !state.available || state.generation != generation {
            return Err(DetectionError::Stale);
        }
        if let Some(Ok(forge)) = state.detections.get(identity) {
            return Ok(*forge);
        }
        if result != Err(DetectionError::Cancelled) {
            state.detections.insert(identity.clone(), result);
        }
        result
    }
}

async fn detect_uncached(
    parsed: &ParsedSource,
    private: bool,
    client: &StreamingClient,
    cancel: &CancellationToken,
) -> Result<Forge, DetectionError> {
    let origin = match &parsed.location {
        SourceLocation::Static { .. } => return Ok(Forge::Static),
        SourceLocation::GitHub { .. } => return Ok(Forge::GitHub),
        SourceLocation::Repository { origin, .. } => origin,
    };
    match origin.host_str() {
        Some("github.com") => return Ok(Forge::GitHub),
        Some("gitlab.com") => return Ok(Forge::GitLab),
        _ => {}
    }
    let policy = RequestPolicy::source(
        RequestPurpose::Probe,
        SourcePolicy::repository(origin, private).map_err(|_| DetectionError::Unavailable)?,
    )
    .map_err(|_| DetectionError::Unavailable)?;
    let client = client.clone().with_deadlines_and_limits(
        Deadlines {
            connect: Duration::from_secs(5),
            idle: Duration::from_secs(5),
            total: Duration::from_secs(5),
        },
        DownloadLimits {
            max_bytes: MAX_PROBE_BYTES,
            max_redirects: 5,
        },
    );
    probe_sequence(origin, cancel, |url| {
        let client = &client;
        let policy = &policy;
        async move {
            client
                .request_scoped(url.as_str(), "vosu", policy, None, cancel)
                .await
        }
    })
    .await
}

async fn probe_sequence<F, Fut>(
    origin: &url::Url,
    cancel: &CancellationToken,
    mut probe: F,
) -> Result<Forge, DetectionError>
where
    F: FnMut(url::Url) -> Fut,
    Fut: Future<
        Output = Result<vosu_net::streaming::NetworkResponse, vosu_net::streaming::NetworkError>,
    >,
{
    for (path, forge) in [
        ("/api/v4/version", Forge::GitLab),
        ("/api/v1/version", Forge::Gitea),
    ] {
        if cancel.is_cancelled() {
            return Err(DetectionError::Cancelled);
        }
        let url = origin.join(path).map_err(|_| DetectionError::Unavailable)?;
        if let Ok(response) = probe(url).await
            && response.status == 200
            && response.final_url.origin() == origin.origin()
            && version_response(&response.body)
        {
            return Ok(forge);
        }
    }
    if cancel.is_cancelled() {
        Err(DetectionError::Cancelled)
    } else {
        Err(DetectionError::Undetected)
    }
}

fn version_response(body: &[u8]) -> bool {
    #[derive(serde::Deserialize)]
    struct Version {
        version: String,
    }
    body.len() <= MAX_PROBE_BYTES as usize
        && serde_json::from_slice::<Version>(body).is_ok_and(|v| {
            !v.version.is_empty()
                && v.version.len() <= 256
                && !v.version.chars().any(char::is_control)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;
    use vosu_net::streaming::{NetworkError, NetworkResponse};

    #[tokio::test]
    async fn detection_probes_exact_origin_in_order_and_stops_on_first_success() {
        let origin = Url::parse("https://forge.test:8443/").unwrap();
        let mut urls = Vec::new();
        let result = probe_sequence(&origin, &CancellationToken::new(), |url| {
            urls.push(url.to_string());
            let status = if urls.len() == 1 { 404 } else { 200 };
            std::future::ready(Ok(NetworkResponse {
                status,
                body: br#"{"version":"1.2.3"}"#.as_slice().into(),
                final_url: url,
            }))
        })
        .await;
        assert_eq!(result, Ok(Forge::Gitea));
        assert_eq!(
            urls,
            [
                "https://forge.test:8443/api/v4/version",
                "https://forge.test:8443/api/v1/version"
            ]
        );
        let mut count = 0;
        let result = probe_sequence(&origin, &CancellationToken::new(), |url| {
            count += 1;
            std::future::ready(Ok(NetworkResponse {
                status: 200,
                body: br#"{"version":"17.0"}"#.as_slice().into(),
                final_url: url,
            }))
        })
        .await;
        assert_eq!(result, Ok(Forge::GitLab));
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn detection_rejects_foreign_status_unbounded_and_nonversion_responses() {
        let origin = Url::parse("https://forge.test/").unwrap();
        let mut oversized = br#"{"version":"1","extra":""#.to_vec();
        oversized.extend(std::iter::repeat_n(b'x', MAX_PROBE_BYTES as usize));
        oversized.extend_from_slice(br#""}"#);
        let mut bodies = vec![
            b"not JSON".to_vec(),
            b"{}".to_vec(),
            br#"{"version":123}"#.to_vec(),
            br#"{"version":""}"#.to_vec(),
            oversized,
        ];
        for body in bodies.drain(..) {
            assert_eq!(
                probe_sequence(&origin, &CancellationToken::new(), |url| {
                    std::future::ready(Ok(NetworkResponse {
                        status: 200,
                        body: body.clone().into(),
                        final_url: url,
                    }))
                })
                .await,
                Err(DetectionError::Undetected)
            );
        }
        for final_url in [
            "https://other.test/api/v4/version",
            "https://forge.test:8443/api/v4/version",
            "http://forge.test/api/v4/version",
        ] {
            assert_eq!(
                probe_sequence(&origin, &CancellationToken::new(), |_| {
                    std::future::ready(Ok(NetworkResponse {
                        status: 200,
                        body: br#"{"version":"1"}"#.as_slice().into(),
                        final_url: Url::parse(final_url).unwrap(),
                    }))
                })
                .await,
                Err(DetectionError::Undetected)
            );
        }
        for status in [201, 204, 401, 404, 500] {
            assert_eq!(
                probe_sequence(&origin, &CancellationToken::new(), |url| {
                    std::future::ready(Ok(NetworkResponse {
                        status,
                        body: br#"{"version":"1"}"#.as_slice().into(),
                        final_url: url,
                    }))
                })
                .await,
                Err(DetectionError::Undetected)
            );
        }
        assert_eq!(
            probe_sequence(&origin, &CancellationToken::new(), |_| std::future::ready(
                Err(NetworkError::Timeout)
            ))
            .await,
            Err(DetectionError::Undetected)
        );
    }
}

//! `GET`/`HEAD /api/v1/jobs/{job}/artifacts/{artifact}` (AF.12, API v1
//! "Job artifacts"): a job's protected output, under exactly the credential
//! that may read the job. Bytes stream one bounded chunk at a time from a
//! fresh per-download file handle or stream lease; nothing is buffered or
//! cached. Every chunk rechecks the credential, the job's retention and the
//! artifact's release, and a release wakes a waiting read, so expiry,
//! revocation and cancellation end an open download promptly.
use super::{
    actions::{ActionAuthority, ActionRoutes},
    authorization::Credentials,
    error::error,
    jobs::job_error,
    resource_headers as headers,
    resources::{ResourceHttp, read},
};
use crate::server::files::ByteRange;
use crate::{
    actions::{ArtifactDownload, ArtifactError, JobRegistry},
    plugin::ResourceRequestError,
    resource_io::{ResolvedResource, open_artifact},
};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES;

impl ActionRoutes {
    pub(super) async fn artifact(
        &self,
        request: Request,
        job: &str,
        artifact: &str,
        resources: Option<&ResourceHttp>,
    ) -> Response {
        let credentials = match Credentials::authenticate(&request, self.tools.as_ref()) {
            Ok(value) => value,
            Err(e) => return headers::secure(e.into_response()),
        };
        if request.uri().query().is_some() {
            return headers::secure(error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Unexpected artifact query",
            ));
        }
        // Exactly the job route's authority: an unreadable job is 404, and
        // the job's fixed action is reauthorized for a launch session.
        let snapshot = match self.jobs.get(&credentials.principal, job) {
            Ok(value) => value,
            Err(e) => return headers::secure(job_error(e)),
        };
        if let Err(e) = credentials.authorize(self.tools.as_ref(), &snapshot.action) {
            return headers::secure(e.into_response());
        }
        let mut download = match self
            .jobs
            .open_artifact(&credentials.principal, job, artifact)
        {
            Ok(value) => value,
            Err(e) => return artifact_error(e),
        };
        let size = download.size();
        let media_type = download.media_type();
        let disposition = content_disposition(download.name());
        let selected = if request.method() == Method::HEAD {
            ByteRange::Full
        } else {
            match headers::range(request.headers(), size) {
                Ok(value) => value,
                Err(()) => return no_store(headers::invalid_range(size)),
            }
        };
        let length = selected.length(size);
        let offset = match selected {
            ByteRange::Full => 0,
            ByteRange::Partial { start, .. } => start,
        };
        let body = if request.method() == Method::HEAD || length == 0 {
            Body::empty()
        } else {
            let Some((host, deadline)) =
                resources.and_then(|r| r.host.upgrade().map(|host| (host, r.deadline)))
            else {
                return provider_error(ResourceRequestError::TargetGone);
            };
            let provider = download.provider();
            let id = download.artifact().to_owned();
            let key = download.key().clone();
            let opened = tokio::select! {
                opened = open_artifact(&host, provider, &id, key, deadline) => opened,
                () = download.released() => return released(),
            };
            drop(host);
            let source = match opened {
                Ok(source) => source,
                Err(e) => return provider_error(e),
            };
            // The declared size is exact: a provider that now reports
            // another size fails this download instead of serving it.
            if source_size(&source) != size {
                return provider_error(ResourceRequestError::InvalidReply);
            }
            let mut stream = Download {
                source: Some(source),
                download: Some(download),
                checks: Checks {
                    jobs: self.jobs.clone(),
                    tools: Arc::clone(&self.tools),
                    credentials,
                    job: job.to_owned(),
                    action: snapshot.action.clone(),
                },
                offset,
                remaining: length,
            };
            // Prime one chunk so an initial provider failure keeps its JSON
            // status instead of a truncated 200.
            let first = match stream.next_chunk().await {
                Ok(bytes) => bytes,
                Err(Stop::Released) => return released(),
                Err(Stop::Revoked) => return gone_job(),
                Err(Stop::Provider(e)) => return provider_error(e),
            };
            Body::from_stream(futures_util::stream::try_unfold(
                (stream, Some(first)),
                |(mut stream, first)| async move {
                    let bytes = match first {
                        Some(bytes) => bytes,
                        None if stream.remaining == 0 => return Ok(None),
                        None => stream
                            .next_chunk()
                            .await
                            .map_err(|_| std::io::Error::other("artifact download ended"))?,
                    };
                    Ok::<_, std::io::Error>(Some((bytes, (stream, None))))
                },
            ))
        };
        let mut response = Response::new(body);
        *response.status_mut() = if matches!(selected, ByteRange::Partial { .. }) {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        };
        let output = response.headers_mut();
        output.insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
        output.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        output.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        output.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        output.insert(header::CONTENT_DISPOSITION, disposition);
        if let ByteRange::Partial {
            start,
            end_inclusive,
        } = selected
        {
            output.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end_inclusive}/{size}"))
                    .expect("numeric range"),
            );
        }
        headers::secure(response)
    }
}

/// `attachment` with an ASCII fallback and the exact RFC 8187 UTF-8 name.
fn content_disposition(name: &str) -> HeaderValue {
    let fallback: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(name.len() * 3);
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}"
    ))
    .expect("an escaped ASCII header value")
}

fn source_size(source: &ResolvedResource) -> u64 {
    match source {
        ResolvedResource::File(file) => file.size(),
        ResolvedResource::Stream(stream) => stream.size(),
    }
}

/// What the per-chunk check rereads: the credential, the job's retention
/// and its action grant.
struct Checks {
    jobs: JobRegistry,
    tools: Arc<dyn ActionAuthority>,
    credentials: Credentials,
    job: String,
    action: String,
}

impl Checks {
    fn current(&self) -> bool {
        self.jobs
            .get(&self.credentials.principal, &self.job)
            .is_ok()
            && self
                .credentials
                .authorize(self.tools.as_ref(), &self.action)
                .is_ok()
    }
}

enum Stop {
    Released,
    Revoked,
    Provider(ResourceRequestError),
}

/// One download in flight. Its source (file handle or stream lease) is
/// dropped before its download slot, so every `STREAM_CLOSE` precedes the
/// plugin's `ARTIFACT_RELEASE`.
struct Download {
    source: Option<ResolvedResource>,
    download: Option<ArtifactDownload>,
    checks: Checks,
    offset: u64,
    remaining: u64,
}

impl Download {
    async fn next_chunk(&mut self) -> Result<Vec<u8>, Stop> {
        let result = self.read_chunk().await;
        if result.is_err() || self.remaining == 0 {
            self.end();
        }
        result
    }

    async fn read_chunk(&mut self) -> Result<Vec<u8>, Stop> {
        let (Some(source), Some(download)) = (&self.source, &mut self.download) else {
            return Err(Stop::Released);
        };
        if download.is_released() {
            return Err(Stop::Released);
        }
        if !self.checks.current() {
            return Err(Stop::Revoked);
        }
        let len = self.remaining.min(u64::from(VOSU_STREAM_CHUNK_MAX_BYTES)) as u32;
        let bytes = tokio::select! {
            bytes = read(source, self.offset, len) => bytes.map_err(Stop::Provider)?,
            () = download.released() => return Err(Stop::Released),
        };
        // A short chunk is an early end: abort rather than pad or truncate.
        if bytes.len() != len as usize {
            return Err(Stop::Provider(ResourceRequestError::InvalidReply));
        }
        self.offset += u64::from(len);
        self.remaining -= u64::from(len);
        Ok(bytes)
    }

    fn end(&mut self) {
        drop(self.source.take());
        drop(self.download.take());
    }
}

impl Drop for Download {
    fn drop(&mut self) {
        self.end();
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn released() -> Response {
    headers::secure(error(
        StatusCode::GONE,
        "released_artifact",
        "Artifact released",
    ))
}

fn gone_job() -> Response {
    headers::secure(error(StatusCode::NOT_FOUND, "unknown_job", "Unknown job"))
}

fn artifact_error(value: ArtifactError) -> Response {
    let (status, code, message) = match value {
        ArtifactError::UnknownJob => (StatusCode::NOT_FOUND, "unknown_job", "Unknown job"),
        ArtifactError::UnknownArtifact => (
            StatusCode::NOT_FOUND,
            "unknown_artifact",
            "Unknown artifact",
        ),
        ArtifactError::Released => return released(),
        ArtifactError::TooManyDownloads => (
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_downloads",
            "Artifact download capacity reached",
        ),
    };
    headers::secure(error(status, code, message))
}

fn provider_error(value: ResourceRequestError) -> Response {
    let (status, code, message) = match value {
        ResourceRequestError::TimedOut => (
            StatusCode::GATEWAY_TIMEOUT,
            "provider_timed_out",
            "Artifact provider timed out",
        ),
        _ => (
            StatusCode::BAD_GATEWAY,
            "provider_failed",
            "Artifact provider failed",
        ),
    };
    headers::secure(error(status, code, message))
}

#[cfg(test)]
mod tests {
    use super::content_disposition;

    #[test]
    fn content_disposition_quotes_nothing_unsafe() {
        assert_eq!(
            content_disposition("replay.osr"),
            "attachment; filename=\"replay.osr\"; filename*=UTF-8''replay.osr"
        );
        assert_eq!(
            content_disposition("a \"b\"\\c 動.mp4"),
            "attachment; filename=\"a _b__c _.mp4\"; filename*=UTF-8''a%20%22b%22%5Cc%20%E5%8B%95.mp4"
        );
    }
}

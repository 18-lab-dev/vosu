use super::{error::error, resource_headers as headers};
use crate::server::files::{ByteRange, FileValidators};
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use std::{
    sync::{Arc, Weak},
    time::Duration,
};
use vosu_core::{
    plugin::{PluginHost, ResourceRequestError},
    resource_io::{ResolvedResource, ResourceResolver},
    resources::ReferenceClass,
};
use vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES;

pub struct ResourceHttp {
    pub(super) host: Weak<PluginHost>,
    pub(super) resources: ResourceResolver,
    pub(super) trees: vosu_core::resource_io::TreeResolver,
}
impl ResourceHttp {
    pub fn new(host: &Arc<PluginHost>, deadline: Duration) -> Self {
        Self {
            host: Arc::downgrade(host),
            resources: ResourceResolver::new(host, deadline),
            trees: vosu_core::resource_io::TreeResolver::new(host, deadline),
        }
    }
    pub(super) async fn resource(
        &self,
        token: &str,
        method: &Method,
        request: &HeaderMap,
    ) -> Response {
        let Some(host) = self.host.upgrade() else {
            return headers::missing(false, true);
        };
        let published = match host.published_resource(token, ReferenceClass::Resource) {
            Ok(value) => value,
            Err(error) => return headers::registry_error(error, false),
        };
        let source = match self
            .resources
            .resolve(published.owner, &published.identity)
            .await
        {
            Ok(source) => source,
            Err(error) => return headers::provider_error(error, false),
        };
        let validators = FileValidators {
            etag: format!("\"{}\"", published.identity.id.as_str()),
            modified: match &source {
                ResolvedResource::File(file) => file.modified(),
                ResolvedResource::Stream(_) => None,
            },
        };
        serve(
            source,
            published.identity.media_type,
            validators,
            true,
            method,
            request,
        )
        .await
    }
}

fn size(source: &ResolvedResource) -> u64 {
    match source {
        ResolvedResource::File(file) => file.size(),
        ResolvedResource::Stream(stream) => stream.size(),
    }
}
async fn read(
    source: &ResolvedResource,
    offset: u64,
    length: u32,
) -> Result<Vec<u8>, ResourceRequestError> {
    match source {
        ResolvedResource::File(file) => file.read(offset, length).await,
        ResolvedResource::Stream(stream) => stream.read(offset, length).await,
    }
}

pub(super) async fn serve(
    source: ResolvedResource,
    mime: &'static str,
    validators: FileValidators,
    immutable: bool,
    method: &Method,
    request: &HeaderMap,
) -> Response {
    let full_size = size(&source);
    // Validate disk identity even for HEAD/304, without asking stream providers for bytes.
    if matches!(source, ResolvedResource::File(_))
        && let Err(error) = read(&source, 0, 0).await
    {
        return headers::provider_error(error, false);
    }
    let not_modified = validators.not_modified(request);
    let selected = if method == Method::HEAD || not_modified {
        Ok(ByteRange::Full)
    } else {
        headers::range(request, full_size)
    };
    let selected = match selected {
        Ok(value) => value,
        Err(()) => return headers::invalid_range(full_size),
    };
    let length = selected.length(full_size);
    let offset = match selected {
        ByteRange::Full => 0,
        ByteRange::Partial { start, .. } => start,
    };
    let status = if not_modified {
        StatusCode::NOT_MODIFIED
    } else if matches!(selected, ByteRange::Partial { .. }) {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let body = if method == Method::HEAD || not_modified || length == 0 {
        Body::empty()
    } else {
        // Prime one bounded chunk so initial provider failures retain JSON HTTP status.
        let first_len = length.min(u64::from(VOSU_STREAM_CHUNK_MAX_BYTES)) as u32;
        let first = match read(&source, offset, first_len).await {
            Ok(bytes) if bytes.len() == first_len as usize => bytes,
            Ok(_) => return headers::provider_error(ResourceRequestError::InvalidReply, false),
            Err(error) => return headers::provider_error(error, false),
        };
        let stream = futures_util::stream::try_unfold(
            (source, offset, length, Some(first)),
            |(source, offset, remaining, first)| async move {
                if remaining == 0 {
                    return Ok(None);
                }
                let len = remaining.min(u64::from(VOSU_STREAM_CHUNK_MAX_BYTES)) as u32;
                let bytes = match first {
                    Some(bytes) => bytes,
                    None => read(&source, offset, len)
                        .await
                        .map_err(|_| std::io::Error::other("resource read failed"))?,
                };
                if bytes.len() != len as usize {
                    return Err(std::io::Error::other("resource ended early"));
                }
                let count = bytes.len() as u64;
                Ok(Some((
                    bytes,
                    (source, offset + count, remaining - count, None),
                )))
            },
        );
        Body::from_stream(stream)
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    validators.apply(response.headers_mut());
    let output = response.headers_mut();
    output.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    output.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if !not_modified {
        output.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    }
    if immutable {
        output.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    if let ByteRange::Partial {
        start,
        end_inclusive,
    } = selected
    {
        output.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{end_inclusive}/{full_size}"))
                .expect("numeric range"),
        );
    }
    headers::secure(response)
}

pub(super) fn unavailable() -> Response {
    headers::secure(error(
        StatusCode::BAD_GATEWAY,
        "provider_failed",
        "Resource provider unavailable",
    ))
}

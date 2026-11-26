use super::{
    error::{error, json, source_error},
    resource_headers as headers,
    resources::{self, ResourceHttp},
};
use crate::server::files::{FileValidators, representation_validators};
use crate::{
    api::{ApiPlan, ApiStateSource, SessionTarget},
    resource_io::ResolvedResource,
    resources::ReferenceClass,
    state::StateValue,
};
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::Response,
};

impl ResourceHttp {
    pub(super) async fn tree(
        &self,
        token: &str,
        path: Option<&str>,
        method: &Method,
        request: &HeaderMap,
    ) -> Response {
        let Some(host) = self.host.upgrade() else {
            return headers::missing(true, true);
        };
        let published = match host.published_resource(token, ReferenceClass::Tree) {
            Ok(value) => value,
            Err(error) => return headers::registry_error(error, true),
        };
        if let Some(path) = path {
            let path = match decode_path(path) {
                Ok(path) => path,
                Err(()) => return headers::missing(false, false),
            };
            let member = match self
                .trees
                .lookup(
                    published.owner,
                    &published.identity,
                    &published.families,
                    &path,
                )
                .await
            {
                Ok(member) => member,
                Err(error) => {
                    return self.recheck_failure(
                        token,
                        ReferenceClass::Tree,
                        headers::provider_error(error, true),
                    );
                }
            };
            let validators = match &member.source {
                ResolvedResource::File(file) => {
                    representation_validators(&file.identity_bytes(), file.modified())
                }
                ResolvedResource::Stream(_) => {
                    let mut bytes = member.index.etag.as_bytes().to_vec();
                    bytes.extend_from_slice(member.entry.path.as_bytes());
                    representation_validators(&bytes, None)
                }
            };
            let response = resources::serve(
                member.source,
                member.entry.media_type,
                validators,
                false,
                method,
                request,
            )
            .await;
            return self.recheck_failure(token, ReferenceClass::Tree, response);
        }
        let index = match self
            .trees
            .index(published.owner, &published.identity, &published.families)
            .await
        {
            Ok(index) => index,
            Err(error) => {
                return self.recheck_failure(
                    token,
                    ReferenceClass::Tree,
                    headers::provider_error(error, true),
                );
            }
        };
        let validators = FileValidators {
            etag: index.etag.clone(),
            modified: None,
        };
        let mut response = if validators.not_modified(request) {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::NOT_MODIFIED;
            response
        } else {
            match serde_json::to_vec(&index) {
                Ok(bytes) => json(StatusCode::OK, bytes),
                Err(_) => {
                    return headers::secure(error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "serialization_failed",
                        "Tree encoding failed",
                    ));
                }
            }
        };
        validators.apply(response.headers_mut());
        headers::secure(response)
    }
}

pub(super) fn decode_path(raw: &str) -> Result<String, ()> {
    let path = raw
        .split('/')
        .map(super::router::decode_segment)
        .collect::<Result<Vec<_>, _>>()?
        .join("/");
    crate::resources::validate_tree_path(&path).map_err(|_| ())?;
    // Encoded separators may not create extra logical components after decoding.
    if path.split('/').count() != raw.split('/').count() {
        return Err(());
    }
    Ok(path)
}

fn encode_path(path: &str) -> String {
    use std::fmt::Write;
    let mut output = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            output.push(char::from(byte));
        } else {
            write!(&mut output, "%{byte:02X}").expect("String writing cannot fail");
        }
    }
    output
}

pub(super) fn redirect(
    source: &ApiStateSource,
    target: &SessionTarget,
    chart: Option<&str>,
    raw_path: Option<&str>,
) -> Response {
    let pointer = chart.map_or_else(
        || "/vosu.skin/tree/url".to_owned(),
        |kind| format!("/vosu.chart/resources/{kind}/url"),
    );
    let plan = ApiPlan::new(&[pointer]).expect("constant public pointer");
    let update = match source.read(target, &plan) {
        Ok(update) => update,
        Err(error) => return source_error(error),
    };
    let Some(StateValue::String(url)) = update.values.first().and_then(Option::as_ref) else {
        return headers::missing(false, false);
    };
    let location = if let Some(raw) = raw_path {
        let path = match decode_path(raw) {
            Ok(path) => path,
            Err(()) => return headers::missing(false, false),
        };
        if !url.starts_with("/api/v1/trees/") || !url.ends_with('/') {
            return headers::missing(true, false);
        }
        format!("{url}{}", encode_path(&path))
    } else {
        if !url.starts_with("/api/v1/resources/") {
            return headers::missing(false, false);
        }
        url.to_string()
    };
    let Ok(location) = HeaderValue::from_str(&location) else {
        return headers::missing(false, false);
    };
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::TEMPORARY_REDIRECT;
    response.headers_mut().insert(header::LOCATION, location);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

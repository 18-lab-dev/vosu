use super::error::{error, json, source_error};
use axum::{http::StatusCode, response::Response};
use vosu_core::{
    api::{ApiPlan, ApiStateSource, SessionTarget},
    state::StateValue,
};

pub(super) fn sessions(source: &ApiStateSource) -> Response {
    let values = match source.sessions() {
        Ok(values) => values,
        Err(e) => return source_error(e),
    };
    let mut body = b"{\"sessions\":[".to_vec();
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            body.push(b',');
        }
        value.write_json(&mut body);
    }
    body.extend_from_slice(b"]}");
    json(StatusCode::OK, body)
}

pub(super) fn summary(source: &ApiStateSource, target: &SessionTarget) -> Response {
    match source.session(target) {
        Ok(value) => value_response(&value, false),
        Err(e) => source_error(e),
    }
}

pub(super) fn state(
    source: &ApiStateSource,
    target: &SessionTarget,
    namespace: Option<&str>,
    query: Option<&str>,
) -> Response {
    let pointer = match pointer_query(query) {
        Ok(pointer) => pointer,
        Err((code, message)) => return error(StatusCode::BAD_REQUEST, code, message),
    };
    // Read the namespace/root once: namespace existence and pointer selection must
    // refer to the same immutable publication, even across active focus changes.
    let path = namespace.map_or_else(String::new, |ns| {
        format!("/{}", ns.replace('~', "~0").replace('/', "~1"))
    });
    let plan = match ApiPlan::new(&[path]) {
        Ok(plan) => plan,
        Err(_) => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Invalid namespace",
            );
        }
    };
    let update = match source.read(target, &plan) {
        Ok(update) => update,
        Err(e) => return source_error(e),
    };
    let Some(value) = update.values.first().and_then(Option::as_ref) else {
        return error(
            StatusCode::NOT_FOUND,
            "unknown_namespace",
            "Unknown namespace",
        );
    };
    match pointer {
        Some(pointer) => {
            let null = StateValue::Null;
            value_response(value.get(pointer.tokens()).unwrap_or(&null), true)
        }
        None => value_response(value, false),
    }
}

fn value_response(value: &StateValue, wrapped: bool) -> Response {
    let mut body = Vec::new();
    if wrapped {
        body.extend_from_slice(b"{\"value\":");
    }
    value.write_json(&mut body);
    if wrapped {
        body.push(b'}');
    }
    json(StatusCode::OK, body)
}

fn pointer_query(
    query: Option<&str>,
) -> Result<Option<vosu_core::state::Pointer>, (&'static str, &'static str)> {
    let mut pointer = None;
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return Ok(None);
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let decode = |text| {
            super::router::decode_form_component(text)
                .map_err(|()| ("invalid_request", "Malformed query encoding"))
        };
        let key = decode(key)?;
        let value = decode(value)?;
        if key != "pointer" || pointer.is_some() {
            return Err(("invalid_request", "Invalid state query"));
        }
        pointer = Some(
            vosu_core::state::Pointer::parse(&value)
                .map_err(|_| ("invalid_pointer", "Invalid JSON Pointer"))?,
        );
    }
    Ok(pointer)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;

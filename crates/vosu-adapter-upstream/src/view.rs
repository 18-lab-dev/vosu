//! Selection decoding is performed once per immutable view, never per client.
use crate::Error;
use ciborium::Value;
use serde::de::DeserializeOwned;
use vosu_adapter_support::json::{push_bool, push_js_number, push_str, write_i64};
use vosu_plugin_api::Selector;
use vosu_surface::state::{CapturedSelection, IdentifiedSession};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub session: IdentifiedSession,
    pub focus_revision: u64,
}
impl Identity {
    pub(crate) fn validate_session(
        capture: &CapturedSelection,
    ) -> Result<&IdentifiedSession, Error> {
        let session = capture.session.as_ref().ok_or(Error::NotReady)?;
        if session.game != "osu" || session.owner != "vosu.osu" {
            return Err(Error::NotReady);
        }
        Ok(session)
    }

    pub(crate) fn from_capture(capture: &CapturedSelection) -> Result<Self, Error> {
        let session = Self::validate_session(capture)?;
        Ok(Self {
            session: session.clone(),
            focus_revision: capture.focus_revision,
        })
    }
}

pub(crate) fn selectors(paths: &[&str]) -> Vec<Selector> {
    paths
        .iter()
        .map(|pointer| Selector {
            ns: "vosu.osu".into(),
            pointer: (*pointer).into(),
        })
        .collect()
}

/// Look only inside the selected image; never retry a missing field on Focused.
pub(crate) fn value<'a>(
    capture: &'a CapturedSelection,
    ns: &str,
    pointer: &str,
) -> Result<&'a Value, Error> {
    let entry = capture
        .values
        .iter()
        .filter(|entry| {
            entry.ns == ns
                && (entry.pointer == pointer
                    || entry.pointer.is_empty()
                    || pointer
                        .strip_prefix(&entry.pointer)
                        .is_some_and(|rest| rest.starts_with('/')))
        })
        .max_by_key(|entry| entry.pointer.len())
        .ok_or(Error::NotReady)?;
    if let Some(error) = &entry.error {
        return Err(Error::InvalidSource(format!("{ns}{pointer}: {error:?}")));
    }
    let mut value = entry.value.as_ref().ok_or(Error::NotReady)?;
    let rest = &pointer[entry.pointer.len()..];
    for token in rest
        .strip_prefix('/')
        .into_iter()
        .flat_map(|rest| rest.split('/'))
    {
        let Value::Map(fields) = value else {
            return Err(Error::NotReady);
        };
        value = fields
            .iter()
            .find_map(|(key, value)| (key.as_text() == Some(token)).then_some(value))
            .ok_or(Error::NotReady)?;
    }
    Ok(value)
}

pub(crate) fn decode<T: DeserializeOwned>(
    capture: &CapturedSelection,
    pointer: &str,
) -> Result<T, Error> {
    decode_ns(capture, "vosu.osu", pointer)
}
pub(crate) fn decode_ns<T: DeserializeOwned>(
    capture: &CapturedSelection,
    ns: &str,
    pointer: &str,
) -> Result<T, Error> {
    value(capture, ns, pointer)?
        .deserialized()
        .map_err(|error| Error::InvalidSource(format!("{ns}{pointer}: {error}")))
}

#[inline]
pub(crate) fn missing(ctx: &str, field: &str) -> Error {
    Error::InvalidSource(format!("{ctx}: missing field `{field}`"))
}

#[inline]
pub(crate) fn as_bool(value: &Value, ctx: &str) -> Result<bool, Error> {
    match value {
        Value::Bool(v) => Ok(*v),
        _ => Err(Error::InvalidSource(format!("{ctx}: expected bool"))),
    }
}

#[inline]
pub(crate) fn as_i64(value: &Value, ctx: &str) -> Result<i64, Error> {
    match value {
        Value::Integer(n) => i64::try_from(*n)
            .map_err(|_| Error::InvalidSource(format!("{ctx}: integer out of i64 range"))),
        _ => Err(Error::InvalidSource(format!("{ctx}: expected integer"))),
    }
}

#[inline]
pub(crate) fn as_opt_i64(value: &Value, ctx: &str) -> Result<Option<i64>, Error> {
    match value {
        Value::Null => Ok(None),
        _ => as_i64(value, ctx).map(Some),
    }
}

#[inline]
pub(crate) fn as_f64(value: &Value, ctx: &str) -> Result<f64, Error> {
    match value {
        Value::Float(v) => Ok(*v),
        Value::Integer(n) => Ok(i128::from(*n) as f64),
        _ => Err(Error::InvalidSource(format!("{ctx}: expected number"))),
    }
}

#[inline]
pub(crate) fn as_opt_f64(value: &Value, ctx: &str) -> Result<Option<f64>, Error> {
    match value {
        Value::Null => Ok(None),
        _ => as_f64(value, ctx).map(Some),
    }
}

#[inline]
pub(crate) fn as_str<'a>(value: &'a Value, ctx: &str) -> Result<&'a str, Error> {
    match value {
        Value::Text(s) => Ok(s.as_str()),
        _ => Err(Error::InvalidSource(format!("{ctx}: expected string"))),
    }
}

#[inline]
pub(crate) fn as_opt_str<'a>(value: &'a Value, ctx: &str) -> Result<Option<&'a str>, Error> {
    match value {
        Value::Null => Ok(None),
        Value::Text(s) => Ok(Some(s.as_str())),
        _ => Err(Error::InvalidSource(format!(
            "{ctx}: expected string or null"
        ))),
    }
}

#[inline]
pub(crate) fn as_map<'a>(value: &'a Value, ctx: &str) -> Result<&'a [(Value, Value)], Error> {
    match value {
        Value::Map(entries) => Ok(entries.as_slice()),
        _ => Err(Error::InvalidSource(format!("{ctx}: expected map"))),
    }
}

#[inline]
pub(crate) fn as_array<'a>(value: &'a Value, ctx: &str) -> Result<&'a [Value], Error> {
    match value {
        Value::Array(items) => Ok(items.as_slice()),
        _ => Err(Error::InvalidSource(format!("{ctx}: expected array"))),
    }
}

/// Writes a dynamic CBOR value (such as `Mod.settings`) directly as JS-compatible JSON.
pub(crate) fn write_cbor_json(out: &mut Vec<u8>, value: &Value, ctx: &str) -> Result<(), Error> {
    match value {
        Value::Null => {
            out.extend_from_slice(b"null");
            Ok(())
        }
        Value::Bool(v) => {
            push_bool(out, *v);
            Ok(())
        }
        Value::Integer(n) => {
            if let Ok(v) = i64::try_from(*n) {
                write_i64(out, v);
            } else if let Ok(v) = u64::try_from(*n) {
                use std::io::Write as _;
                write!(out, "{v}").expect("writing to a Vec cannot fail");
            } else {
                return Err(Error::InvalidSource(format!(
                    "{ctx}: integer out of i64/u64 range"
                )));
            }
            Ok(())
        }
        Value::Float(v) => {
            push_js_number(out, *v);
            Ok(())
        }
        Value::Text(s) => {
            push_str(out, s);
            Ok(())
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i != 0 {
                    out.push(b',');
                }
                write_cbor_json(out, item, ctx)?;
            }
            out.push(b']');
            Ok(())
        }
        Value::Map(entries) => {
            out.push(b'{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i != 0 {
                    out.push(b',');
                }
                let key = as_str(k, ctx)?;
                push_str(out, key);
                out.push(b':');
                write_cbor_json(out, v, ctx)?;
            }
            out.push(b'}');
            Ok(())
        }
        _ => Err(Error::InvalidSource(format!(
            "{ctx}: unsupported CBOR value"
        ))),
    }
}

//! Selection decoding is performed once per immutable view, never per client.
use crate::Error;
use ciborium::Value;
use serde::de::DeserializeOwned;
use vosu_plugin_api::Selector;
use vosu_surface::state::{CapturedSelection, IdentifiedSession};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub session: IdentifiedSession,
    pub focus_revision: u64,
}
impl Identity {
    pub(crate) fn from_capture(capture: &CapturedSelection) -> Result<Self, Error> {
        let session = capture.session.as_ref().ok_or(Error::NotReady)?;
        if session.game != "osu" || session.owner != "vosu.osu" {
            return Err(Error::NotReady);
        }
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

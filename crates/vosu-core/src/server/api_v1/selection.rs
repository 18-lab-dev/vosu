//! Selected values are already projected by core; never rebuild a merged document here.
use std::io::{self, Write};

use crate::state::StateValue;
use bytes::Bytes;

pub(super) const MAX_FRAME: usize = 1024 * 1024;

struct Bounded(Vec<u8>, Vec<u8>);
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME - self.0.len() {
            return Err(io::Error::other("subscription frame exceeds 1 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn value(out: &mut Bounded, state: &StateValue) -> io::Result<()> {
    match state {
        StateValue::String(s) => serde_json::to_writer(out, s.as_ref()).map_err(io::Error::other),
        StateValue::Array(items) => {
            out.write_all(b"[")?;
            for (i, item) in items.iter().enumerate() {
                if i != 0 {
                    out.write_all(b",")?;
                }
                value(out, item)?;
            }
            out.write_all(b"]")
        }
        StateValue::Object(object) => {
            out.write_all(b"{")?;
            for (i, (key, item)) in object.iter().enumerate() {
                if i != 0 {
                    out.write_all(b",")?;
                }
                serde_json::to_writer(&mut *out, key).map_err(io::Error::other)?;
                out.write_all(b":")?;
                value(out, item)?;
            }
            out.write_all(b"}")
        }
        StateValue::Null => out.write_all(b"null"),
        StateValue::Bool(true) => out.write_all(b"true"),
        StateValue::Bool(false) => out.write_all(b"false"),
        StateValue::I64(n) => write!(out, "{n}"),
        StateValue::U64(n) => write!(out, "{n}"),
        _ => {
            // Scalar scratch is bounded too. Core owns ECMA number/base64 spelling.
            if let StateValue::Bytes(bytes) = state
                && bytes.len().div_ceil(3).saturating_mul(4).saturating_add(2)
                    > MAX_FRAME - out.0.len()
            {
                return Err(io::Error::other("subscription frame exceeds 1 MiB"));
            }
            out.1.clear();
            state.write_json(&mut out.1);
            if out.1.len() > MAX_FRAME - out.0.len() {
                return Err(io::Error::other("subscription frame exceeds 1 MiB"));
            }
            out.0.extend_from_slice(&out.1);
            Ok(())
        }
    }
}

/// `None` baseline requests an initial; an empty diff produces no frame.
pub(super) fn frame(
    revision: u64,
    baseline: Option<&[Option<StateValue>]>,
    current: &[Option<StateValue>],
) -> io::Result<Option<Bytes>> {
    let changed = |id: usize| baseline.is_none_or(|old| old[id] != current[id]);
    if baseline.is_some() && !(0..current.len()).any(changed) {
        return Ok(None);
    }
    let mut out = Bounded(Vec::with_capacity(4096), Vec::new());
    write!(out, "[{},{},[", u8::from(baseline.is_some()), revision)?;
    let mut comma = false;
    for (id, selected) in current.iter().enumerate() {
        if let Some(selected) = selected
            && changed(id)
        {
            if comma {
                out.write_all(b",")?;
            }
            comma = true;
            write!(out, "[{id},")?;
            value(&mut out, selected)?;
            out.write_all(b"]")?;
        }
    }
    out.write_all(b"]")?;
    if baseline.is_some() {
        out.write_all(b",[")?;
        comma = false;
        for (id, selected) in current.iter().enumerate() {
            if selected.is_none() && changed(id) {
                if comma {
                    out.write_all(b",")?;
                }
                comma = true;
                write!(out, "{id}")?;
            }
        }
        out.write_all(b"]")?;
    }
    out.write_all(b"]")?;
    Ok(Some(Bytes::from(out.0)))
}

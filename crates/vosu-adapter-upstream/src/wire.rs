//! Direct ordered writers borrow the immutable projection; no history clones.
use crate::{Error, dto::*};
use bytes::Bytes;
use vosu_adapter_support::json::{write_i64, write_js_number, write_str};

mod regular;

pub(crate) use regular::regular;

pub(super) fn invalid(error: impl std::fmt::Display) -> Error {
    Error::InvalidSource(error.to_string())
}

fn keys(out: &mut Vec<u8>, keys: &Keys) -> std::io::Result<()> {
    out.push(b'{');
    for (i, (name, button)) in [
        ("k1", &keys.k1),
        ("k2", &keys.k2),
        ("m1", &keys.m1),
        ("m2", &keys.m2),
    ]
    .iter()
    .enumerate()
    {
        if i != 0 {
            out.push(b',');
        }
        write_str(out, name)?;
        out.extend_from_slice(if button.is_pressed {
            b":{\"isPressed\":true,\"count\":"
        } else {
            b":{\"isPressed\":false,\"count\":"
        });
        write_i64(out, button.count);
        out.push(b'}');
    }
    out.push(b'}');
    Ok(())
}
fn numbers(out: &mut Vec<u8>, values: &[f64]) -> std::io::Result<()> {
    out.push(b'[');
    for (i, &value) in values.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        write_js_number(out, value)?;
    }
    out.push(b']');
    Ok(())
}
pub(crate) fn precise(r: &PreciseResponse) -> Result<Bytes, Error> {
    let capacity = r
        .tourney
        .iter()
        .fold(256 + r.hit_errors.len() * 8, |n, client| {
            n + 256 + client.hit_errors.len() * 8
        });
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(b"{\"keys\":");
    keys(&mut out, &r.keys).map_err(invalid)?;
    out.extend_from_slice(b",\"hitErrors\":");
    numbers(&mut out, &r.hit_errors).map_err(invalid)?;
    out.extend_from_slice(b",\"tourney\":[");
    for (i, client) in r.tourney.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        out.extend_from_slice(b"{\"ipcId\":");
        write_js_number(&mut out, client.ipc_id).map_err(invalid)?;
        out.extend_from_slice(b",\"keys\":");
        keys(&mut out, &client.keys).map_err(invalid)?;
        out.extend_from_slice(b",\"hitErrors\":");
        numbers(&mut out, &client.hit_errors).map_err(invalid)?;
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
    Ok(Bytes::from(out))
}

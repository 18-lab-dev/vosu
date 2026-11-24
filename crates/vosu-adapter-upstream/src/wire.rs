//! Direct ordered writers borrow the immutable projection; no history clones.
use crate::{Error, dto::*};
use bytes::Bytes;
use serde::Serialize;
use vosu_adapter_support::json::{write_i64, write_js_number, write_str};

fn field<T: Serialize>(out: &mut Vec<u8>, name: &str, value: &T) -> Result<(), Error> {
    if out.len() > 1 {
        out.push(b',');
    }
    write_str(out, name).map_err(invalid)?;
    out.push(b':');
    vosu_adapter_support::json::Wire::write_wire(&vosu_adapter_support::json::Serde(value), out)
        .map_err(invalid)
}
fn invalid(error: impl std::fmt::Display) -> Error {
    Error::InvalidSource(error.to_string())
}

pub(crate) fn regular(r: &Response) -> Result<Bytes, Error> {
    let mut out = Vec::with_capacity(8192 + r.play.hit_error_array.len() * 8);
    out.push(b'{');
    field(&mut out, "game", &r.game)?;
    field(&mut out, "client", &r.client)?;
    field(&mut out, "server", &r.server)?;
    field(&mut out, "state", &r.state)?;
    field(&mut out, "session", &r.session)?;
    if let Some(value) = &r.ranked_play {
        field(&mut out, "rankedPlay", value)?;
    }
    field(&mut out, "settings", &r.settings)?;
    field(&mut out, "profile", &r.profile)?;
    field(&mut out, "beatmap", &r.beatmap)?;
    field(&mut out, "play", &r.play)?;
    field(&mut out, "leaderboard", &r.leaderboard)?;
    field(&mut out, "performance", &r.performance)?;
    field(&mut out, "resultsScreen", &r.results_screen)?;
    field(&mut out, "folders", &r.folders)?;
    field(&mut out, "files", &r.files)?;
    field(&mut out, "directPath", &r.direct_path)?;
    if let Some(value) = &r.tourney {
        field(&mut out, "tourney", value)?;
    }
    if let Some(value) = &r.room {
        field(&mut out, "room", value)?;
    }
    out.push(b'}');
    Ok(Bytes::from(out))
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

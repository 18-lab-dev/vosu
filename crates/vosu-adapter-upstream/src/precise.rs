//! Upstream precise keys and hit history, without ADR 0021 extensions.
use crate::{
    Error,
    dto::*,
    view::{self, Identity, as_array, as_bool, as_f64, as_i64, as_map, missing},
};
use bytes::Bytes;
use ciborium::Value;
use std::{cmp::Ordering, sync::Arc};
use vosu_adapter_support::json::{push_js_number, write_i64};
use vosu_plugin_api::{Selector, StateEntry};
use vosu_surface::{
    requester::BundledRequester,
    state::{CapturedSelection, SessionTarget},
};

pub fn selectors() -> Vec<Selector> {
    view::selectors(&["/keys/overlay", "/play/hitErrors/all", "/tourney"])
}

struct PreciseSlots<'a> {
    capture: &'a CapturedSelection,
    keys_overlay: Option<&'a StateEntry>,
    hit_errors: Option<&'a StateEntry>,
    tourney: Option<&'a StateEntry>,
    has_prefix: bool,
}

impl<'a> PreciseSlots<'a> {
    fn index(capture: &'a CapturedSelection) -> Self {
        let mut keys_overlay = None;
        let mut hit_errors = None;
        let mut tourney = None;
        let mut has_prefix = false;
        for entry in capture.values.iter() {
            if entry.ns != "vosu.osu" {
                continue;
            }
            match entry.pointer.as_str() {
                "/keys/overlay" => keys_overlay = Some(entry),
                "/play/hitErrors/all" => hit_errors = Some(entry),
                "/tourney" => tourney = Some(entry),
                "" | "/keys" | "/play" | "/play/hitErrors" => has_prefix = true,
                _ => {}
            }
        }
        Self {
            capture,
            keys_overlay,
            hit_errors,
            tourney,
            has_prefix,
        }
    }

    #[inline]
    fn resolve(&self, entry: Option<&'a StateEntry>, pointer: &str) -> Result<&'a Value, Error> {
        if let Some(entry) = entry {
            if let Some(error) = &entry.error {
                return Err(Error::InvalidSource(format!(
                    "vosu.osu{pointer}: {error:?}"
                )));
            }
            return entry.value.as_ref().ok_or(Error::NotReady);
        }
        if self.has_prefix {
            view::value(self.capture, "vosu.osu", pointer)
        } else {
            Err(Error::NotReady)
        }
    }
}

fn parse_button(v: &Value, ctx: &str) -> Result<KeyOverlayButton, Error> {
    if matches!(v, Value::Null) {
        return Ok(KeyOverlayButton {
            is_pressed: false,
            count: 0,
        });
    }
    let mut is_pressed = None;
    let mut count = None;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("isPressed") => is_pressed = Some(as_bool(val, ctx)?),
            Some("count") => count = Some(as_i64(val, ctx)?),
            _ => {}
        }
    }
    Ok(KeyOverlayButton {
        is_pressed: is_pressed.ok_or_else(|| missing(ctx, "isPressed"))?,
        count: count.ok_or_else(|| missing(ctx, "count"))?,
    })
}

fn parse_keys(v: &Value, ctx: &str) -> Result<Keys, Error> {
    let zero = KeyOverlayButton {
        is_pressed: false,
        count: 0,
    };
    if matches!(v, Value::Null) {
        return Ok(Keys {
            k1: zero.clone(),
            k2: zero.clone(),
            m1: zero.clone(),
            m2: zero,
        });
    }
    let mut k1 = zero.clone();
    let mut k2 = zero.clone();
    let mut m1 = zero.clone();
    let mut m2 = zero;
    for (k, val) in as_map(v, ctx)? {
        match k.as_text() {
            Some("k1") => k1 = parse_button(val, ctx)?,
            Some("k2") => k2 = parse_button(val, ctx)?,
            Some("m1") => m1 = parse_button(val, ctx)?,
            Some("m2") => m2 = parse_button(val, ctx)?,
            _ => {}
        }
    }
    Ok(Keys { k1, k2, m1, m2 })
}

struct PreciseClientRef<'a> {
    ipc_id: f64,
    keys: Keys,
    hit_errors: &'a [Value],
}

fn parse_tourney_clients<'a>(v: &'a Value, ctx: &str) -> Result<Vec<PreciseClientRef<'a>>, Error> {
    if matches!(v, Value::Null) {
        return Ok(Vec::new());
    }
    let mut clients_val = None;
    for (k, val) in as_map(v, ctx)? {
        if k.as_text() == Some("clients") {
            clients_val = Some(as_array(val, ctx)?);
        }
    }
    let items = clients_val.ok_or_else(|| missing(ctx, "clients"))?;
    let mut clients = Vec::with_capacity(items.len());
    for item in items {
        let mut ipc_id = None;
        let mut keys_val = None;
        let mut hit_errors = None;
        for (ck, cv) in as_map(item, ctx)? {
            match ck.as_text() {
                Some("ipcId") => ipc_id = Some(as_f64(cv, ctx)?),
                Some("keysOverlay") => keys_val = Some(parse_keys(cv, ctx)?),
                Some("play") => {
                    for (pk, pv) in as_map(cv, ctx)? {
                        if pk.as_text() == Some("hitErrors") {
                            for (hk, hv) in as_map(pv, ctx)? {
                                if hk.as_text() == Some("all") {
                                    hit_errors = Some(as_array(hv, ctx)?);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let keys = match keys_val {
            Some(k) => k,
            None => parse_keys(&Value::Null, ctx)?,
        };
        clients.push(PreciseClientRef {
            ipc_id: ipc_id.ok_or_else(|| missing(ctx, "clients.ipcId"))?,
            keys,
            hit_errors: hit_errors.ok_or_else(|| missing(ctx, "clients.play.hitErrors.all"))?,
        });
    }
    // JS sort is stable, and a NaN comparator behaves like equality.
    clients.sort_by(|a, b| a.ipc_id.partial_cmp(&b.ipc_id).unwrap_or(Ordering::Equal));
    Ok(clients)
}

#[derive(Debug)]
pub struct View {
    pub identity: Identity,
    response: PreciseResponse,
}
impl View {
    pub fn capture(
        requester: &dyn BundledRequester,
        target: SessionTarget,
    ) -> Result<Arc<Self>, Error> {
        Self::from_capture(&requester.capture(target, &selectors())?).map(Arc::new)
    }
    pub fn from_capture(capture: &CapturedSelection) -> Result<Self, Error> {
        let identity = Identity::from_capture(capture)?;
        let response = decode_response(capture, identity.session.variant == "stable")?;
        Ok(Self { identity, response })
    }
    pub fn response(&self) -> &PreciseResponse {
        &self.response
    }
}

pub(crate) fn decode_response_only(capture: &CapturedSelection) -> Result<PreciseResponse, Error> {
    let session = Identity::validate_session(capture)?;
    decode_response(capture, session.variant == "stable")
}

fn decode_response(capture: &CapturedSelection, stable: bool) -> Result<PreciseResponse, Error> {
    let slots = PreciseSlots::index(capture);
    // The pinned precise tournament traversal is stable manager/IPC only.
    let tourney = if stable {
        let clients = parse_tourney_clients(slots.resolve(slots.tourney, "/tourney")?, "/tourney")?;
        let mut out = Vec::with_capacity(clients.len());
        for client in clients {
            let mut errs = Vec::with_capacity(client.hit_errors.len());
            for item in client.hit_errors {
                errs.push(as_f64(item, "/tourney")?);
            }
            out.push(PreciseTourney {
                ipc_id: client.ipc_id,
                keys: client.keys,
                hit_errors: errs,
            });
        }
        out
    } else {
        Vec::new()
    };
    let keys = parse_keys(
        slots.resolve(slots.keys_overlay, "/keys/overlay")?,
        "/keys/overlay",
    )?;
    let hit_errors_val = as_array(
        slots.resolve(slots.hit_errors, "/play/hitErrors/all")?,
        "/play/hitErrors/all",
    )?;
    let mut hit_errors = Vec::with_capacity(hit_errors_val.len());
    for item in hit_errors_val {
        hit_errors.push(as_f64(item, "/play/hitErrors/all")?);
    }
    Ok(PreciseResponse {
        keys,
        hit_errors,
        tourney,
    })
}

fn write_keys(out: &mut Vec<u8>, keys: &Keys) {
    out.extend_from_slice(b"{\"k1\":{\"isPressed\":");
    write_button_body(out, &keys.k1);
    out.extend_from_slice(b",\"k2\":{\"isPressed\":");
    write_button_body(out, &keys.k2);
    out.extend_from_slice(b",\"m1\":{\"isPressed\":");
    write_button_body(out, &keys.m1);
    out.extend_from_slice(b",\"m2\":{\"isPressed\":");
    write_button_body(out, &keys.m2);
    out.push(b'}');
}

#[inline]
fn write_button_body(out: &mut Vec<u8>, button: &KeyOverlayButton) {
    out.extend_from_slice(if button.is_pressed {
        b"true,\"count\":"
    } else {
        b"false,\"count\":"
    });
    write_i64(out, button.count);
    out.push(b'}');
}

fn write_cbor_numbers(out: &mut Vec<u8>, items: &[Value], ctx: &str) -> Result<(), Error> {
    if items.is_empty() {
        out.extend_from_slice(b"[]");
        return Ok(());
    }
    out.push(b'[');
    for (i, item) in items.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        push_js_number(out, as_f64(item, ctx)?);
    }
    out.push(b']');
    Ok(())
}

pub(crate) fn encode_capture(capture: &CapturedSelection) -> Result<Bytes, Error> {
    let session = Identity::validate_session(capture)?;
    let slots = PreciseSlots::index(capture);
    let tourney = if session.variant == "stable" {
        parse_tourney_clients(slots.resolve(slots.tourney, "/tourney")?, "/tourney")?
    } else {
        Vec::new()
    };
    let keys = parse_keys(
        slots.resolve(slots.keys_overlay, "/keys/overlay")?,
        "/keys/overlay",
    )?;
    let hit_errors = as_array(
        slots.resolve(slots.hit_errors, "/play/hitErrors/all")?,
        "/play/hitErrors/all",
    )?;
    let capacity = tourney
        .iter()
        .fold(256 + hit_errors.len() * 8, |n, client| {
            n + 256 + client.hit_errors.len() * 8
        });
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(b"{\"keys\":");
    write_keys(&mut out, &keys);
    out.extend_from_slice(b",\"hitErrors\":");
    write_cbor_numbers(&mut out, hit_errors, "/play/hitErrors/all")?;
    out.extend_from_slice(b",\"tourney\":[");
    for (i, client) in tourney.iter().enumerate() {
        if i != 0 {
            out.push(b',');
        }
        out.extend_from_slice(b"{\"ipcId\":");
        push_js_number(&mut out, client.ipc_id);
        out.extend_from_slice(b",\"keys\":");
        write_keys(&mut out, &client.keys);
        out.extend_from_slice(b",\"hitErrors\":");
        write_cbor_numbers(&mut out, client.hit_errors, "/tourney")?;
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
    Ok(Bytes::from(out))
}

pub fn build(view: &View) -> &PreciseResponse {
    view.response()
}
pub fn encode(view: &View) -> Result<Bytes, Error> {
    crate::wire::precise(view.response())
}
pub fn filtered(view: &View) -> vosu_adapter_support::filtered::SharedDocument<&PreciseResponse> {
    vosu_adapter_support::filtered::SharedDocument::new(view.response())
}

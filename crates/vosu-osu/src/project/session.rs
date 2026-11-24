//! Raw osu! session bookkeeping; foreground is deliberately per-session.
use super::{
    OsuPointer as P, ProjectionSink, SessionFacts,
    common::{number, number_name},
};
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::PublishValueRef as V;

pub(super) fn project(
    s: &StateBundleSnapshot,
    facts: &SessionFacts,
    sink: &mut dyn ProjectionSink,
) {
    sink.put(
        P::OsuSessionPlayTime,
        V::F64(s.regular.global.game_time as f64),
    );
    sink.put(P::OsuSessionPlayCount, V::U64(0));
    sink.put(P::OsuSessionPaused, V::Bool(s.regular.global.paused));
    sink.put(
        P::OsuSessionFocused,
        if cfg!(windows) {
            facts.foreground.map_or(V::Null, V::Bool)
        } else {
            V::Null
        },
    );
    sink.put(
        P::OsuSessionIpcId,
        number(
            facts
                .settings
                .tournament_ipc_id
                .filter(|_| facts.settings.tournament_spectator),
        ),
    );
    number_name(
        sink,
        P::OsuSessionState,
        V::I64(s.precise.status),
        vosu_osu_model::names::game_state_name(s.precise.status),
    );
    match facts.settings.devserver_endpoint.as_deref() {
        None => sink.put(P::OsuSessionServer, V::String("ppy.sh")),
        Some(server) if server.to_str().is_some() => {
            sink.put(P::OsuSessionServer, V::String(server.to_str().unwrap()));
        }
        Some(server) => sink.put_cbor(P::OsuSessionServer, &mut |out| {
            let mut len = 0;
            lossy_parts(server, |part| len += part.len());
            super::common::head(out, 3, len as u64);
            lossy_parts(server, |part| out.extend_from_slice(part.as_bytes()));
        }),
    }
}

// Match OsStr::to_string_lossy without allocating even for malformed endpoints.
#[cfg(unix)]
fn lossy_parts(value: &std::ffi::OsStr, mut emit: impl FnMut(&str)) {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = value.as_bytes();
    loop {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                emit(text);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                emit(std::str::from_utf8(&bytes[..valid]).unwrap());
                emit("\u{fffd}");
                match error.error_len() {
                    Some(invalid) => bytes = &bytes[valid + invalid..],
                    None => break,
                }
            }
        }
    }
}
#[cfg(windows)]
fn lossy_parts(value: &std::ffi::OsStr, mut emit: impl FnMut(&str)) {
    use std::os::windows::ffi::OsStrExt;
    let mut buffer = [0; 4];
    for character in char::decode_utf16(value.encode_wide()) {
        emit(
            character
                .unwrap_or(char::REPLACEMENT_CHARACTER)
                .encode_utf8(&mut buffer),
        );
    }
}

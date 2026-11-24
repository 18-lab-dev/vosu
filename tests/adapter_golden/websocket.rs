//! Raw WebSocket exchanges with the replay's real listener (RFC 6455).
//!
//! The recorded handshake request is sent byte for byte. After a `101`, the
//! recorded frame sequence is replayed in order: each client frame is sent
//! masked, and each server position reads exactly one frame as received
//! (fin, opcode, mask bit and unmasked payload). Nothing is coalesced,
//! skipped or reordered, so an extra server frame (a ping, a second state
//! frame) or a missing one shows up as a frame mismatch.
use super::routes::{RawResponse, Wire};
use std::net::SocketAddr;

/// The fixed client masking key. Masking is transport framing, never
/// compared payload.
const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
/// The largest frame payload read; above the adapters' frame limits.
const MAX_PAYLOAD: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub from_server: bool,
    pub fin: bool,
    pub opcode: u8,
    pub masked: bool,
    /// Unmasked payload bytes.
    pub payload: Vec<u8>,
}

/// One position of a recorded frame sequence.
#[derive(Clone, Debug)]
pub enum Step {
    /// Send this client frame (always masked).
    Send {
        fin: bool,
        opcode: u8,
        payload: Vec<u8>,
    },
    /// Read the next server frame.
    Receive,
}

pub struct Exchange {
    pub handshake: RawResponse,
    /// Every frame in sequence order, client frames as sent.
    pub frames: Vec<Frame>,
}

/// Sends `request`; after a `101` runs `steps`. A non-upgrade answer ends
/// the exchange with no frames.
pub fn exchange(address: SocketAddr, request: &[u8], steps: &[Step]) -> Result<Exchange, String> {
    let mut wire = Wire::connect(address)?;
    wire.send(request)?;
    let handshake = wire.response(request.starts_with(b"HEAD "))?;
    let mut frames = Vec::with_capacity(steps.len());
    if handshake.status != 101 {
        return Ok(Exchange { handshake, frames });
    }
    for (index, step) in steps.iter().enumerate() {
        let frame = match step {
            Step::Send {
                fin,
                opcode,
                payload,
            } => {
                wire.send(&encode(*fin, *opcode, payload))?;
                Frame {
                    from_server: false,
                    fin: *fin,
                    opcode: *opcode,
                    masked: true,
                    payload: payload.clone(),
                }
            }
            Step::Receive => read(&mut wire).map_err(|e| format!("server frame {index}: {e}"))?,
        };
        frames.push(frame);
    }
    // A normal close; the connection is dropped either way.
    wire.send(&encode(true, 8, &1000_u16.to_be_bytes())).ok();
    Ok(Exchange { handshake, frames })
}

/// A masked client frame.
fn encode(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(u8::from(fin) << 7 | opcode);
    match payload.len() {
        length @ 0..=125 => out.push(0x80 | u8::try_from(length).expect("short length")),
        length @ 126..=0xffff => {
            out.push(0x80 | 126);
            out.extend(u16::try_from(length).expect("16-bit length").to_be_bytes());
        }
        length => {
            out.push(0x80 | 127);
            out.extend(u64::try_from(length).expect("64-bit length").to_be_bytes());
        }
    }
    out.extend(MASK);
    out.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ MASK[index % 4]),
    );
    out
}

/// Reads one frame exactly as the server framed it.
fn read(wire: &mut Wire) -> Result<Frame, String> {
    let head = wire.take(2)?;
    if head[0] & 0x70 != 0 {
        return Err("reserved bits set".into());
    }
    let masked = head[1] & 0x80 != 0;
    let length = match head[1] & 0x7f {
        126 => u64::from(u16::from_be_bytes(
            wire.take(2)?.try_into().expect("two bytes"),
        )),
        127 => u64::from_be_bytes(wire.take(8)?.try_into().expect("eight bytes")),
        short => u64::from(short),
    };
    if length > MAX_PAYLOAD {
        return Err(format!("{length}-byte payload exceeds the replay bound"));
    }
    let key = if masked { Some(wire.take(4)?) } else { None };
    let mut payload = wire.take(usize::try_from(length).map_err(|e| e.to_string())?)?;
    if let Some(key) = key {
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= key[index % 4];
        }
    }
    Ok(Frame {
        from_server: true,
        fin: head[0] & 0x80 != 0,
        opcode: head[0] & 0x0f,
        masked,
        payload,
    })
}

//! Frame codec and per-direction frame policy (`docs/v2/plugin-abi.md`
//! §7.2 "Framing", §7.6 private control frames).
//!
//! On the wire a frame is exactly `u32 total_len; u32 opcode; u32 flags;
//! u64 correlation; u32 payload_len; payload`, little-endian, where
//! `total_len` excludes its own 4 bytes. No Rust struct is ever copied: the
//! 24 head bytes are encoded and decoded field by field. A decoded head is
//! a bounded local snapshot, and every length in it is checked against the
//! universal 1 MiB cap before anything is allocated or indexed.

use vosu_plugin_api::{
    VOSU_FRAME_MAX_BYTES, VOSU_OPCODE_PRIVATE_BIT, VOSU_READ_PLAN_SYNC, vosu_opcode,
};

/// The `total_len` prefix.
pub const PREFIX_BYTES: usize = 4;
/// `opcode`, `flags`, `correlation`, `payload_len`.
pub const HEADER_BYTES: usize = 20;
/// Prefix plus header: every frame's fixed overhead.
pub const HEAD_BYTES: usize = PREFIX_BYTES + HEADER_BYTES;
/// The universal payload cap, in either direction, for every opcode.
pub const MAX_PAYLOAD_BYTES: usize = VOSU_FRAME_MAX_BYTES as usize;
/// The largest legal frame on the wire.
pub const MAX_FRAME_BYTES: usize = HEAD_BYTES + MAX_PAYLOAD_BYTES;

/// Set in every transport-private control opcode and in no public ABI
/// opcode (A5.2, `include/vosu_plugin.h` `VOSU_OPCODE_PRIVATE_BIT`).
/// Private frames never reach plugin code.
pub const PRIVATE_OPCODE_BIT: u32 = VOSU_OPCODE_PRIVATE_BIT;

/// Transport-private control opcodes (A5.2, `plugin-abi.md` §7.6).
pub mod control {
    /// core → child, CBOR launch metadata.
    pub const CREATE: u32 = 0x8000_0001;
    /// child → core, CBOR: `create` succeeded.
    pub const READY: u32 = 0x8000_0002;
    /// child → core, CBOR: startup failed.
    pub const FAILED: u32 = 0x8000_0003;
    /// core → child, empty: run the plugin's `destroy`.
    pub const DESTROY: u32 = 0x8000_0004;
    /// child → core, empty: `destroy` and every worker finished.
    pub const DESTROYED: u32 = 0x8000_0005;
    /// child → core, empty, `correlation` = request id: the very next frame
    /// is one public plugin → host frame whose status core acknowledges.
    pub const ACK_NEXT: u32 = 0x8000_0010;
    /// core → child, 4-byte little-endian `vosu_status`, `correlation` =
    /// request id.
    pub const STATUS: u32 = 0x8000_0011;
    /// core → child, empty, `correlation` = request id: the very next frame
    /// is the public `READ_PLAN_RESULT` answering a SYNC submission.
    pub const REPLY_NEXT: u32 = 0x8000_0012;
}

/// One frame's header fields, as the receiver snapshotted and validated
/// them. `correlation` is the public `correlation_id` for a public frame and
/// the private request id for a private one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameHeader {
    pub opcode: u32,
    pub flags: u32,
    pub correlation: u64,
}

impl FrameHeader {
    pub fn is_private(&self) -> bool {
        self.opcode & PRIVATE_OPCODE_BIT != 0
    }
}

/// A frame head that cannot describe a legal frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("payload of {len} bytes exceeds the {MAX_PAYLOAD_BYTES}-byte frame cap")]
    Oversize { len: usize },
    #[error("frame total length {total_len} disagrees with payload length {payload_len}")]
    LengthMismatch { total_len: u32, payload_len: u32 },
}

/// Encodes the 24 head bytes of a frame carrying `payload_len` bytes.
/// Refuses an oversize payload before anything is written anywhere.
pub fn encode_head(
    header: &FrameHeader,
    payload_len: usize,
) -> Result<[u8; HEAD_BYTES], FrameError> {
    if payload_len > MAX_PAYLOAD_BYTES {
        return Err(FrameError::Oversize { len: payload_len });
    }
    // Cannot truncate: both are at most 1 MiB + 20.
    let payload = payload_len as u32;
    let total = payload + HEADER_BYTES as u32;
    let mut head = [0_u8; HEAD_BYTES];
    head[0..4].copy_from_slice(&total.to_le_bytes());
    head[4..8].copy_from_slice(&header.opcode.to_le_bytes());
    head[8..12].copy_from_slice(&header.flags.to_le_bytes());
    head[12..20].copy_from_slice(&header.correlation.to_le_bytes());
    head[20..24].copy_from_slice(&payload.to_le_bytes());
    Ok(head)
}

/// Decodes and validates a head snapshot, returning its header and payload
/// length. The payload length is at most [`MAX_PAYLOAD_BYTES`] and agrees
/// with `total_len`.
pub fn decode_head(head: &[u8; HEAD_BYTES]) -> Result<(FrameHeader, usize), FrameError> {
    let word = |at: usize| u32::from_le_bytes([head[at], head[at + 1], head[at + 2], head[at + 3]]);
    let total_len = word(0);
    let payload_len = word(20);
    if payload_len as usize > MAX_PAYLOAD_BYTES {
        return Err(FrameError::Oversize {
            len: payload_len as usize,
        });
    }
    if u64::from(total_len) != u64::from(payload_len) + HEADER_BYTES as u64 {
        return Err(FrameError::LengthMismatch {
            total_len,
            payload_len,
        });
    }
    let mut correlation = [0_u8; 8];
    correlation.copy_from_slice(&head[12..20]);
    Ok((
        FrameHeader {
            opcode: word(4),
            flags: word(8),
            correlation: u64::from_le_bytes(correlation),
        },
        payload_len as usize,
    ))
}

/// Which way a ring carries frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    HostToPlugin,
    PluginToHost,
}

/// A frame its sender may not send in this direction. Always a protocol
/// violation by the peer: the receiving side terminates the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyViolation {
    #[error("opcode {opcode:#x} is not valid {direction:?}")]
    Opcode { opcode: u32, direction: Direction },
    #[error("flags {flags:#x} are not valid on opcode {opcode:#x}")]
    Flags { opcode: u32, flags: u32 },
    #[error("private opcode {opcode:#x} carries a {len}-byte payload")]
    PrivatePayload { opcode: u32, len: usize },
    #[error("private opcode {opcode:#x} carries correlation {correlation} where none is allowed")]
    PrivateCorrelation { opcode: u32, correlation: u64 },
    #[error("ACK_NEXT request id {id} does not increase past {previous}")]
    RequestId { id: u64, previous: u64 },
    #[error("opcode {opcode:#x} breaks the pairing its preceding control frame requires")]
    Pairing { opcode: u32 },
}

/// Checks each frame's validated header before its payload is copied.
pub trait FramePolicy: Send {
    fn admit(&mut self, header: &FrameHeader, payload_len: usize) -> Result<(), PolicyViolation>;
}

/// The frozen policy for one direction: public ABI opcodes valid that way
/// with their defined flags, the A5.2 private set with its payload and
/// correlation rules, and the `ACK_NEXT`/`REPLY_NEXT` pairing.
#[derive(Debug)]
pub struct DirectionPolicy {
    direction: Direction,
    expect: Expect,
    last_request: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Any,
    /// After `ACK_NEXT`: exactly one public plugin → host frame.
    PublicFrame,
    /// After `REPLY_NEXT`: exactly one public `READ_PLAN_RESULT`.
    ReadPlanResult,
}

impl DirectionPolicy {
    /// The policy for frames received in `direction`.
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            expect: Expect::Any,
            last_request: None,
        }
    }

    fn admit_private(
        &mut self,
        header: &FrameHeader,
        payload_len: usize,
    ) -> Result<(), PolicyViolation> {
        use control::*;
        let opcode = header.opcode;
        // (valid direction, exact payload length if fixed, carries a request id)
        let (direction, fixed_len, request) = match opcode {
            CREATE => (Direction::HostToPlugin, None, false),
            READY | FAILED => (Direction::PluginToHost, None, false),
            DESTROY => (Direction::HostToPlugin, Some(0), false),
            DESTROYED => (Direction::PluginToHost, Some(0), false),
            ACK_NEXT => (Direction::PluginToHost, Some(0), true),
            STATUS => (Direction::HostToPlugin, Some(4), true),
            REPLY_NEXT => (Direction::HostToPlugin, Some(0), true),
            _ => return Err(self.opcode_violation(opcode)),
        };
        if direction != self.direction {
            return Err(self.opcode_violation(opcode));
        }
        if header.flags != 0 {
            return Err(PolicyViolation::Flags {
                opcode,
                flags: header.flags,
            });
        }
        if fixed_len.is_some_and(|len| len != payload_len) {
            return Err(PolicyViolation::PrivatePayload {
                opcode,
                len: payload_len,
            });
        }
        if !request && header.correlation != 0 {
            return Err(PolicyViolation::PrivateCorrelation {
                opcode,
                correlation: header.correlation,
            });
        }
        match opcode {
            ACK_NEXT => {
                if let Some(previous) = self.last_request
                    && header.correlation <= previous
                {
                    return Err(PolicyViolation::RequestId {
                        id: header.correlation,
                        previous,
                    });
                }
                self.last_request = Some(header.correlation);
                self.expect = Expect::PublicFrame;
            }
            REPLY_NEXT => self.expect = Expect::ReadPlanResult,
            _ => {}
        }
        Ok(())
    }

    fn admit_public(&self, header: &FrameHeader) -> Result<(), PolicyViolation> {
        use vosu_opcode::*;
        let opcode = vosu_opcode::try_from(header.opcode)
            .map_err(|()| self.opcode_violation(header.opcode))?;
        let (to_host, to_plugin) = match opcode {
            VOSU_OP_ACTION_RESULT | VOSU_OP_ACTION_CANCEL | VOSU_OP_ACTION_PROGRESS => (true, true),
            VOSU_OP_PUBLISH_FIELD
            | VOSU_OP_READ_PLAN_SUBMIT
            | VOSU_OP_READ_PLAN_CANCEL
            | VOSU_OP_PUBLISH_BATCH
            | VOSU_OP_SESSION_IDENTIFY
            | VOSU_OP_SESSION_DECLINE
            | VOSU_OP_SCAN_REQUEST
            | VOSU_OP_RESOURCE_RESOLVED
            | VOSU_OP_STREAM_CHUNK
            | VOSU_OP_ROOT_DECLARE
            | VOSU_OP_ACTION_CALL
            | VOSU_OP_LOG_RECORD
            | VOSU_OP_TIMER_SET
            | VOSU_OP_TIMER_CANCEL
            | VOSU_OP_STATE_SUBSCRIBE => (true, false),
            VOSU_OP_DEMAND_CHANGED
            | VOSU_OP_READ_PLAN_RESULT
            | VOSU_OP_SESSION_ATTACHED
            | VOSU_OP_SESSION_DETACHED
            | VOSU_OP_FOCUS_CHANGED
            | VOSU_OP_SCAN_RESULT
            | VOSU_OP_RESOURCE_RESOLVE
            | VOSU_OP_TREE_LIST
            | VOSU_OP_TREE_LOOKUP
            | VOSU_OP_STREAM_READ
            | VOSU_OP_STREAM_CLOSE
            | VOSU_OP_ACTION_INVOKE
            | VOSU_OP_SETTINGS_CHANGED
            | VOSU_OP_TIMER_FIRED
            | VOSU_OP_STATE_UPDATE => (false, true),
        };
        let allowed = match self.direction {
            Direction::PluginToHost => to_host,
            Direction::HostToPlugin => to_plugin,
        };
        if !allowed {
            return Err(self.opcode_violation(header.opcode));
        }
        let allowed_flags = match (self.direction, opcode) {
            (Direction::PluginToHost, VOSU_OP_READ_PLAN_SUBMIT) => VOSU_READ_PLAN_SYNC,
            _ => 0,
        };
        if header.flags & !allowed_flags != 0 {
            return Err(PolicyViolation::Flags {
                opcode: header.opcode,
                flags: header.flags,
            });
        }
        Ok(())
    }

    fn opcode_violation(&self, opcode: u32) -> PolicyViolation {
        PolicyViolation::Opcode {
            opcode,
            direction: self.direction,
        }
    }
}

impl FramePolicy for DirectionPolicy {
    fn admit(&mut self, header: &FrameHeader, payload_len: usize) -> Result<(), PolicyViolation> {
        let expect = std::mem::replace(&mut self.expect, Expect::Any);
        let paired = match expect {
            Expect::Any => true,
            Expect::PublicFrame => !header.is_private(),
            Expect::ReadPlanResult => header.opcode == vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
        };
        if !paired {
            return Err(PolicyViolation::Pairing {
                opcode: header.opcode,
            });
        }
        if header.is_private() {
            self.admit_private(header, payload_len)
        } else {
            self.admit_public(header)
        }
    }
}

#[cfg(test)]
mod tests;

//! Frame codec and per-direction frame policy (`docs/v2/plugin-abi.md`
//! §7.2 "Framing", §7.6 private control frames).
//!
//! On the wire a frame is exactly `u32 total_len; u32 opcode; u32 flags;
//! u64 correlation; u32 payload_len; payload`, little-endian, where
//! `total_len` excludes its own 4 bytes. No Rust struct is ever copied: the
//! 24 head bytes are encoded and decoded field by field. A decoded head is
//! a bounded local snapshot, and every length in it is checked against the
//! universal 1 MiB cap before anything is allocated or indexed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use vosu_plugin_api::{
    VOSU_FRAME_MAX_BYTES, VOSU_OPCODE_PRIVATE_BIT, VOSU_READ_PLAN_MAX_BYTES, VOSU_READ_PLAN_SYNC,
    vosu_opcode, vosu_status,
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
    #[error("STATUS carries {raw}, which is no vosu_status")]
    Status { raw: u32 },
    #[error("public opcode {opcode:#x} arrived without the ACK_NEXT its status needs")]
    Unacknowledged { opcode: u32 },
    #[error("VOSU_OP_LOG_RECORD is never acknowledged")]
    AcknowledgedLog,
    #[error("request {id} would exceed the {MAX_REQUESTS} live status and SYNC requests")]
    RequestCap { id: u64 },
    #[error("SYNC result of {len} bytes exceeds the {VOSU_READ_PLAN_MAX_BYTES}-byte read-plan cap")]
    SyncResultSize { len: usize },
}

/// The concurrent status and `SYNC` waiters of one instance (A5.2): one
/// shared cap, separate from the asynchronous request caps.
pub const MAX_REQUESTS: usize = 64;

/// What one acknowledged plugin → host frame waits for (§7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    /// A `STATUS` reply.
    Status,
    /// A `VOSU_READ_PLAN_SYNC` submission: `REPLY_NEXT` and its result, or
    /// a non-OK `STATUS`.
    Sync,
}

/// The live acknowledged requests of one channel, shared by the side that
/// receives frames (its [`DirectionPolicy`]) and the side that answers or
/// awaits them. At most [`MAX_REQUESTS`] are live at once.
///
/// - Core opens an entry when its policy admits an acknowledged frame
///   (a 65th is the child's protocol violation: the child refuses its own
///   65th waiter before writing) and closes it before writing the reply,
///   so the child's next request can never meet a full ledger.
/// - The child opens an entry when it reserves a waiter, before writing,
///   and closes it on the reply or a release; its policy reads the kind to
///   pair `STATUS` and `REPLY_NEXT` with the request they answer.
#[derive(Debug, Default)]
pub struct Ledger {
    live: Mutex<HashMap<u64, RequestKind>>,
}

impl Ledger {
    fn lock(&self) -> MutexGuard<'_, HashMap<u64, RequestKind>> {
        // Every critical section is one map operation: a panicking holder
        // cannot leave it half-updated.
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Opens `id`. False when [`MAX_REQUESTS`] are already live or `id` is.
    pub fn open(&self, id: u64, kind: RequestKind) -> bool {
        let mut live = self.lock();
        if live.len() >= MAX_REQUESTS || live.contains_key(&id) {
            return false;
        }
        live.insert(id, kind);
        true
    }

    /// The live request `id`'s kind; `None` for an unknown or closed id.
    pub fn kind(&self, id: u64) -> Option<RequestKind> {
        self.lock().get(&id).copied()
    }

    /// Closes `id`, returning its kind if it was live.
    pub fn close(&self, id: u64) -> Option<RequestKind> {
        self.lock().remove(&id)
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Checks each frame's validated header before its payload is copied, and
/// the payloads whose validity depends on their bytes after the copy.
pub trait FramePolicy: Send {
    fn admit(&mut self, header: &FrameHeader, payload_len: usize) -> Result<(), PolicyViolation>;

    /// Runs after the payload of an admitted frame was copied, before the
    /// frame is consumed.
    fn admit_payload(
        &mut self,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Result<(), PolicyViolation> {
        let _ = (header, payload);
        Ok(())
    }

    /// The request ledger this policy shares, if it keeps one.
    fn ledger(&self) -> Option<Arc<Ledger>> {
        None
    }
}

/// The frozen policy for one direction: public ABI opcodes valid that way
/// with their defined flags, the A5.2 private set with its payload and
/// correlation rules, and the request pairing:
///
/// - host → plugin: a `STATUS` carries a real `vosu_status`, and a
///   `REPLY_NEXT` is followed by one `READ_PLAN_RESULT` within the 64 KiB
///   read-plan cap;
/// - with a ledger ([`with_ledger`](Self::with_ledger), every production
///   channel), plugin → host: every public frame except
///   `VOSU_OP_LOG_RECORD` follows its own `ACK_NEXT` (a `LOG_RECORD` never
///   does), and at most [`MAX_REQUESTS`] acknowledged frames are unanswered
///   at once; host → plugin: a `STATUS` is never `VOSU_OK` for a `SYNC`
///   request (an accepted `SYNC` is answered by `REPLY_NEXT`), and a
///   `REPLY_NEXT` never answers a status request. A reply naming an
///   unknown or expired id is admitted: the router discards and counts it.
#[derive(Debug)]
pub struct DirectionPolicy {
    direction: Direction,
    expect: Expect,
    last_request: Option<u64>,
    /// `Some` when this policy accounts requests (`with_ledger`).
    ledger: Option<Arc<Ledger>>,
    /// The kind of the request the `STATUS` being admitted answers.
    status_kind: Option<RequestKind>,
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
    /// The structural policy for frames received in `direction`, without
    /// request accounting: for a channel nobody answers requests on.
    pub fn new(direction: Direction) -> Self {
        Self {
            direction,
            expect: Expect::Any,
            last_request: None,
            ledger: None,
            status_kind: None,
        }
    }

    /// The full policy, sharing a fresh [`Ledger`] through
    /// [`FramePolicy::ledger`] with the side that answers or awaits the
    /// requests it pairs.
    pub fn with_ledger(direction: Direction) -> Self {
        Self {
            ledger: Some(Arc::new(Ledger::default())),
            ..Self::new(direction)
        }
    }

    fn kind(&self, id: u64) -> Option<RequestKind> {
        self.ledger.as_ref().and_then(|ledger| ledger.kind(id))
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
            STATUS => self.status_kind = self.kind(header.correlation),
            REPLY_NEXT => {
                if self.kind(header.correlation) == Some(RequestKind::Status) {
                    return Err(PolicyViolation::Pairing { opcode });
                }
                self.expect = Expect::ReadPlanResult;
            }
            _ => {}
        }
        Ok(())
    }

    /// Plugin → host: the `ACK_NEXT` pairing every public frame obeys, and
    /// the [`MAX_REQUESTS`] cap. `acknowledged` is whether an `ACK_NEXT`
    /// immediately preceded this frame.
    fn admit_request(
        &mut self,
        header: &FrameHeader,
        acknowledged: bool,
    ) -> Result<(), PolicyViolation> {
        let Some(ledger) = self.ledger.as_ref() else {
            return Ok(());
        };
        if self.direction != Direction::PluginToHost {
            return Ok(());
        }
        let log = header.opcode == vosu_opcode::VOSU_OP_LOG_RECORD as u32;
        match (acknowledged, log) {
            (false, true) => Ok(()),
            (true, true) => Err(PolicyViolation::AcknowledgedLog),
            (false, false) => Err(PolicyViolation::Unacknowledged {
                opcode: header.opcode,
            }),
            (true, false) => {
                let id = self
                    .last_request
                    .expect("an ACK_NEXT recorded its id before the frame it pairs");
                let sync = header.opcode == vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT as u32
                    && header.flags & VOSU_READ_PLAN_SYNC != 0;
                let kind = if sync {
                    RequestKind::Sync
                } else {
                    RequestKind::Status
                };
                if ledger.open(id, kind) {
                    Ok(())
                } else {
                    Err(PolicyViolation::RequestCap { id })
                }
            }
        }
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
        if expect == Expect::ReadPlanResult && payload_len > VOSU_READ_PLAN_MAX_BYTES as usize {
            return Err(PolicyViolation::SyncResultSize { len: payload_len });
        }
        if header.is_private() {
            self.admit_private(header, payload_len)
        } else {
            self.admit_public(header)?;
            self.admit_request(header, expect == Expect::PublicFrame)
        }
    }

    fn admit_payload(
        &mut self,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Result<(), PolicyViolation> {
        if header.opcode != control::STATUS {
            return Ok(());
        }
        // `admit` already fixed this private payload at exactly 4 bytes.
        let raw = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let status = vosu_status::try_from(raw).map_err(|_| PolicyViolation::Status { raw })?;
        if status == vosu_status::VOSU_OK && self.status_kind == Some(RequestKind::Sync) {
            return Err(PolicyViolation::Pairing {
                opcode: header.opcode,
            });
        }
        Ok(())
    }

    fn ledger(&self) -> Option<Arc<Ledger>> {
        self.ledger.clone()
    }
}

#[cfg(test)]
mod tests;

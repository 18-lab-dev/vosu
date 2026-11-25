//! One direction's single-producer/single-consumer byte ring over a
//! [`SharedRegion`] (`docs/v2/plugin-abi.md` §7.2 "Channel setup").
//!
//! Region layout (all words little-endian `u64`, accessed atomically):
//!
//! | offset | word | owner |
//! | --- | --- | --- |
//! | 0 | capacity | creator, read once at attach |
//! | 8 | layout magic | creator, read once at attach |
//! | 64 | write cursor | producer |
//! | 72 | space wanted | producer sets, consumer clears |
//! | 128 | read cursor | consumer |
//! | 136 | consumer parked | consumer |
//! | 256.. | data, [`CAPACITY`] bytes | |
//!
//! Cursors are free-running `u64` byte counts that wrap modulo 2^64; a
//! cursor's data index is `cursor & (CAPACITY - 1)`, continuous across that
//! wrap because the capacity is a power of two. Each side keeps its own
//! cursor locally and only ever stores it; it never reads it back from
//! shared memory. Every load of the peer's cursor is validated against the
//! locally known window before it is used, and every frame head is decoded
//! from a bounded local copy. The producer publishes its write cursor only
//! after a whole frame (or several) is in place, so a consumer that finds a
//! published byte count ending inside a frame has met a protocol violation,
//! not a frame in progress.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use vosu_sys::plugin_process::{OutOfBounds, SharedRegion};

use super::frame::{
    FrameError, FrameHeader, FramePolicy, HEAD_BYTES, MAX_FRAME_BYTES, PolicyViolation, decode_head,
};

/// Data bytes per direction. A power of two of at least one maximum frame
/// plus a following maximum-overhead frame (an `ACK_NEXT` ahead of a 1 MiB
/// frame), so a legal frame always fits an empty ring.
pub const CAPACITY: usize = 2 * 1024 * 1024;
/// Header bytes before the data area.
pub const HEADER_BYTES: usize = 256;
/// Exact length of each direction's shared region: the trusted expected
/// mapping size both processes use.
pub const REGION_BYTES: usize = HEADER_BYTES + CAPACITY;

const _: () = assert!(CAPACITY.is_power_of_two());
const _: () = assert!(CAPACITY >= MAX_FRAME_BYTES + HEAD_BYTES);

const CAPACITY_WORD: usize = 0;
const MAGIC_WORD: usize = 8;
const WRITE_CURSOR: usize = 64;
const SPACE_WANTED: usize = 72;
const READ_CURSOR: usize = 128;
const CONSUMER_PARKED: usize = 136;
const MAGIC: u64 = u64::from_le_bytes(*b"vosuRNG1");
const INDEX_MASK: u64 = CAPACITY as u64 - 1;

/// A ring that cannot be attached or whose peer broke the protocol. Every
/// variant ends the channel; none can be retried.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RingError {
    #[error("ring region is {actual} bytes, expected {REGION_BYTES}")]
    RegionSize { actual: usize },
    #[error("ring header names capacity {0}, expected {CAPACITY}")]
    Capacity(u64),
    #[error("ring header magic {0:#x} is not the expected layout")]
    Magic(u64),
    #[error("ring cursors at attach (write {write}, read {read}) exceed the capacity")]
    AttachCursors { write: u64, read: u64 },
    #[error("peer write cursor {cursor} leaves the window [{published}, {read} + capacity]")]
    WriteCursor {
        cursor: u64,
        published: u64,
        read: u64,
    },
    #[error("peer read cursor {cursor} leaves the window [{previous}, {write}]")]
    ReadCursor {
        cursor: u64,
        previous: u64,
        write: u64,
    },
    #[error("published bytes ({available}) end inside a {needed}-byte frame")]
    PartialFrame { available: u64, needed: u64 },
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error(transparent)]
    Policy(#[from] PolicyViolation),
    /// A local access outside the region; impossible for a region whose
    /// size was checked at attach, reported rather than assumed.
    #[error(transparent)]
    Region(#[from] OutOfBounds),
}

/// Writes a fresh header. Only the region's creator calls this, before the
/// region is shared.
pub fn initialize(region: &SharedRegion) -> Result<(), RingError> {
    initialize_at(region, 0)
}

/// [`initialize`] with both cursors starting at `start` (tests place frames
/// across the data wrap and the 2^64 cursor wrap this way).
pub(super) fn initialize_at(region: &SharedRegion, start: u64) -> Result<(), RingError> {
    check_size(region)?;
    store(region, CAPACITY_WORD, CAPACITY as u64)?;
    store(region, MAGIC_WORD, MAGIC)?;
    store(region, WRITE_CURSOR, start)?;
    store(region, SPACE_WANTED, 0)?;
    store(region, READ_CURSOR, start)?;
    store(region, CONSUMER_PARKED, 0)
}

/// Reusable 8-byte-aligned storage a received payload is copied into
/// before any decoder or `dispatch` sees it. It grows only to a validated
/// payload length (at most 1 MiB) and never shrinks.
#[derive(Debug, Default)]
pub struct PayloadBuffer {
    words: Vec<u64>,
    len: usize,
}

impl PayloadBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last received payload; 8-byte aligned.
    pub fn bytes(&self) -> &[u8] {
        let words = self.words.as_slice();
        debug_assert!(self.len <= words.len() * 8);
        // SAFETY: `words` is a live, 8-aligned `u64` allocation of at least
        // `len` bytes (`prepare` sizes it), every bit pattern is a valid `u8`,
        // and the returned borrow keeps `self` (so `words`) borrowed.
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), self.len) }
    }

    /// Bytes allocated for reuse.
    pub fn capacity_bytes(&self) -> usize {
        self.words.capacity() * 8
    }

    /// Replaces the payload with a copy of `bytes`, reusing the allocation.
    /// For payloads built locally (a re-split `STATE_UPDATE`), which must
    /// reach `dispatch` 8-byte aligned like every received one.
    pub fn fill(&mut self, bytes: &[u8]) {
        self.prepare(bytes.len()).copy_from_slice(bytes);
    }

    /// Resizes the payload to `len` bytes (already validated against the
    /// cap) and returns it for filling.
    fn prepare(&mut self, len: usize) -> &mut [u8] {
        let words = len.div_ceil(8);
        if self.words.len() < words {
            self.words.resize(words, 0);
        }
        self.len = len;
        let words = self.words.as_mut_slice();
        // SAFETY: as in `bytes`, with a unique borrow of `words`, which holds
        // at least `len.div_ceil(8) * 8 >= len` bytes.
        unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), len) }
    }
}

/// The producer end of one ring. Exactly one exists per ring; the channel
/// serializes every sender behind it.
#[derive(Debug)]
pub struct RingProducer {
    region: Arc<SharedRegion>,
    write: u64,
    peer_read: u64,
}

impl RingProducer {
    /// Attaches to an initialized region, adopting its current cursors.
    pub fn attach(region: Arc<SharedRegion>) -> Result<Self, RingError> {
        let (write, read) = attach_cursors(&region)?;
        Ok(Self {
            region,
            write,
            peer_read: read,
        })
    }

    /// Copies one encoded frame in and publishes it, or returns `false`
    /// without writing anything when it does not fit the current free
    /// space.
    pub fn try_write(
        &mut self,
        head: &[u8; HEAD_BYTES],
        payload: &[u8],
    ) -> Result<bool, RingError> {
        let needed = (HEAD_BYTES + payload.len()) as u64;
        if self.free()? < needed {
            return Ok(false);
        }
        copy_in(&self.region, self.write, head)?;
        copy_in(
            &self.region,
            self.write.wrapping_add(HEAD_BYTES as u64),
            payload,
        )?;
        self.write = self.write.wrapping_add(needed);
        // Publishes the complete frame: the consumer's acquiring load of
        // this cursor makes every byte above visible to it.
        store(&self.region, WRITE_CURSOR, self.write)?;
        Ok(true)
    }

    /// Free bytes after a fresh, validated read of the consumer's cursor.
    pub fn free(&mut self) -> Result<u64, RingError> {
        let cursor = load(&self.region, READ_CURSOR)?;
        let window = self.write.wrapping_sub(self.peer_read);
        let advance = cursor.wrapping_sub(self.peer_read);
        if advance > window {
            return Err(RingError::ReadCursor {
                cursor,
                previous: self.peer_read,
                write: self.write,
            });
        }
        self.peer_read = cursor;
        Ok(CAPACITY as u64 - self.write.wrapping_sub(cursor))
    }

    /// Asks the consumer to ring after it next frees space. Callers recheck
    /// [`free`](Self::free) after this so a consumer that freed space just
    /// before seeing the flag is not missed.
    pub fn want_space(&self) -> Result<(), RingError> {
        store(&self.region, SPACE_WANTED, 1)
    }

    /// Whether the consumer is (or is about to be) parked on its doorbell
    /// and so needs a ring for data just published.
    pub fn consumer_parked(&self) -> Result<bool, RingError> {
        Ok(load(&self.region, CONSUMER_PARKED)? != 0)
    }
}

/// The consumer end of one ring.
#[derive(Debug)]
pub struct RingConsumer {
    region: Arc<SharedRegion>,
    read: u64,
    published: u64,
}

impl RingConsumer {
    /// Attaches to an initialized region, adopting its current cursors.
    pub fn attach(region: Arc<SharedRegion>) -> Result<Self, RingError> {
        let (write, read) = attach_cursors(&region)?;
        Ok(Self {
            region,
            read,
            published: write,
        })
    }

    /// Published, unconsumed bytes after a fresh, validated read of the
    /// producer's cursor.
    pub fn available(&mut self) -> Result<u64, RingError> {
        let cursor = load(&self.region, WRITE_CURSOR)?;
        let available = cursor.wrapping_sub(self.read);
        let previous = self.published.wrapping_sub(self.read);
        if available > CAPACITY as u64 || available < previous {
            return Err(RingError::WriteCursor {
                cursor,
                published: self.published,
                read: self.read,
            });
        }
        self.published = cursor;
        Ok(available)
    }

    /// Takes the next frame, if one is published: validates its head,
    /// applies `policy`, copies its payload into `buffer`, and releases its
    /// bytes. Returns the header and whether the producer asked to be told
    /// about the freed space.
    pub fn try_read(
        &mut self,
        buffer: &mut PayloadBuffer,
        policy: &mut dyn FramePolicy,
    ) -> Result<Option<(FrameHeader, bool)>, RingError> {
        let available = self.available()?;
        if available == 0 {
            return Ok(None);
        }
        if available < HEAD_BYTES as u64 {
            return Err(RingError::PartialFrame {
                available,
                needed: HEAD_BYTES as u64,
            });
        }
        let mut head = [0_u8; HEAD_BYTES];
        copy_out(&self.region, self.read, &mut head)?;
        let (header, payload_len) = decode_head(&head)?;
        let needed = (HEAD_BYTES + payload_len) as u64;
        if needed > available {
            return Err(RingError::PartialFrame { available, needed });
        }
        policy.admit(&header, payload_len)?;
        let payload = buffer.prepare(payload_len);
        copy_out(
            &self.region,
            self.read.wrapping_add(HEAD_BYTES as u64),
            payload,
        )?;
        policy.admit_payload(&header, payload)?;
        self.read = self.read.wrapping_add(needed);
        // Releases the bytes: the producer's acquiring load of this cursor
        // orders its next writes there after the copy above.
        store(&self.region, READ_CURSOR, self.read)?;
        let space_wanted = self.region.word(SPACE_WANTED)?.swap(0, Ordering::SeqCst) != 0;
        Ok(Some((header, space_wanted)))
    }

    /// Marks the consumer parked (or not). After marking itself parked the
    /// consumer must recheck [`available`](Self::available) before waiting.
    pub fn set_parked(&self, parked: bool) -> Result<(), RingError> {
        store(&self.region, CONSUMER_PARKED, u64::from(parked))
    }
}

fn check_size(region: &SharedRegion) -> Result<(), RingError> {
    if region.len() != REGION_BYTES {
        return Err(RingError::RegionSize {
            actual: region.len(),
        });
    }
    Ok(())
}

fn attach_cursors(region: &SharedRegion) -> Result<(u64, u64), RingError> {
    check_size(region)?;
    let capacity = load(region, CAPACITY_WORD)?;
    if capacity != CAPACITY as u64 {
        return Err(RingError::Capacity(capacity));
    }
    let magic = load(region, MAGIC_WORD)?;
    if magic != MAGIC {
        return Err(RingError::Magic(magic));
    }
    let write = load(region, WRITE_CURSOR)?;
    let read = load(region, READ_CURSOR)?;
    if write.wrapping_sub(read) > CAPACITY as u64 {
        return Err(RingError::AttachCursors { write, read });
    }
    Ok((write, read))
}

fn load(region: &SharedRegion, offset: usize) -> Result<u64, RingError> {
    Ok(u64::from_le(region.word(offset)?.load(Ordering::SeqCst)))
}

fn store(region: &SharedRegion, offset: usize, value: u64) -> Result<(), RingError> {
    region.word(offset)?.store(value.to_le(), Ordering::SeqCst);
    Ok(())
}

/// Copies `src` into the data area starting at `cursor`, wrapping once.
fn copy_in(region: &SharedRegion, cursor: u64, src: &[u8]) -> Result<(), RingError> {
    let index = (cursor & INDEX_MASK) as usize;
    let first = src.len().min(CAPACITY - index);
    region.write(HEADER_BYTES + index, &src[..first])?;
    region.write(HEADER_BYTES, &src[first..])?;
    Ok(())
}

/// Copies `dst.len()` bytes out of the data area starting at `cursor`,
/// wrapping once.
fn copy_out(region: &SharedRegion, cursor: u64, dst: &mut [u8]) -> Result<(), RingError> {
    let index = (cursor & INDEX_MASK) as usize;
    let first = dst.len().min(CAPACITY - index);
    let (head, tail) = dst.split_at_mut(first);
    region.read(HEADER_BYTES + index, head)?;
    region.read(HEADER_BYTES, tail)?;
    Ok(())
}

#[cfg(test)]
mod tests;

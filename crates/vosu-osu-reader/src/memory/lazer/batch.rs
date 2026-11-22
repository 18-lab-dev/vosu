//! Building blocks for decoders that read independent fields in one native batch.
//!
//! Batched decoders return `None` for any failed, invalid, or out-of-bounds read and the
//! caller then runs the original sequential decoder, so errors and partially assigned decoder
//! state stay exactly those of the sequential read order.
use super::*;

/// The smallest protection granule on supported targets. Bytes on the same 4 KiB page as a
/// readable byte are readable, so a read extended only to the end of the page holding the last
/// required byte succeeds exactly when the required read would.
const PAGE: usize = 4096;
const STRING_PREFETCH_UNITS: usize = 128;
const MAX_STRING_UNITS: usize = 4095;
/// Destination size for [`LazerMemory::string_head_span`]: the length plus leading units.
pub(super) const STRING_HEAD: usize = 4 + 2 * STRING_PREFETCH_UNITS;
/// Lists whose pointer slots span more than this take per-slot reads.
pub(super) const BATCHED_LIST_BYTES: usize = 4096;

/// `Dictionary<TKey, TValue>` layout read by lazer decoders: `_entries` at +0x10 and
/// `_count` at +0x38, with entries starting 0x10 into the entries array. One header read
/// covers both fields.
pub(super) const DICTIONARY_HEADER: usize = 0x10;
pub(super) const DICTIONARY_HEADER_LEN: usize = 0x38 + 4 - DICTIONARY_HEADER;
pub(super) const DICTIONARY_COUNT: usize = 0x38 - DICTIONARY_HEADER;

/// Up to `N` independent reads issued as one native batch.
pub(super) struct Stage<'a, const N: usize> {
    requests: [ReadRequest<'a>; N],
    count: usize,
}

impl<'a, const N: usize> Stage<'a, N> {
    pub(super) fn new() -> Self {
        Self {
            requests: std::array::from_fn(|_| ReadRequest::new(0, &mut [])),
            count: 0,
        }
    }

    /// Queues a read; the decoder sizes `N` for its maximum request count.
    pub(super) fn push(&mut self, address: usize, dst: &'a mut [u8]) {
        self.requests[self.count] = ReadRequest::new(address, dst);
        self.count += 1;
    }

    pub(super) fn read(mut self, process: &ProcessHandle) -> Option<()> {
        if self.count == 0 {
            return Some(());
        }
        process.read_many(&mut self.requests[..self.count]).ok()
    }
}

/// Reads one fixed-width value per present address in a single native batch.
pub(super) fn read_fixed<const N: usize, const L: usize>(
    process: &ProcessHandle,
    addresses: [Option<usize>; N],
) -> Option<[[u8; L]; N]> {
    let mut bytes = [[0u8; L]; N];
    let count = addresses.iter().flatten().count();
    {
        let mut pending = bytes
            .iter_mut()
            .zip(addresses)
            .filter_map(|(dst, address)| address.map(|address| ReadRequest::new(address, dst)));
        let mut requests: [ReadRequest<'_>; N] = std::array::from_fn(|_| {
            pending
                .next()
                .unwrap_or_else(|| ReadRequest::new(0, &mut []))
        });
        process.read_many(&mut requests[..count]).ok()?;
    }
    Some(bytes)
}

pub(super) fn le_pointer(bytes: &[u8]) -> Option<usize> {
    usize::try_from(u64::from_le_bytes(bytes[..8].try_into().unwrap())).ok()
}

pub(super) fn pointers<const N: usize>(bytes: [[u8; 8]; N]) -> Option<[usize; N]> {
    let mut out = [0; N];
    for (out, bytes) in out.iter_mut().zip(bytes) {
        *out = le_pointer(&bytes)?;
    }
    Some(out)
}

pub(super) fn le_i32(bytes: &[u8]) -> i32 {
    i32::from_le_bytes(bytes[..4].try_into().unwrap())
}

pub(super) fn le_i64(bytes: &[u8]) -> i64 {
    i64::from_le_bytes(bytes[..8].try_into().unwrap())
}

pub(super) fn le_f64(bytes: &[u8]) -> f64 {
    f64::from_le_bytes(bytes[..8].try_into().unwrap())
}

impl LazerMemory {
    /// Page-clamped span holding a non-null managed string's length (at +pointer size) and
    /// up to [`STRING_PREFETCH_UNITS`] UTF-16 units.
    pub(super) fn string_head_span(&self, object: usize) -> Option<(usize, usize)> {
        let length_address = object.checked_add(self.process.pointer_size())?;
        let page_end = (length_address.checked_add(3)? | (PAGE - 1)).checked_add(1)?;
        Some((length_address, STRING_HEAD.min(page_end - length_address)))
    }

    /// Decodes a string from its [`Self::string_head_span`] bytes with `read_managed_string`
    /// validation, reading any units beyond the head exactly.
    pub(super) fn string_from_head(&self, object: usize, head: &[u8]) -> Option<String> {
        let length = usize::try_from(le_i32(head)).ok()?;
        if length > MAX_STRING_UNITS {
            return None;
        }
        let byte_len = length * 2;
        let prefetched = &head[4..];
        let decode = |bytes: &[u8]| {
            char::decode_utf16(
                bytes
                    .chunks_exact(2)
                    .map(|unit| u16::from_le_bytes([unit[0], unit[1]])),
            )
            .collect::<Result<String, _>>()
            .ok()
        };
        if byte_len <= prefetched.len() {
            return decode(&prefetched[..byte_len]);
        }
        let chars_address = object
            .checked_add(self.process.pointer_size() + 4)?
            .checked_add(prefetched.len())?;
        let mut bytes = vec![0; byte_len];
        bytes[..prefetched.len()].copy_from_slice(prefetched);
        self.process
            .read_exact(chars_address, &mut bytes[prefetched.len()..])
            .ok()?;
        decode(&bytes)
    }
}

//! Borrowed publish decode and the reusable batch writer
//! (`VOSU_OP_PUBLISH_FIELD`/`VOSU_OP_PUBLISH_BATCH`, D4, S2): split out of
//! `message::flat` to keep that file under the maintained-file length
//! limit (`docs/architecture/source-map.md`). [`decode_publish_field_ref`]/
//! [`decode_publish_batch_ref`] are the allocation-free counterparts of
//! [`super::decode_publish_field`]/[`super::decode_publish_batch`], and
//! [`PublishBatchWriter`] is the allocation-free counterpart of
//! [`super::encode_publish_batch`].

use core::mem::size_of;

use crate::flat::publish::{vosu_field_publish_header, vosu_publish_batch_header, vosu_scalar};
use crate::message::MessageError;
use crate::validate::{self, read_flat_header};
use crate::value::vosu_value_kind;

use super::{push_struct, read_header_at};

/// `vosu_field_publish_header::value_kind` plus its payload, borrowed
/// (D4; [`super::PublishValue`] is the owning equivalent). A CBOR-kind value
/// (`Cbor`/`ResourceRef`/`TreeRef`) stays as its raw encoded bytes here:
/// decoding it into a [`crate::CborValue`] always allocates, so a caller
/// on the allocation-free hot path decodes it only once it knows the
/// pointer is actually covered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PublishValueRef<'a> {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    /// UTF-8
    String(&'a str),
    /// opaque
    Bytes(&'a [u8]),
    /// raw CBOR bytes, not yet decoded
    Cbor(&'a [u8]),
    /// raw CBOR bytes, not yet decoded
    ResourceRef(&'a [u8]),
    /// raw CBOR bytes, not yet decoded
    TreeRef(&'a [u8]),
}

/// `VOSU_OP_PUBLISH_FIELD`, or one `VOSU_OP_PUBLISH_BATCH` entry, borrowed
/// ([`super::PublishField`] is the owning equivalent).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PublishFieldRef<'a> {
    pub handle: u64,
    pub ns: &'a str,
    pub pointer: &'a str,
    pub value: PublishValueRef<'a>,
}

fn borrowed_str(buffer: &[u8], offset: u32, len: u32) -> Result<&str, MessageError> {
    let range = offset as usize..(offset as usize + len as usize);
    core::str::from_utf8(&buffer[range]).map_err(MessageError::InvalidUtf8)
}

fn decode_value_ref(
    buffer: &[u8],
    value_kind: u32,
    scalar: vosu_scalar,
    data_offset: u32,
    data_len: u32,
) -> Result<PublishValueRef<'_>, MessageError> {
    let kind = vosu_value_kind::try_from(value_kind)
        .map_err(|()| MessageError::Flat(crate::ValidationError::InvalidEnumValue))?;
    let data = &buffer[data_offset as usize..(data_offset as usize + data_len as usize)];
    Ok(match kind {
        vosu_value_kind::VOSU_VALUE_NULL => PublishValueRef::Null,
        // SAFETY: see `decode_value`'s matching arm above.
        vosu_value_kind::VOSU_VALUE_BOOL => PublishValueRef::Bool(unsafe { scalar.b } != 0),
        // SAFETY: see `decode_value`'s matching arm above.
        vosu_value_kind::VOSU_VALUE_I64 => PublishValueRef::I64(unsafe { scalar.i64 }),
        // SAFETY: see `decode_value`'s matching arm above.
        vosu_value_kind::VOSU_VALUE_U64 => PublishValueRef::U64(unsafe { scalar.u64 }),
        // SAFETY: see `decode_value`'s matching arm above.
        vosu_value_kind::VOSU_VALUE_F64 => PublishValueRef::F64(unsafe { scalar.f64 }),
        vosu_value_kind::VOSU_VALUE_STRING => {
            PublishValueRef::String(core::str::from_utf8(data).map_err(MessageError::InvalidUtf8)?)
        }
        vosu_value_kind::VOSU_VALUE_BYTES => PublishValueRef::Bytes(data),
        vosu_value_kind::VOSU_VALUE_CBOR => PublishValueRef::Cbor(data),
        vosu_value_kind::VOSU_VALUE_RESOURCE_REF => PublishValueRef::ResourceRef(data),
        vosu_value_kind::VOSU_VALUE_TREE_REF => PublishValueRef::TreeRef(data),
    })
}

fn decode_field_header_ref<'a>(
    buffer: &'a [u8],
    header: &vosu_field_publish_header,
) -> Result<PublishFieldRef<'a>, MessageError> {
    Ok(PublishFieldRef {
        handle: header.handle,
        ns: borrowed_str(buffer, header.ns_offset, header.ns_len)?,
        pointer: borrowed_str(buffer, header.pointer_offset, header.pointer_len)?,
        value: decode_value_ref(
            buffer,
            header.value_kind,
            header.scalar,
            header.data_offset,
            header.data_len,
        )?,
    })
}

/// Borrowed decode of `VOSU_OP_PUBLISH_FIELD`: allocates nothing, keeping
/// every `ns`/`pointer`/`data` range as a borrow into `buffer`
/// ([`super::decode_publish_field`] is the owning equivalent).
pub fn decode_publish_field_ref(buffer: &[u8]) -> Result<PublishFieldRef<'_>, MessageError> {
    validate::validate_publish_field(buffer)?;
    let header: vosu_field_publish_header = read_flat_header(buffer)?;
    decode_field_header_ref(buffer, &header)
}

/// Borrowed decode of `VOSU_OP_PUBLISH_BATCH` (D4): validates the whole
/// payload and every entry's `ns`/`pointer`/string UTF-8 once, up front, so
/// [`PublishBatchRef::entries`] can be an infallible, allocation-free
/// iterator over borrows into `buffer` ([`super::decode_publish_batch`] is the
/// owning equivalent).
pub fn decode_publish_batch_ref(buffer: &[u8]) -> Result<PublishBatchRef<'_>, MessageError> {
    validate::validate_publish_batch(buffer)?;
    let header: vosu_publish_batch_header = read_flat_header(buffer)?;
    let elem_size = size_of::<vosu_field_publish_header>();
    for index in 0..header.entry_count as usize {
        let entry: vosu_field_publish_header =
            read_header_at(buffer, header.entries_offset as usize + index * elem_size)?;
        decode_field_header_ref(buffer, &entry)?;
    }
    Ok(PublishBatchRef {
        handle: header.handle,
        buffer,
        entries_offset: header.entries_offset,
        entry_count: header.entry_count,
    })
}

/// [`decode_publish_batch_ref`]'s result: one handle plus a lazy,
/// allocation-free view of its entries.
#[derive(Debug, Clone, Copy)]
pub struct PublishBatchRef<'a> {
    pub handle: u64,
    buffer: &'a [u8],
    entries_offset: u32,
    entry_count: u32,
}

impl<'a> PublishBatchRef<'a> {
    pub fn len(&self) -> usize {
        self.entry_count as usize
    }

    pub fn is_empty(&self) -> bool {
        self.entry_count == 0
    }

    /// Iterates every entry, re-reading its header and re-deriving its
    /// borrows from `buffer` on each step rather than collecting them:
    /// [`decode_publish_batch_ref`] already validated every entry
    /// (including `ns`/`pointer`/string UTF-8), so these re-reads cannot
    /// fail.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = PublishFieldRef<'a>> + 'a {
        let buffer = self.buffer;
        let offset = self.entries_offset;
        let elem_size = size_of::<vosu_field_publish_header>();
        (0..self.entry_count as usize).map(move |index| {
            let entry: vosu_field_publish_header =
                read_header_at(buffer, offset as usize + index * elem_size)
                    .expect("validated by decode_publish_batch_ref");
            decode_field_header_ref(buffer, &entry).expect("validated by decode_publish_batch_ref")
        })
    }
}

/// One [`PublishBatchWriter::push`]/`push_cbor_with` call's header fields:
/// every `{offset, len}` here names a range inside the writer's own
/// `data` buffer, not yet rebased to its final position after the batch
/// header and entry array ([`finish_into`](PublishBatchWriter::finish_into)
/// does that same rebase [`build_field_header`]/[`rebase_field_header`] do
/// for the owning [`super::encode_publish_batch`]).
struct PendingEntry {
    handle: u64,
    ns_offset: u32,
    ns_len: u32,
    pointer_offset: u32,
    pointer_len: u32,
    value_kind: u32,
    scalar: vosu_scalar,
    data_offset: u32,
    data_len: u32,
}

/// Reusable `VOSU_OP_PUBLISH_BATCH` writer (D4). `push`/`push_cbor_with`
/// append one entry's `ns`/`pointer`/value straight into `self.data`
/// (never an intermediate per-entry `Vec`, unlike [`super::encode_publish_batch`]'s
/// owned path), and `finish_into` lays the whole batch out into a
/// caller-owned buffer. `begin` clears `self.entries`/`self.data` but
/// keeps their capacity, so a writer reused across batches of the same
/// shape is allocation-free once both have grown to their steady-state
/// size.
pub struct PublishBatchWriter {
    handle: u64,
    entries: Vec<PendingEntry>,
    data: Vec<u8>,
}

impl Default for PublishBatchWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl PublishBatchWriter {
    pub fn new() -> Self {
        Self {
            handle: 0,
            entries: Vec::new(),
            data: Vec::new(),
        }
    }

    /// Starts a new batch for `handle`, clearing any previous batch's
    /// entries and data but keeping both buffers' capacity.
    pub fn begin(&mut self, handle: u64) {
        self.handle = handle;
        self.entries.clear();
        self.data.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Appends one entry for the batch started by the last [`Self::begin`].
    pub fn push(&mut self, ns: &str, pointer: &str, value: PublishValueRef<'_>) {
        let (value_kind, scalar, extra): (u32, vosu_scalar, &[u8]) = match value {
            PublishValueRef::Null => (
                vosu_value_kind::VOSU_VALUE_NULL as u32,
                vosu_scalar { u64: 0 },
                &[],
            ),
            PublishValueRef::Bool(b) => (
                vosu_value_kind::VOSU_VALUE_BOOL as u32,
                vosu_scalar { u64: u64::from(b) },
                &[],
            ),
            PublishValueRef::I64(v) => (
                vosu_value_kind::VOSU_VALUE_I64 as u32,
                vosu_scalar { i64: v },
                &[],
            ),
            PublishValueRef::U64(v) => (
                vosu_value_kind::VOSU_VALUE_U64 as u32,
                vosu_scalar { u64: v },
                &[],
            ),
            PublishValueRef::F64(v) => (
                vosu_value_kind::VOSU_VALUE_F64 as u32,
                vosu_scalar { f64: v },
                &[],
            ),
            PublishValueRef::String(s) => (
                vosu_value_kind::VOSU_VALUE_STRING as u32,
                vosu_scalar { u64: 0 },
                s.as_bytes(),
            ),
            PublishValueRef::Bytes(b) => (
                vosu_value_kind::VOSU_VALUE_BYTES as u32,
                vosu_scalar { u64: 0 },
                b,
            ),
            PublishValueRef::Cbor(b) => (
                vosu_value_kind::VOSU_VALUE_CBOR as u32,
                vosu_scalar { u64: 0 },
                b,
            ),
            PublishValueRef::ResourceRef(b) => (
                vosu_value_kind::VOSU_VALUE_RESOURCE_REF as u32,
                vosu_scalar { u64: 0 },
                b,
            ),
            PublishValueRef::TreeRef(b) => (
                vosu_value_kind::VOSU_VALUE_TREE_REF as u32,
                vosu_scalar { u64: 0 },
                b,
            ),
        };
        let ns_offset = self.data.len() as u32;
        self.data.extend_from_slice(ns.as_bytes());
        let pointer_offset = self.data.len() as u32;
        self.data.extend_from_slice(pointer.as_bytes());
        let data_offset = self.data.len() as u32;
        self.data.extend_from_slice(extra);
        self.entries.push(PendingEntry {
            handle: self.handle,
            ns_offset,
            ns_len: ns.len() as u32,
            pointer_offset,
            pointer_len: pointer.len() as u32,
            value_kind,
            scalar,
            data_offset,
            data_len: extra.len() as u32,
        });
    }

    /// Appends one `VOSU_VALUE_CBOR` entry, serializing it in place:
    /// `write` appends the encoded CBOR bytes directly to `self.data`
    /// (reused buffer, no intermediate allocation), rather than building
    /// them in a `Vec` of their own first.
    pub fn push_cbor_with(&mut self, ns: &str, pointer: &str, write: impl FnOnce(&mut Vec<u8>)) {
        let ns_offset = self.data.len() as u32;
        self.data.extend_from_slice(ns.as_bytes());
        let pointer_offset = self.data.len() as u32;
        self.data.extend_from_slice(pointer.as_bytes());
        let data_offset = self.data.len() as u32;
        write(&mut self.data);
        let data_len = self.data.len() as u32 - data_offset;
        self.entries.push(PendingEntry {
            handle: self.handle,
            ns_offset,
            ns_len: ns.len() as u32,
            pointer_offset,
            pointer_len: pointer.len() as u32,
            value_kind: vosu_value_kind::VOSU_VALUE_CBOR as u32,
            scalar: vosu_scalar { u64: 0 },
            data_offset,
            data_len,
        });
    }

    /// Lays the batch started by the last [`Self::begin`] out into `out`
    /// (`out.clear()`ed first, capacity kept — the same reuse pattern
    /// `super::encode_submit_into` uses), 8-byte aligned (`vosu_field_publish_header`'s
    /// own size already is, so the entry array and `self.data` need no
    /// extra padding between them), capped at
    /// [`crate::limits::VOSU_PUBLISH_BATCH_MAX_BYTES`] like every other
    /// encode in this module.
    pub fn finish_into(&mut self, out: &mut Vec<u8>) -> Result<(), MessageError> {
        let batch_header_size = size_of::<vosu_publish_batch_header>() as u32;
        let entry_size = size_of::<vosu_field_publish_header>() as u32;
        let entries_offset = batch_header_size;
        let entries_array_end = entries_offset + entry_size * self.entries.len() as u32;

        out.clear();
        push_struct(
            out,
            &vosu_publish_batch_header {
                handle: self.handle,
                entries_offset,
                entry_count: self.entries.len() as u32,
            },
        );
        for entry in &self.entries {
            push_struct(
                out,
                &vosu_field_publish_header {
                    handle: entry.handle,
                    ns_offset: entries_array_end + entry.ns_offset,
                    ns_len: entry.ns_len,
                    pointer_offset: entries_array_end + entry.pointer_offset,
                    pointer_len: entry.pointer_len,
                    value_kind: entry.value_kind,
                    reserved: 0,
                    scalar: entry.scalar,
                    data_offset: entries_array_end + entry.data_offset,
                    data_len: entry.data_len,
                },
            );
        }
        out.extend_from_slice(&self.data);
        validate::validate_frame_len(out.len())?;
        Ok(())
    }
}

//! Typed scalar comparison and reusable encoded buffers for whole CBOR subtrees.
use super::OsuPointer;
use vosu_plugin_api::{PublishBatchWriter, PublishValueRef};

pub trait ProjectionSink {
    fn put(&mut self, at: OsuPointer, value: PublishValueRef<'_>);
    fn put_cbor(&mut self, at: OsuPointer, write: &mut dyn FnMut(&mut Vec<u8>));
}

#[derive(Default)]
struct LastValue {
    valid: bool,
    kind: u8,
    scalar: u64,
    bytes: Vec<u8>,
}

pub struct LastValues {
    values: Vec<LastValue>,
    scratch: Vec<u8>,
}
impl Default for LastValues {
    fn default() -> Self {
        Self {
            values: (0..OsuPointer::ALL.len())
                .map(|_| LastValue::default())
                .collect(),
            scratch: Vec::new(),
        }
    }
}
impl LastValues {
    /// Refused batches must be retried, not mistaken for committed values.
    pub(crate) fn invalidate(&mut self) {
        for value in &mut self.values {
            value.valid = false;
        }
    }
}

pub struct DiffSink<'a> {
    last: &'a mut LastValues,
    writer: &'a mut PublishBatchWriter,
}
impl<'a> DiffSink<'a> {
    pub fn new(last: &'a mut LastValues, writer: &'a mut PublishBatchWriter) -> Self {
        Self { last, writer }
    }
}
impl ProjectionSink for DiffSink<'_> {
    fn put(&mut self, at: OsuPointer, value: PublishValueRef<'_>) {
        let (kind, scalar, bytes): (u8, u64, &[u8]) = match value {
            PublishValueRef::Null => (0, 0, &[]),
            PublishValueRef::Bool(v) => (1, u64::from(v), &[]),
            PublishValueRef::I64(v) => (2, v as u64, &[]),
            PublishValueRef::U64(v) => (3, v, &[]),
            PublishValueRef::F64(v) => (4, v.to_bits(), &[]),
            PublishValueRef::String(v) => (5, 0, v.as_bytes()),
            PublishValueRef::Bytes(v) => (6, 0, v),
            PublishValueRef::Cbor(v) => (7, 0, v),
            PublishValueRef::ResourceRef(v) => (8, 0, v),
            PublishValueRef::TreeRef(v) => (9, 0, v),
        };
        let last = &mut self.last.values[at as usize];
        if last.valid && last.kind == kind && last.scalar == scalar && last.bytes == bytes {
            return;
        }
        last.valid = true;
        last.kind = kind;
        last.scalar = scalar;
        last.bytes.clear();
        last.bytes.extend_from_slice(bytes);
        self.writer.push(at.ns(), at.pointer(), value);
    }
    fn put_cbor(&mut self, at: OsuPointer, write: &mut dyn FnMut(&mut Vec<u8>)) {
        // Move the single scratch buffer out temporarily, retaining its capacity.
        let mut scratch = std::mem::take(&mut self.last.scratch);
        scratch.clear();
        write(&mut scratch);
        self.put(at, PublishValueRef::Cbor(&scratch));
        self.last.scratch = scratch;
    }
}

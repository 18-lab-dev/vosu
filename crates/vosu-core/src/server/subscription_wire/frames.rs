use std::{io, sync::Arc};

use bytes::{BufMut, Bytes, BytesMut, buf::Writer};
use serde::Serialize;
use serde_json::{
    Value,
    ser::{Formatter, Serializer},
};

/// Selected values retain absence separately from explicit JSON null.
pub type Extraction = Arc<[Option<Value>]>;

/// One selected value, owned by an [`Extraction`] or borrowed from the dispatched document.
pub trait Selected {
    fn value(&self) -> Option<&Value>;
}

impl Selected for Option<Value> {
    fn value(&self) -> Option<&Value> {
        self.as_ref()
    }
}

impl Selected for Option<&Value> {
    fn value(&self) -> Option<&Value> {
        *self
    }
}

/// Collects into `changed`, in id order, the ids whose value differs from `baseline`.
pub fn changes(baseline: &[Option<Value>], current: &[impl Selected], changed: &mut Vec<usize>) {
    debug_assert_eq!(baseline.len(), current.len(), "one plan selects both");
    changed.clear();
    changed.extend(
        baseline
            .iter()
            .zip(current)
            .enumerate()
            .filter(|(_, (old, new))| old.as_ref() != new.value())
            .map(|(id, _)| id),
    );
}

/// Records `selected` as a client's baseline. A baseline only this subscription holds is not
/// published, so only the `changed` ids are updated, in place. A shared one is replaced, never
/// mutated: its other holders, and frame memos keyed on its address, rely on its contents.
pub fn store_baseline(
    baseline: &mut Option<Extraction>,
    selected: &[Option<&Value>],
    changed: &[usize],
) {
    if let Some(values) = baseline.as_mut().and_then(Arc::get_mut)
        && values.len() == selected.len()
    {
        for &id in changed {
            values[id] = selected[id].cloned();
        }
    } else {
        *baseline = Some(selected.iter().map(|value| value.cloned()).collect());
    }
}

/// Number spelling identical to the state bus's `write_json` (`JSON.stringify`);
/// pointer frames serialize straight into a [`FrameArena`] rather than through
/// `encode_dto`'s per-frame growing `Vec`.
struct FrameFormatter;

impl Formatter for FrameFormatter {
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        crate::state::ecma_number::write_js_number_to(writer, f64::from(value))
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        crate::state::ecma_number::write_js_number_to(writer, value)
    }
}

/// The generic serde encoding of `value` with the frames' number spelling:
/// the oracle the direct frame writer is compared against.
#[cfg(test)]
pub(crate) fn encode_js<T: Serialize>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value
        .serialize(&mut Serializer::with_formatter(&mut out, FrameFormatter))
        .expect("test frames serialize");
    out
}

/// Initial capacity of each frame arena chunk.
pub(crate) const FRAME_ARENA_CAPACITY: usize = 4096;

/// Writes a frame's JSON skeleton directly, with the same punctuation and integer spelling as
/// serde_json's compact output for the equivalent tuple; only selected values go through serde.
struct FrameWriter<'a>(Writer<&'a mut BytesMut>);

impl FrameWriter<'_> {
    fn raw(&mut self, bytes: &'static [u8]) {
        self.0.get_mut().extend_from_slice(bytes);
    }

    fn integer(&mut self, value: u64) -> serde_json::Result<()> {
        FrameFormatter
            .write_u64(&mut self.0, value)
            .map_err(serde_json::Error::io)
    }

    /// Writes `[[id,value],...]` followed by `then`, merging the punctuation around the pairs
    /// into as few writes as possible: `then.0` follows written pairs, `then.1` an empty list.
    fn pairs<'v>(
        &mut self,
        pairs: impl Iterator<Item = (usize, &'v Value)>,
        then: (&'static [u8], &'static [u8]),
    ) -> serde_json::Result<()> {
        /// Ids below ten with their following comma, which spares the formatter.
        const SMALL_IDS: [&[u8]; 10] = [
            b"0,", b"1,", b"2,", b"3,", b"4,", b"5,", b"6,", b"7,", b"8,", b"9,",
        ];
        let mut written = false;
        for (id, value) in pairs {
            self.raw(if written { b",[" } else { b"[[" });
            written = true;
            if let Some(&text) = SMALL_IDS.get(id) {
                self.raw(text);
            } else {
                self.integer(id as u64)?;
                self.raw(b",");
            }
            value.serialize(&mut Serializer::with_formatter(&mut self.0, FrameFormatter))?;
            self.raw(b"]");
        }
        self.raw(if written { then.0 } else { then.1 });
        Ok(())
    }
}

/// Frames encoded during one dispatch, split off shared chunks so each frame costs no
/// allocation of its own and clones only bump a reference count. A queued frame pins at most
/// its chunk: `FRAME_ARENA_CAPACITY`, or the larger buffer an oversized frame needed.
#[derive(Default)]
pub struct FrameArena {
    buffer: Option<BytesMut>,
    /// The revision last written and its digits: a dispatch writes one revision into every
    /// frame, so it is formatted once.
    revision: Option<(u64, Vec<u8>)>,
}

impl FrameArena {
    /// Encodes `kind` (`[0,` or `[1,`), the revision, and a comma, followed by what `write`
    /// writes.
    fn encode(
        &mut self,
        kind: &'static [u8],
        revision: u64,
        write: impl FnOnce(&mut FrameWriter<'_>) -> serde_json::Result<()>,
    ) -> serde_json::Result<Bytes> {
        if self
            .revision
            .as_ref()
            .is_none_or(|(cached, _)| *cached != revision)
        {
            let mut digits = Vec::new();
            FrameFormatter
                .write_u64(&mut digits, revision)
                .map_err(serde_json::Error::io)?;
            self.revision = Some((revision, digits));
        }
        let digits = self.revision.as_ref().map_or(&[][..], |(_, digits)| digits);
        let buffer = self
            .buffer
            .get_or_insert_with(|| BytesMut::with_capacity(FRAME_ARENA_CAPACITY));
        buffer.extend_from_slice(kind);
        buffer.extend_from_slice(digits);
        buffer.extend_from_slice(b",");
        let written = write(&mut FrameWriter((&mut *buffer).writer()));
        match written {
            Ok(()) => Ok(buffer.split().freeze()),
            Err(error) => {
                buffer.clear();
                Err(error)
            }
        }
    }

    /// `[0,revision,[[id,value],...]]` with every present value.
    pub fn initial(&mut self, revision: u64, state: &[impl Selected]) -> serde_json::Result<Bytes> {
        self.encode(b"[0,", revision, |frame| {
            frame.pairs(
                state
                    .iter()
                    .enumerate()
                    .filter_map(|(id, value)| value.value().map(|value| (id, value))),
                (b"]]", b"[]]"),
            )
        })
    }

    /// `[1,revision,[[id,value],...],[id,...]]`: of the `changed` ids, those now present with
    /// their values, then those now absent.
    pub fn update(
        &mut self,
        revision: u64,
        current: &[impl Selected],
        changed: &[usize],
    ) -> serde_json::Result<Bytes> {
        self.encode(b"[1,", revision, |frame| {
            frame.pairs(
                changed
                    .iter()
                    .filter_map(|&id| current[id].value().map(|value| (id, value))),
                (b"],[", b"[],["),
            )?;
            let removed = changed.iter().filter(|&&id| current[id].value().is_none());
            for (index, &id) in removed.enumerate() {
                if index > 0 {
                    frame.raw(b",");
                }
                frame.integer(id as u64)?;
            }
            frame.raw(b"]]");
            Ok(())
        })
    }
}

#[cfg(test)]
pub(crate) fn initial_frame(revision: u64, state: &[Option<Value>]) -> serde_json::Result<Bytes> {
    FrameArena::default().initial(revision, state)
}

#[cfg(test)]
pub(crate) fn update_frame(
    revision: u64,
    baseline: &[Option<Value>],
    current: &[Option<Value>],
) -> serde_json::Result<Bytes> {
    let mut changed = Vec::new();
    changes(baseline, current, &mut changed);
    FrameArena::default().update(revision, current, &changed)
}

pub fn error_frame(code: u16, message: &'static str) -> Bytes {
    serde_json::to_vec(&(2_u8, code, message))
        .map(Bytes::from)
        .unwrap_or_else(|_| Bytes::from_static(b"[2,4,\"state serialization failed\"]"))
}

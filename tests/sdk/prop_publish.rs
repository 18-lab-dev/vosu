//! `PublishBatchWriter`/`decode_publish_batch_ref` (D4, S2): a `prop_*`
//! roundtrip proving the borrowed writer/decoder pair agrees with the
//! owning `encode_publish_batch`/`decode_publish_batch` path (reached here
//! through `Message`, since those two functions are not re-exported on
//! their own), plus counting-allocator proof that both the borrowed decode
//! and the writer are allocation-free once warmed.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use proptest::prelude::*;
use vosu_plugin_api::{
    CborValue, Message, PublishBatchWriter, PublishField, PublishValue, PublishValueRef,
    decode_publish_batch_ref, decode_publish_field_ref, encode, vosu_opcode,
};
use vosu_proptest::vosu_proptest_config;

fn arbitrary_ns() -> impl Strategy<Value = String> {
    "[a-z][a-z]{1,8}\\.[a-z][a-z]{1,8}"
}

fn arbitrary_pointer() -> impl Strategy<Value = String> {
    proptest::collection::vec("[a-z]{1,8}", 0..4).prop_map(|segments| {
        segments
            .into_iter()
            .fold(String::new(), |mut pointer, segment| {
                pointer.push('/');
                pointer.push_str(&segment);
                pointer
            })
    })
}

/// Every [`PublishValue`] kind the writer's `push` (rather than
/// `push_cbor_with`) can carry: the CBOR-encoded kinds
/// (`Cbor`/`ResourceRef`/`TreeRef`) go through a separate code path
/// ([`PublishBatchWriter::push_cbor_with`]) and are out of this roundtrip's
/// scope.
fn arbitrary_value() -> impl Strategy<Value = PublishValue> {
    prop_oneof![
        Just(PublishValue::Null),
        any::<bool>().prop_map(PublishValue::Bool),
        any::<i64>().prop_map(PublishValue::I64),
        any::<u64>().prop_map(PublishValue::U64),
        any::<f64>().prop_map(PublishValue::F64),
        "[a-zA-Z0-9 ÀéñÜ日本語🎮]{0,24}".prop_map(PublishValue::String),
        proptest::collection::vec(any::<u8>(), 0..24).prop_map(PublishValue::Bytes),
    ]
}

fn arbitrary_entries() -> impl Strategy<Value = Vec<(String, String, PublishValue)>> {
    proptest::collection::vec(
        (arbitrary_ns(), arbitrary_pointer(), arbitrary_value()),
        0..8,
    )
}

fn value_ref_of(value: &PublishValue) -> PublishValueRef<'_> {
    match value {
        PublishValue::Null => PublishValueRef::Null,
        PublishValue::Bool(b) => PublishValueRef::Bool(*b),
        PublishValue::I64(v) => PublishValueRef::I64(*v),
        PublishValue::U64(v) => PublishValueRef::U64(*v),
        PublishValue::F64(v) => PublishValueRef::F64(*v),
        PublishValue::String(s) => PublishValueRef::String(s),
        PublishValue::Bytes(b) => PublishValueRef::Bytes(b),
        PublishValue::Cbor(_) | PublishValue::ResourceRef(_) | PublishValue::TreeRef(_) => {
            unreachable!("arbitrary_value never generates a CBOR-kind value")
        }
    }
}

fn values_match(value: &PublishValue, value_ref: PublishValueRef<'_>) -> bool {
    match (value, value_ref) {
        (PublishValue::Null, PublishValueRef::Null) => true,
        (PublishValue::Bool(a), PublishValueRef::Bool(b)) => *a == b,
        (PublishValue::I64(a), PublishValueRef::I64(b)) => *a == b,
        (PublishValue::U64(a), PublishValueRef::U64(b)) => *a == b,
        (PublishValue::F64(a), PublishValueRef::F64(b)) => a.to_bits() == b.to_bits(),
        (PublishValue::String(a), PublishValueRef::String(b)) => a == b,
        (PublishValue::Bytes(a), PublishValueRef::Bytes(b)) => a.as_slice() == b,
        _ => false,
    }
}

proptest! {
    #![proptest_config(vosu_proptest_config())]

    /// `PublishBatchWriter::push`/`finish_into` lays out bytes that both
    /// `decode_publish_batch_ref` (borrowed) and the owning decode behind
    /// `Message::decode` agree with, and with the entries the writer was
    /// given.
    #[test]
    fn prop_publish_batch_writer_roundtrips_through_ref_and_owned_decoders(
        handle in any::<u64>(),
        entries in arbitrary_entries(),
    ) {
        let mut writer = PublishBatchWriter::new();
        writer.begin(handle);
        for (ns, pointer, value) in &entries {
            writer.push(ns, pointer, value_ref_of(value));
        }
        let mut buf = Vec::new();
        writer.finish_into(&mut buf).expect("finish_into");

        let batch_ref = decode_publish_batch_ref(&buf).expect("decode_publish_batch_ref");
        prop_assert_eq!(batch_ref.handle, handle);
        prop_assert_eq!(batch_ref.len(), entries.len());
        prop_assert_eq!(batch_ref.is_empty(), entries.is_empty());
        for (entry_ref, (ns, pointer, value)) in batch_ref.entries().zip(&entries) {
            prop_assert_eq!(entry_ref.handle, handle);
            prop_assert_eq!(entry_ref.ns, ns.as_str());
            prop_assert_eq!(entry_ref.pointer, pointer.as_str());
            prop_assert!(values_match(value, entry_ref.value));
        }

        let decoded = Message::decode(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, &buf)
            .expect("Message::decode");
        let Message::PublishBatch(owned) = decoded else {
            panic!("expected Message::PublishBatch, got {decoded:?}");
        };
        prop_assert_eq!(owned.handle, handle);
        prop_assert_eq!(owned.entries.len(), entries.len());
        for (owned_entry, (ns, pointer, value)) in owned.entries.iter().zip(&entries) {
            prop_assert_eq!(owned_entry.handle, handle);
            prop_assert_eq!(&owned_entry.ns, ns);
            prop_assert_eq!(&owned_entry.pointer, pointer);
            prop_assert_eq!(&owned_entry.value, value);
        }
    }
}

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method below forwards unchanged to `std::alloc::System`, a
// valid `GlobalAlloc` implementation; this wrapper only adds allocation
// counting around the delegated calls (mirrors `plan_memory_alloc.rs`'s
// own `TrackingAllocator`, a separate type in a separate test binary).
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `ptr`, `layout`, and `new_size` unchanged to
        // `System::realloc`, which has the same safety contract as this
        // method.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwards `ptr` and `layout` unchanged to `System::dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn allocations_during<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|count| count.set(0));
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
    let result = operation();
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    (result, allocations)
}

fn fill_sample_batch(writer: &mut PublishBatchWriter, buf: &mut Vec<u8>) {
    writer.begin(1);
    writer.push("vosu.osu", "/play/combo", PublishValueRef::U64(42));
    writer.push("vosu.osu", "/play/pp/current", PublishValueRef::F64(123.4));
    writer.push(
        "vosu.osu",
        "/play/accuracyKind",
        PublishValueRef::String("osu-standard"),
    );
    writer.finish_into(buf).expect("finish_into");
}

/// [`decode_publish_batch_ref`] and iterating [`PublishBatchRef::entries`]
/// (`crate::PublishBatchRef`) allocate nothing once `buf` has grown to its
/// final size (S2's allocation-free acceptance criterion).
#[test]
fn publish_batch_ref_decode_allocates_nothing() {
    let mut writer = PublishBatchWriter::new();
    let mut buf = Vec::new();
    // Warm-up: grows `buf` to its steady-state capacity and primes any
    // one-time lazy state before measuring.
    for _ in 0..2 {
        fill_sample_batch(&mut writer, &mut buf);
        let _ = decode_publish_batch_ref(&buf).expect("warm-up decode");
    }

    let (entry_count, allocations) = allocations_during(|| {
        let batch = decode_publish_batch_ref(&buf).expect("decode_publish_batch_ref");
        let mut count = 0usize;
        for entry in batch.entries() {
            black_box(entry);
            count += 1;
        }
        count
    });
    assert_eq!(entry_count, 3);
    assert_eq!(
        allocations, 0,
        "decode_publish_batch_ref must not allocate once warmed"
    );
}

/// `PublishBatchWriter::push`/`finish_into` allocate nothing once both the
/// writer's own buffers and the caller's output buffer have grown to their
/// steady-state size (S2's allocation-free acceptance criterion).
#[test]
fn publish_batch_writer_allocates_nothing_once_warmed() {
    let mut writer = PublishBatchWriter::new();
    let mut buf = Vec::new();
    // Warm-up: grows `writer`'s internal buffers and `buf` to their
    // steady-state capacity.
    for _ in 0..2 {
        fill_sample_batch(&mut writer, &mut buf);
    }

    let (_, allocations) = allocations_during(|| {
        fill_sample_batch(&mut writer, &mut buf);
    });
    assert_eq!(
        allocations, 0,
        "PublishBatchWriter must not allocate once warmed"
    );
}

/// [`decode_publish_field_ref`] agrees with the owning decode behind
/// `Message::decode` on a standalone `VOSU_OP_PUBLISH_FIELD` payload
/// (`PublishBatchWriter`/`decode_publish_batch_ref`'s roundtrip above
/// only exercises the batch opcode).
#[test]
fn decode_publish_field_ref_matches_owned_decode() {
    let field = PublishField {
        handle: 7,
        ns: "vosu.osu".into(),
        pointer: "/play/pp/current".into(),
        value: PublishValue::F64(123.5),
    };
    let payload = Message::PublishField(field.clone())
        .encode_payload()
        .expect("encode");

    let field_ref = decode_publish_field_ref(&payload).expect("decode_publish_field_ref");
    assert_eq!(field_ref.handle, field.handle);
    assert_eq!(field_ref.ns, field.ns);
    assert_eq!(field_ref.pointer, field.pointer);
    assert!(values_match(&field.value, field_ref.value));

    let decoded =
        Message::decode(vosu_opcode::VOSU_OP_PUBLISH_FIELD, 0, &payload).expect("Message::decode");
    assert_eq!(decoded, Message::PublishField(field));
}

/// `PublishBatchWriter::push_cbor_with` writes the exact bytes its
/// closure appends (through the append-only `CborAppend`, never a
/// separate `Vec` the writer copies from), and both decoders agree on the
/// result: the borrowed path returns them raw, the owning path decodes
/// them back to the original `CborValue`.
#[test]
fn push_cbor_with_roundtrips_through_both_decoders() {
    let value = CborValue::Map(vec![(
        CborValue::Text("judgements".into()),
        CborValue::Integer(300.into()),
    )]);
    let encoded = encode(&value).expect("encode");

    let mut writer = PublishBatchWriter::new();
    writer.begin(9);
    writer.push_cbor_with("vosu.osu", "/result/statistics", |w| {
        w.extend_from_slice(&encoded);
    });
    let mut buf = Vec::new();
    writer.finish_into(&mut buf).expect("finish_into");

    let batch_ref = decode_publish_batch_ref(&buf).expect("decode_publish_batch_ref");
    let entry_ref = batch_ref.entries().next().expect("one entry");
    match entry_ref.value {
        PublishValueRef::Cbor(bytes) => assert_eq!(bytes, encoded.as_slice()),
        other => panic!("expected PublishValueRef::Cbor, got {other:?}"),
    }

    let decoded =
        Message::decode(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, &buf).expect("Message::decode");
    let Message::PublishBatch(owned) = decoded else {
        panic!("expected Message::PublishBatch, got {decoded:?}");
    };
    assert_eq!(owned.entries[0].value, PublishValue::Cbor(value));
}

/// Lays a one-entry `VOSU_OP_PUBLISH_BATCH` payload out through
/// `PublishBatchWriter`, for building a known-good buffer to corrupt.
fn one_entry_batch_bytes(
    handle: u64,
    ns: &str,
    pointer: &str,
    value: PublishValueRef<'_>,
) -> Vec<u8> {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push(ns, pointer, value);
    let mut buf = Vec::new();
    writer.finish_into(&mut buf).expect("finish_into");
    buf
}

/// `decode_publish_batch_ref` and the owning decode behind
/// `Message::decode` share the same layout validation
/// (`crate::validate::validate_publish_batch`) and the same per-string
/// UTF-8 check, so every malformed input one rejects, the other does too
/// — the only documented exception is CBOR-kind payload bytes, which the
/// borrowed path never parses (see `message/flat/publish.rs`'s module
/// doc comment), so this test sticks to corruptions that do not depend on
/// CBOR content.
#[test]
fn both_decoders_reject_the_same_malformed_batches() {
    let good = one_entry_batch_bytes(1, "vosu.osu", "/play/combo", PublishValueRef::U64(1));
    // 16-byte batch header + one 48-byte `vosu_field_publish_header`:
    // `ns`'s trailing bytes start right after the entry array.
    let data_start = 16 + 48;
    assert_eq!(&good[data_start..data_start + 8], b"vosu.osu");

    // Truncated: cuts off the last byte of the trailing `pointer` bytes,
    // so `validate_bytes_range` sees a range past the buffer's end.
    let truncated = good[..good.len() - 1].to_vec();
    // Nonzero `reserved`: offset 28 inside `vosu_field_publish_header`
    // (handle 8 + ns_offset 4 + ns_len 4 + pointer_offset 4 + pointer_len
    // 4 + value_kind 4), so global offset 16 + 28 = 44.
    let mut reserved_nonzero = good.clone();
    reserved_nonzero[44] = 1;
    // Invalid UTF-8: the first byte of `ns`'s trailing bytes.
    let mut bad_utf8 = good.clone();
    bad_utf8[data_start] = 0xFF;

    for (name, bytes) in [
        ("truncated", truncated),
        ("reserved_nonzero", reserved_nonzero),
        ("bad_utf8", bad_utf8),
    ] {
        assert!(
            decode_publish_batch_ref(&bytes).is_err(),
            "{name}: decode_publish_batch_ref should reject this input"
        );
        assert!(
            Message::decode(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, &bytes).is_err(),
            "{name}: the owning decode (via Message::decode) should reject this input"
        );
    }
}

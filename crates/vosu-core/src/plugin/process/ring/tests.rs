use std::collections::VecDeque;
use std::sync::Arc;

use proptest::prelude::*;

use super::super::frame::{
    FrameHeader, FramePolicy, HEAD_BYTES, MAX_PAYLOAD_BYTES, PolicyViolation, encode_head,
};
use super::*;

/// Admits every structurally valid frame; the codec checks still apply.
struct AcceptAll;

impl FramePolicy for AcceptAll {
    fn admit(&mut self, _: &FrameHeader, _: usize) -> Result<(), PolicyViolation> {
        Ok(())
    }
}

/// Refuses everything, recording that it was consulted.
struct RefuseAll(usize);

impl FramePolicy for RefuseAll {
    fn admit(&mut self, header: &FrameHeader, _: usize) -> Result<(), PolicyViolation> {
        self.0 += 1;
        Err(PolicyViolation::Pairing {
            opcode: header.opcode,
        })
    }
}

thread_local! {
    /// One region per test thread, reinitialized per case: allocating 2 MiB
    /// per case would dominate the property tests.
    static REGION: Arc<SharedRegion> = Arc::new(SharedRegion::heap(REGION_BYTES).unwrap());
}

fn ring_at(start: u64) -> (Arc<SharedRegion>, RingProducer, RingConsumer) {
    let region = REGION.with(Arc::clone);
    initialize_at(&region, start).unwrap();
    let producer = RingProducer::attach(region.clone()).unwrap();
    let consumer = RingConsumer::attach(region.clone()).unwrap();
    (region, producer, consumer)
}

fn frame(index: u64, len: usize) -> (FrameHeader, Vec<u8>) {
    let header = FrameHeader {
        opcode: index as u32 ^ 0x5a5a_0001,
        flags: (index as u32).rotate_left(7),
        correlation: index.wrapping_mul(0x9e37_79b9_7f4a_7c15),
    };
    let payload = (0..len)
        .map(|byte| (byte as u64 ^ index) as u8 | 1)
        .collect();
    (header, payload)
}

fn write(producer: &mut RingProducer, header: &FrameHeader, payload: &[u8]) -> bool {
    let head = encode_head(header, payload.len()).unwrap();
    producer.try_write(&head, payload).unwrap()
}

fn read(consumer: &mut RingConsumer, buffer: &mut PayloadBuffer) -> Option<(FrameHeader, Vec<u8>)> {
    consumer
        .try_read(buffer, &mut AcceptAll)
        .unwrap()
        .map(|(header, _)| (header, buffer.bytes().to_vec()))
}

fn publish(region: &SharedRegion, cursor: u64) {
    store(region, WRITE_CURSOR, cursor).unwrap();
}

#[test]
fn frames_wrap_at_every_byte_boundary_of_prefix_header_and_payload() {
    let mut buffer = PayloadBuffer::new();
    let lengths = [0, 1, 3, 7, 8, 9, 16, 17, 33, 40];
    // Data-index wrap alone, and together with the 2^64 cursor wrap.
    for base in [CAPACITY as u64, 0] {
        for back in 0..=(HEAD_BYTES as u64 + 41) {
            for (index, &len) in lengths.iter().enumerate() {
                let start = base.wrapping_sub(back);
                let (_, mut producer, mut consumer) = ring_at(start);
                let (header, payload) = frame(back * 16 + index as u64, len);
                assert!(write(&mut producer, &header, &payload));
                let got = read(&mut consumer, &mut buffer).unwrap();
                assert_eq!(got, (header, payload), "start {start:#x} len {len}");
                assert_eq!(buffer.bytes().as_ptr() as usize % 8, 0, "aligned copy");
                assert_eq!(consumer.available().unwrap(), 0);
                assert_eq!(producer.free().unwrap(), CAPACITY as u64);
            }
        }
    }
}

#[test]
fn empty_ring_takes_the_legal_maximum_and_refuses_what_does_not_fit_without_writing() {
    let (_, mut producer, mut consumer) = ring_at(CAPACITY as u64 - 5);
    let (max_header, max_payload) = frame(1, MAX_PAYLOAD_BYTES);
    assert!(
        write(&mut producer, &max_header, &max_payload),
        "legal maximum fits an empty ring"
    );
    let (small_header, small_payload) = frame(2, 1);
    assert!(write(&mut producer, &small_header, &small_payload));
    let before = consumer.available().unwrap();
    assert!(
        !write(&mut producer, &max_header, &max_payload),
        "second maximum does not fit"
    );
    assert_eq!(consumer.available().unwrap(), before, "nothing published");

    let mut buffer = PayloadBuffer::new();
    assert_eq!(
        read(&mut consumer, &mut buffer).unwrap(),
        (max_header, max_payload.clone())
    );
    let grown = buffer.capacity_bytes();
    assert!((MAX_PAYLOAD_BYTES..=MAX_PAYLOAD_BYTES + 8).contains(&grown));
    assert!(
        write(&mut producer, &max_header, &max_payload),
        "fits once space is released"
    );
    assert_eq!(
        read(&mut consumer, &mut buffer).unwrap(),
        (small_header, small_payload)
    );
    assert_eq!(
        read(&mut consumer, &mut buffer).unwrap(),
        (max_header, max_payload)
    );
    assert_eq!(
        buffer.capacity_bytes(),
        grown,
        "scratch reused, never regrown"
    );
    assert_eq!(read(&mut consumer, &mut buffer), None);
}

#[test]
fn a_frame_written_but_never_published_is_invisible() {
    // A producer that died mid-frame copied bytes but never moved its
    // cursor; the consumer must not see any of it.
    let (region, mut producer, mut consumer) = ring_at(100);
    let (header, payload) = frame(3, 50);
    let head = encode_head(&header, payload.len()).unwrap();
    copy_in(&region, 100, &head).unwrap();
    copy_in(&region, 124, &payload).unwrap();
    let mut buffer = PayloadBuffer::new();
    assert_eq!(read(&mut consumer, &mut buffer), None);
    // The next legal producer simply overwrites it.
    assert!(write(&mut producer, &header, &payload));
    assert_eq!(read(&mut consumer, &mut buffer).unwrap(), (header, payload));
}

#[test]
fn a_cursor_published_inside_a_frame_is_a_violation() {
    for cut in [
        1,
        HEAD_BYTES as u64 - 1,
        HEAD_BYTES as u64,
        HEAD_BYTES as u64 + 49,
    ] {
        let (region, _, mut consumer) = ring_at(7);
        let (header, payload) = frame(4, 50);
        copy_in(&region, 7, &encode_head(&header, payload.len()).unwrap()).unwrap();
        copy_in(&region, 31, &payload).unwrap();
        publish(&region, 7 + cut);
        let mut buffer = PayloadBuffer::new();
        assert!(
            matches!(
                consumer.try_read(&mut buffer, &mut AcceptAll),
                Err(RingError::PartialFrame { .. })
            ),
            "cut {cut}"
        );
    }
}

#[test]
fn forged_write_cursors_are_refused() {
    let mut buffer = PayloadBuffer::new();
    for forged in [
        1000 + CAPACITY as u64 + 1, // more than a ring's worth published
        u64::MAX,                   // before the read cursor
        999,                        // likewise
    ] {
        let (region, _, mut consumer) = ring_at(1000);
        publish(&region, forged);
        assert!(
            matches!(
                consumer.try_read(&mut buffer, &mut AcceptAll),
                Err(RingError::WriteCursor { .. })
            ),
            "{forged}"
        );
    }
    // Unpublishing bytes already observed is refused too.
    let (region, mut producer, mut consumer) = ring_at(0);
    let (header, payload) = frame(5, 10);
    assert!(write(&mut producer, &header, &payload));
    assert_eq!(consumer.available().unwrap(), 34);
    publish(&region, 10);
    assert!(matches!(
        consumer.available(),
        Err(RingError::WriteCursor { .. })
    ));
}

#[test]
fn forged_read_cursors_are_refused() {
    let (region, mut producer, _) = ring_at(500);
    let (header, payload) = frame(6, 10);
    assert!(write(&mut producer, &header, &payload));
    for forged in [535, u64::MAX, 499] {
        store(&region, READ_CURSOR, forged).unwrap();
        assert!(
            matches!(producer.free(), Err(RingError::ReadCursor { .. })),
            "{forged}"
        );
    }
    store(&region, READ_CURSOR, 534).unwrap();
    assert_eq!(producer.free().unwrap(), CAPACITY as u64);
    store(&region, READ_CURSOR, 520).unwrap();
    assert!(
        matches!(producer.free(), Err(RingError::ReadCursor { .. })),
        "moved backwards"
    );
}

#[test]
fn forged_heads_are_refused_before_any_allocation() {
    let (region, _, mut consumer) = ring_at(0);
    let mut head = encode_head(&frame(7, 0).0, 0).unwrap();
    // Claims the maximum-plus-one payload; total_len agrees.
    let len = MAX_PAYLOAD_BYTES as u32 + 1;
    head[0..4].copy_from_slice(&(len + 20).to_le_bytes());
    head[20..24].copy_from_slice(&len.to_le_bytes());
    copy_in(&region, 0, &head).unwrap();
    publish(&region, CAPACITY as u64);
    let mut buffer = PayloadBuffer::new();
    assert!(matches!(
        consumer.try_read(&mut buffer, &mut AcceptAll),
        Err(RingError::Frame(FrameError::Oversize { .. }))
    ));
    assert_eq!(
        buffer.capacity_bytes(),
        0,
        "nothing allocated for a forged length"
    );

    let (region, _, mut consumer) = ring_at(0);
    head[0..4].copy_from_slice(&21_u32.to_le_bytes());
    head[20..24].copy_from_slice(&0_u32.to_le_bytes());
    copy_in(&region, 0, &head).unwrap();
    publish(&region, 64);
    assert!(matches!(
        consumer.try_read(&mut buffer, &mut AcceptAll),
        Err(RingError::Frame(FrameError::LengthMismatch { .. }))
    ));
}

#[test]
fn policy_refusal_stops_the_frame_before_its_payload_is_copied() {
    let (_, mut producer, mut consumer) = ring_at(0);
    let (header, payload) = frame(8, 4096);
    assert!(write(&mut producer, &header, &payload));
    let mut buffer = PayloadBuffer::new();
    let mut policy = RefuseAll(0);
    assert!(matches!(
        consumer.try_read(&mut buffer, &mut policy),
        Err(RingError::Policy(PolicyViolation::Pairing { .. }))
    ));
    assert_eq!(policy.0, 1);
    assert_eq!(buffer.capacity_bytes(), 0);
}

#[test]
fn attach_validates_size_capacity_magic_and_cursors() {
    let small = Arc::new(SharedRegion::heap(4096).unwrap());
    assert!(matches!(
        initialize(&small),
        Err(RingError::RegionSize { actual: 4096 })
    ));
    assert!(matches!(
        RingConsumer::attach(small),
        Err(RingError::RegionSize { .. })
    ));

    let (region, _, _) = ring_at(0);
    store(&region, CAPACITY_WORD, CAPACITY as u64 * 2).unwrap();
    assert!(matches!(
        RingProducer::attach(region.clone()),
        Err(RingError::Capacity(_))
    ));
    let (region, _, _) = ring_at(0);
    store(&region, MAGIC_WORD, 1).unwrap();
    assert!(matches!(
        RingConsumer::attach(region.clone()),
        Err(RingError::Magic(1))
    ));
    let (region, _, _) = ring_at(0);
    publish(&region, CAPACITY as u64 + 1);
    assert!(matches!(
        RingConsumer::attach(region.clone()),
        Err(RingError::AttachCursors { .. })
    ));
}

#[test]
fn space_and_parked_flags_cross_between_ends() {
    let (_, mut producer, mut consumer) = ring_at(0);
    assert!(!producer.consumer_parked().unwrap());
    consumer.set_parked(true).unwrap();
    assert!(producer.consumer_parked().unwrap());
    consumer.set_parked(false).unwrap();

    let (header, payload) = frame(9, 3);
    assert!(write(&mut producer, &header, &payload));
    assert!(write(&mut producer, &header, &payload));
    producer.want_space().unwrap();
    let mut buffer = PayloadBuffer::new();
    let (_, wanted) = consumer
        .try_read(&mut buffer, &mut AcceptAll)
        .unwrap()
        .unwrap();
    assert!(wanted, "first release after the request reports it");
    let (_, wanted) = consumer
        .try_read(&mut buffer, &mut AcceptAll)
        .unwrap()
        .unwrap();
    assert!(!wanted, "the request is consumed once");
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]

    /// Any interleaving of writes and reads from any starting cursor
    /// delivers exactly the frames that fit, in order, byte for byte, and
    /// refuses only frames that do not fit the current free space.
    #[test]
    fn prop_ring_round_trips_interleaved_frames(
        start in prop_oneof![any::<u64>(), (0..64_u64).prop_map(|back| (CAPACITY as u64).wrapping_sub(back))],
        steps in proptest::collection::vec((any::<bool>(), 0..70_000_usize), 1..80),
    ) {
        let (_, mut producer, mut consumer) = ring_at(start);
        let mut buffer = PayloadBuffer::new();
        let mut expected = VecDeque::new();
        let mut queued = 0_usize;
        for (index, (is_write, len)) in steps.into_iter().enumerate() {
            if is_write {
                let (header, payload) = frame(index as u64, len);
                let fits = queued + HEAD_BYTES + len <= CAPACITY;
                prop_assert_eq!(write(&mut producer, &header, &payload), fits);
                if fits {
                    queued += HEAD_BYTES + len;
                    expected.push_back((header, payload));
                }
            } else {
                let got = read(&mut consumer, &mut buffer);
                let want = expected.pop_front();
                if let Some((_, payload)) = &want {
                    queued -= HEAD_BYTES + payload.len();
                }
                prop_assert_eq!(got, want);
            }
        }
        while let Some(want) = expected.pop_front() {
            prop_assert_eq!(read(&mut consumer, &mut buffer), Some(want));
        }
        prop_assert_eq!(read(&mut consumer, &mut buffer), None);
    }

    /// Whatever a hostile producer writes into the data area and cursor,
    /// the consumer never panics, never accepts more than was published,
    /// and never allocates past the frame cap.
    #[test]
    fn prop_hostile_producer_bytes_never_panic_or_overallocate(
        start in any::<u64>(),
        garbage in proptest::collection::vec((0..CAPACITY, proptest::collection::vec(any::<u8>(), 1..64)), 0..16),
        heads in proptest::collection::vec((0..CAPACITY, any::<[u8; HEAD_BYTES]>()), 0..4),
        advance in prop_oneof![any::<u64>(), 0..=(CAPACITY as u64 + 8)],
    ) {
        let (region, _, mut consumer) = ring_at(start);
        for (at, bytes) in garbage {
            copy_in(&region, at as u64, &bytes).unwrap();
        }
        for (at, head) in heads {
            copy_in(&region, at as u64, &head).unwrap();
        }
        publish(&region, start.wrapping_add(advance));
        let mut buffer = PayloadBuffer::new();
        let mut consumed = 0_u64;
        for _ in 0..64 {
            match consumer.try_read(&mut buffer, &mut AcceptAll) {
                Ok(Some(_)) => {
                    consumed += (HEAD_BYTES + buffer.bytes().len()) as u64;
                    prop_assert!(consumed <= advance);
                }
                Ok(None) | Err(_) => break,
            }
        }
        prop_assert!(buffer.capacity_bytes() <= MAX_PAYLOAD_BYTES + 8);
    }
}

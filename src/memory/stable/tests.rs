use super::readers::{MAX_TOURNEY_CHAT_CHANNELS, MAX_TOURNEY_CHAT_MESSAGES};
use super::*;

use crate::process::{MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};
use arc_swap::ArcSwap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct SparseMemory {
    base: usize,
    bytes: Vec<u8>,
    reads: AtomicUsize,
    batches: AtomicUsize,
}
impl SparseMemory {
    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let start = address
            .checked_sub(self.base)
            .ok_or_else(|| MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside fixture".into(),
            })?;
        let end = start
            .checked_add(dst.len())
            .ok_or(MemoryError::AddressOverflow {
                base: start,
                offset: dst.len(),
            })?;
        let source = self
            .bytes
            .get(start..end)
            .ok_or_else(|| MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside fixture".into(),
            })?;
        dst.copy_from_slice(source);
        Ok(())
    }
}
impl ProcessMemory for SparseMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.copy(address, dst)
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.batches.fetch_add(1, Ordering::Relaxed);
        for read in reads {
            self.copy(read.address, read.dst)?;
        }
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: self.base,
            size: self.bytes.len(),
        }])
    }
}
struct MutableMemory {
    bytes: ArcSwap<Vec<u8>>,
    batches: AtomicUsize,
}
impl ProcessMemory for MutableMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let bytes = self.bytes.load();
        let end = address
            .checked_add(dst.len())
            .ok_or(MemoryError::AddressOverflow {
                base: address,
                offset: dst.len(),
            })?;
        let source = bytes.get(address..end).ok_or_else(|| MemoryError::Read {
            address,
            len: dst.len(),
            message: "outside fixture".into(),
        })?;
        dst.copy_from_slice(source);
        Ok(())
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batches.fetch_add(1, Ordering::Relaxed);
        for read in reads {
            self.read_exact(read.address, read.dst)?
        }
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.bytes.load().len(),
        }])
    }
}
fn put_u32(bytes: &mut [u8], address: usize, value: u32) {
    bytes[address..address + 4].copy_from_slice(&value.to_le_bytes())
}
fn put_i32(bytes: &mut [u8], address: usize, value: i32) {
    bytes[address..address + 4].copy_from_slice(&value.to_le_bytes())
}
fn put_f32(bytes: &mut [u8], address: usize, value: f32) {
    bytes[address..address + 4].copy_from_slice(&value.to_le_bytes())
}
fn put_string(bytes: &mut [u8], address: usize, value: &str) {
    put_i32(bytes, address + 4, value.encode_utf16().count() as i32);
    for (index, unit) in value.encode_utf16().enumerate() {
        bytes[address + 8 + 2 * index..address + 10 + 2 * index]
            .copy_from_slice(&unit.to_le_bytes())
    }
}
fn handle(base: usize, bytes: Vec<u8>) -> ProcessHandle {
    ProcessHandle::new(
        Arc::new(SparseMemory {
            base,
            bytes,
            reads: AtomicUsize::new(0),
            batches: AtomicUsize::new(0),
        }),
        ProcessBitness::X86,
    )
}
pub(super) fn fixture_patterns() -> StablePatterns {
    StablePatterns {
        base_addr: 0x200,
        play_time_addr: 0x300,
        chat_checker_ptr: 0x400,
        skin_data_addr: 0x500,
        settings_class_addr: 0x600,
        configuration_addr: 0x700,
        bindings_addr: 0x800,
        rulesets_addr: 0x900,
        can_run_slowly_addr: 0xa00,
        status_ptr: 0xb00,
        menu_mods_ptr: 0xc00,
        get_audio_length_ptr: 0xd00,
        user_profile_ptr: 0xe00,
        raw_login_status_ptr: 0xf00,
        spectating_user_ptr: 0x1000,
        game_time_ptr: 0x1100,
    }
}
fn key_overlay_reader(length: i32) -> StableMemory {
    key_overlay_reader_with_memory(length).0
}

fn key_overlay_reader_with_memory(length: i32) -> (StableMemory, Arc<SparseMemory>) {
    let mut bytes = vec![0; 0x3000];
    let ruleset_anchor = 0x1200usize;
    let ruleset = 0x1300usize;
    let overlay = 0x1400usize;
    let nested = 0x1500usize;
    let array = 0x1600usize;
    put_u32(&mut bytes, 0x900 - 0xb, ruleset_anchor as u32);
    put_u32(&mut bytes, ruleset_anchor + 4, ruleset as u32);
    put_u32(&mut bytes, ruleset + 0xac, overlay as u32);
    put_u32(&mut bytes, overlay + 0x10, nested as u32);
    put_u32(&mut bytes, nested + 4, array as u32);
    put_i32(&mut bytes, array + 4, length);
    for index in 0..length.clamp(0, 18) as usize {
        let button = 0x1800 + index * 0x40;
        put_u32(&mut bytes, array + 8 + index * 4, button as u32);
        put_i32(&mut bytes, button + 0x14, index as i32 + 10);
        bytes[button + 0x1c] = u8::from(index % 2 == 0);
    }
    let memory = Arc::new(SparseMemory {
        base: 0,
        bytes,
        reads: AtomicUsize::new(0),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    (StableMemory::new(process, fixture_patterns()), memory)
}

fn key_overlay_buttons(reader: &mut StableMemory, mode: i64) -> KeyOverlay {
    match reader.key_overlay(mode).unwrap() {
        ReadStatus::Ready(buttons) => buttons,
        other => panic!("{other:?}"),
    }
}

#[test]
fn ranked_status_keeps_stable_integer_exact() {
    for raw in [i32::MIN, -2, 0, 7, i32::MAX] {
        let status = stable_ranked_status(i64::from(raw));
        assert!(status.is_finite());
        assert_eq!(status, f64::from(raw));
    }
}
#[test]
fn all_sixteen_signatures_resolve_in_order_with_signed_offsets() {
    let mut bytes = vec![];
    let mut expected = vec![];
    for (signature, offset) in PATTERNS {
        let parsed = Pattern::parse(signature, false).unwrap();
        let start = 0x4000 + bytes.len();
        expected.push(shifted(start, offset).unwrap());
        for (&byte, &mask) in parsed.bytes().iter().zip(parsed.mask()) {
            bytes.push(if mask == 0 { 0xee } else { byte });
        }
        bytes.extend_from_slice(&[0u8; 31]);
    }
    let resolved = match StablePatterns::resolve(&handle(0x4000, bytes)).unwrap() {
        ReadStatus::Ready(v) => v,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        [
            resolved.base_addr,
            resolved.play_time_addr,
            resolved.chat_checker_ptr,
            resolved.skin_data_addr,
            resolved.settings_class_addr,
            resolved.configuration_addr,
            resolved.bindings_addr,
            resolved.rulesets_addr,
            resolved.can_run_slowly_addr,
            resolved.status_ptr,
            resolved.menu_mods_ptr,
            resolved.get_audio_length_ptr,
            resolved.user_profile_ptr,
            resolved.raw_login_status_ptr,
            resolved.spectating_user_ptr,
            resolved.game_time_ptr
        ],
        expected.as_slice()
    );
}
#[test]
fn stable_hit_errors_are_appended_to_all_and_press() {
    let mut bytes = vec![0; 0x3000];
    let ruleset_anchor = 0x1200usize;
    let ruleset = 0x1300usize;
    let gameplay = 0x1400usize;
    let score = 0x1500usize;
    let list = 0x1600usize;
    let items = 0x1700usize;
    put_u32(&mut bytes, 0x900 - 0xb, ruleset_anchor as u32);
    put_u32(&mut bytes, ruleset_anchor + 4, ruleset as u32);
    put_u32(&mut bytes, ruleset + 0x64, gameplay as u32);
    put_u32(&mut bytes, gameplay + 0x38, score as u32);
    put_u32(&mut bytes, score + 0x38, list as u32);
    put_u32(&mut bytes, list + 4, items as u32);
    put_i32(&mut bytes, list + 0xc, 2);
    put_i32(&mut bytes, items + LEADER_START, -4);
    put_i32(&mut bytes, items + LEADER_START + 4, 7);
    let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());

    let ReadStatus::Ready(suffix) = reader.hit_errors(0).unwrap() else {
        panic!("stable hit errors ready");
    };

    assert_eq!(suffix.index, 2);
    assert_eq!(suffix.errors.all, [-4.0, 7.0]);
    assert_eq!(suffix.errors.press, [-4.0, 7.0]);
    assert!(suffix.errors.release.is_empty());
    assert!(suffix.release_corrections.is_empty());
}

type ListGrowth = Box<dyn FnOnce(&mut Vec<u8>) + Send>;

/// The game grows its hit-error list (moving it to a larger array) right after the
/// reader's first read of the list header, as happened live at the ninth error.
struct GrowingListMemory {
    bytes: parking_lot::Mutex<Vec<u8>>,
    header: [usize; 2],
    grow: parking_lot::Mutex<Option<ListGrowth>>,
}
impl ProcessMemory for GrowingListMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let mut bytes = self.bytes.lock();
        let source = bytes
            .get(address..address + dst.len())
            .ok_or_else(|| MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside fixture".into(),
            })?;
        dst.copy_from_slice(source);
        if self.header.contains(&address)
            && let Some(grow) = self.grow.lock().take()
        {
            grow(&mut bytes);
        }
        Ok(())
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.bytes.lock().len(),
        }])
    }
}

#[test]
fn stable_hit_errors_never_pair_a_grown_size_with_the_abandoned_array() {
    let mut bytes = vec![0; 0x3000];
    let (ruleset_anchor, ruleset, gameplay, score, list) = (
        0x1200usize,
        0x1300usize,
        0x1400usize,
        0x1500usize,
        0x1600usize,
    );
    let (old_items, new_items) = (0x1700usize, 0x1800usize);
    put_u32(&mut bytes, 0x900 - 0xb, ruleset_anchor as u32);
    put_u32(&mut bytes, ruleset_anchor + 4, ruleset as u32);
    put_u32(&mut bytes, ruleset + 0x64, gameplay as u32);
    put_u32(&mut bytes, gameplay + 0x38, score as u32);
    put_u32(&mut bytes, score + 0x38, list as u32);
    put_u32(&mut bytes, list + 4, old_items as u32);
    put_i32(&mut bytes, list + 0xc, 8);
    let values = [-8, 5, -3, 12, 0, -1, 9, -14, -79];
    for (index, value) in values[..8].iter().enumerate() {
        put_i32(&mut bytes, old_items + LEADER_START + 4 * index, *value);
    }
    // .NET List growth: copy into the larger array, publish it, then the new size.
    let grow = Box::new(move |bytes: &mut Vec<u8>| {
        for (index, value) in values.iter().enumerate() {
            put_i32(bytes, new_items + LEADER_START + 4 * index, *value);
        }
        put_u32(bytes, list + 4, new_items as u32);
        put_i32(bytes, list + 0xc, 9);
    });
    let memory = GrowingListMemory {
        bytes: parking_lot::Mutex::new(bytes),
        header: [list + 4, list + 0xc],
        grow: parking_lot::Mutex::new(Some(grow)),
    };
    let mut reader = StableMemory::new(
        ProcessHandle::new(Arc::new(memory), ProcessBitness::X86),
        fixture_patterns(),
    );

    let mut observed = Vec::new();
    let mut index = 0;
    for _ in 0..2 {
        let ReadStatus::Ready(suffix) = reader.hit_errors(index).unwrap() else {
            panic!("stable hit errors ready");
        };
        observed.extend(suffix.errors.all);
        index = suffix.index;
    }
    assert_eq!(observed, values.map(f64::from));
}

#[test]
fn stable_hit_errors_catches_up_in_bounded_captures_and_rewinds() {
    const COUNT: i32 = 5_000;
    let mut bytes = vec![0; 0x8000];
    let ruleset_anchor = 0x1200usize;
    let ruleset = 0x1300usize;
    let gameplay = 0x1400usize;
    let score = 0x1500usize;
    let list = 0x1600usize;
    let items = 0x2000usize;
    put_u32(&mut bytes, 0x900 - 0xb, ruleset_anchor as u32);
    put_u32(&mut bytes, ruleset_anchor + 4, ruleset as u32);
    put_u32(&mut bytes, ruleset + 0x64, gameplay as u32);
    put_u32(&mut bytes, gameplay + 0x38, score as u32);
    put_u32(&mut bytes, score + 0x38, list as u32);
    put_u32(&mut bytes, list + 4, items as u32);
    put_i32(&mut bytes, list + 0xc, COUNT);
    for index in 0..COUNT as usize {
        put_i32(&mut bytes, items + LEADER_START + index * 4, index as i32);
    }
    let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());

    let ReadStatus::Ready(first) = reader.hit_errors(0).unwrap() else {
        panic!("stable hit errors ready");
    };
    assert_eq!(first.index, 4_096);
    assert_eq!(first.errors.all.len(), 4_096);
    assert_eq!(first.errors.all[0], 0.0);
    assert_eq!(first.errors.all[4_095], 4_095.0);

    let ReadStatus::Ready(second) = reader.hit_errors(first.index).unwrap() else {
        panic!("stable hit errors ready");
    };
    assert_eq!(second.index, COUNT as i64);
    assert_eq!(
        second.errors.all,
        (4_096..COUNT).map(f64::from).collect::<Vec<_>>()
    );

    let ReadStatus::Ready(rewound) = reader.hit_errors(0).unwrap() else {
        panic!("stable hit errors ready");
    };
    assert_eq!(rewound.index, 4_096);
    assert_eq!(rewound.errors.all[0], 0.0);
}

#[test]
fn stable_mania_replay_frames_remain_cached_across_hit_error_acknowledgement() {
    let mut bytes = vec![0; 0x4000];
    put_u32(&mut bytes, 0x305, 0x350);
    put_i32(&mut bytes, 0x350, 1000);
    let ruleset_anchor = 0x1200usize;
    let ruleset = 0x1300usize;
    let gameplay = 0x1400usize;
    let score = 0x1500usize;
    let error_list = 0x1600usize;
    let error_items = 0x1700usize;
    let replay_list = 0x1800usize;
    let replay_items = 0x1900usize;
    let frames = [0x1a00usize, 0x1b00, 0x1c00];
    put_u32(&mut bytes, 0x900 - 0xb, ruleset_anchor as u32);
    put_u32(&mut bytes, ruleset_anchor + 4, ruleset as u32);
    put_u32(&mut bytes, ruleset + 0x64, gameplay as u32);
    put_u32(&mut bytes, gameplay + 0x38, score as u32);
    put_u32(&mut bytes, score + 0x38, error_list as u32);
    put_u32(&mut bytes, error_list + 4, error_items as u32);
    put_i32(&mut bytes, error_list + 0xc, 1);
    put_i32(&mut bytes, error_items + LEADER_START, 3);
    put_u32(&mut bytes, score + 0x34, replay_list as u32);
    put_u32(&mut bytes, replay_list + 4, replay_items as u32);
    put_i32(&mut bytes, replay_list + 0xc, frames.len() as i32);
    for (index, frame) in frames.into_iter().enumerate() {
        put_u32(
            &mut bytes,
            replay_items + LEADER_START + 4 * index,
            frame as u32,
        );
    }
    for (frame, keys, y, time) in [
        (frames[0], 256.0, -500.0, -12_345),
        (frames[1], 1.0, 0.0, 103),
        (frames[2], 0.0, 0.0, 110),
    ] {
        put_f32(&mut bytes, frame + 4, keys);
        put_f32(&mut bytes, frame + 8, y);
        put_i32(&mut bytes, frame + 0x10, time);
    }
    let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());
    reader.synchronize_precise(ReaderSynchronization::default(), 3);

    let ReadStatus::Ready(first) = reader.hit_errors(0).unwrap() else {
        panic!("stable mania hit errors ready");
    };
    let batch = first.stable_replay_frames.unwrap();
    assert_eq!(batch.start_index, 0);
    assert_eq!(batch.index, 3);
    assert_eq!(
        batch.frames.iter().copied().collect::<Vec<_>>(),
        [
            StableReplayFrame {
                time: -12_345,
                x: 256.0,
                buttons: 0,
                buttons_raw: 0,
                y: -500.0,
            },
            StableReplayFrame {
                time: 103,
                x: 1.0,
                buttons: 0,
                buttons_raw: 0,
                y: 0.0,
            },
            StableReplayFrame {
                time: 110,
                x: 0.0,
                buttons: 0,
                buttons_raw: 0,
                y: 0.0,
            },
        ]
    );

    let ReadStatus::Ready(unacknowledged) = reader.hit_errors(0).unwrap() else {
        panic!("unacknowledged stable mania hit errors ready");
    };
    let unacknowledged_batch = unacknowledged.stable_replay_frames.unwrap();
    assert!(Arc::ptr_eq(&unacknowledged_batch.frames, &batch.frames));
    assert_eq!(unacknowledged_batch, batch);

    let ReadStatus::Ready(acknowledged) = reader.hit_errors(1).unwrap() else {
        panic!("acknowledged stable mania hit errors ready");
    };
    assert_eq!(acknowledged.stable_replay_frames.unwrap(), batch);
}

// Count backend operations, not ReadRequest entries: native backends implement
// read_many as one scatter/gather operation for these small fixtures.
struct CountingReplayMemory {
    memory: MutableMemory,
    reads: AtomicUsize,
}

impl ProcessMemory for CountingReplayMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.memory.read_exact(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.memory.read_many(reads)
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.memory.readable_regions()
    }
}

fn counting_replay(
    total: usize,
    size: usize,
    time: i32,
) -> (Arc<CountingReplayMemory>, StableMemory) {
    let items = 0x2000;
    let frame_base = items + total * 4 + 16;
    let mut bytes = vec![0; frame_base + total * 20];
    put_u32(&mut bytes, 0x305, 0x350);
    put_i32(&mut bytes, 0x350, time);
    put_u32(&mut bytes, 0x1534, 0x1800);
    put_u32(&mut bytes, 0x1804, items as u32);
    put_i32(&mut bytes, 0x180c, size as i32);
    for index in 0..total {
        let frame = frame_base + index * 20;
        put_u32(&mut bytes, items + LEADER_START + index * 4, frame as u32);
        put_f32(&mut bytes, frame + 4, (index % 16) as f32);
        put_i32(&mut bytes, frame + 16, index as i32 * 10);
    }
    let memory = Arc::new(CountingReplayMemory {
        memory: MutableMemory {
            bytes: ArcSwap::from_pointee(bytes),
            batches: AtomicUsize::new(0),
        },
        reads: AtomicUsize::new(0),
    });
    let mut reader = StableMemory::new(
        ProcessHandle::new(memory.clone(), ProcessBitness::X86),
        fixture_patterns(),
    );
    reader.play_time_container = 0x350;
    (memory, reader)
}

// Read-only reference for the old prefix-positioning algorithm. It performs
// the same header/clock, binary-search and bounded suffix reads as the old
// reader, without duplicating its frame decoding or retained-history logic.
fn legacy_replay_reads(reader: &mut StableMemory, index: i64) -> i64 {
    let process = &reader.process;
    let list = process.read_u32(0x1534).unwrap() as usize;
    let size = process.read_i32(list + 12).unwrap() as i64;
    let items = process.read_u32(list + 4).unwrap() as usize;
    let time = reader.play_time_value().unwrap();
    let mut low = 0;
    let mut high = size;
    while low < high {
        let middle = low + (high - low) / 2;
        let frame = reader
            .process
            .read_u32(items + LEADER_START + 4 * middle as usize)
            .unwrap();
        if reader.process.read_i32(frame as usize + 16).unwrap() as i64 <= time {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let start = index.max(low - crate::model::memory::StableReplayFrames::LIMIT as i64);
    let count = (low - start) as usize;
    if count > 0 {
        let mut pointers = vec![0; count * 4];
        reader
            .process
            .read_exact(items + LEADER_START + 4 * start as usize, &mut pointers)
            .unwrap();
        let mut frames = vec![[0; 16]; count];
        let mut requests = pointers
            .chunks_exact(4)
            .zip(frames.iter_mut())
            .map(|(pointer, frame)| {
                ReadRequest::new(
                    u32::from_le_bytes(pointer.try_into().unwrap()) as usize + 4,
                    frame,
                )
            })
            .collect::<Vec<_>>();
        reader.process.read_many(&mut requests).unwrap();
    }
    low
}

#[test]
fn stable_live_replay_appends_need_only_six_backend_reads() {
    let (memory, mut reader) = counting_replay(16_384, 4_096, i32::MAX);
    let first = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
    assert_eq!(first.index, 4_096);
    let mut size = 4_096;
    for appended in [1, 2, 8, 32, 128, 1_024] {
        let previous = size;
        size += appended;
        let mut next = (**memory.memory.bytes.load()).clone();
        put_i32(&mut next, 0x180c, size);
        memory.memory.bytes.store(Arc::new(next));
        memory.reads.store(0, Ordering::Relaxed);
        assert_eq!(
            legacy_replay_reads(&mut reader, previous as i64),
            size as i64
        );
        let before = memory.reads.swap(0, Ordering::Relaxed);
        let batch = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
        let after = memory.reads.swap(0, Ordering::Relaxed);
        // Three ordered header reads, one cached clock batch, one pointer
        // slice and one frame scatter/gather, independent of append count.
        assert_eq!(after, 6);
        assert!(before >= 30);
        assert_eq!(
            batch.frames_since(previous as i64).unwrap().count(),
            appended as usize
        );
        eprintln!("stable live N={appended}: legacy={before}, forward={after}");
    }
    let before = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
    memory.reads.store(0, Ordering::Relaxed);
    let after = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
    assert_eq!(memory.reads.load(Ordering::Relaxed), 4);
    assert!(Arc::ptr_eq(&before.frames, &after.frames));
}

#[test]
fn stable_preloaded_replay_walks_forward_without_repeated_searches() {
    let (memory, mut reader) = counting_replay(30_000, 30_000, 100_000);
    let first = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
    assert_eq!((first.start_index, first.index), (1_809, 10_001));
    assert_eq!(first.frames.len(), 8_192);
    for time in (100_010..=101_000).step_by(10) {
        let previous = reader.replay_frame_index;
        let mut next = (**memory.memory.bytes.load()).clone();
        put_i32(&mut next, 0x350, time);
        memory.memory.bytes.store(Arc::new(next));
        memory.reads.store(0, Ordering::Relaxed);
        assert_eq!(
            legacy_replay_reads(&mut reader, previous),
            time as i64 / 10 + 1
        );
        let before = memory.reads.swap(0, Ordering::Relaxed);
        let batch = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
        let after = memory.reads.swap(0, Ordering::Relaxed);
        assert!(
            after == 4 || after == 6,
            "no per-step native search: {after}"
        );
        assert!(before >= 34);
        assert_eq!(batch.index, time as i64 / 10 + 1);
        assert_eq!(batch.newest().unwrap().time, time as i64);
        // Retention evicts whole shared chunks, so the bound is not an
        // assertion that every rolling batch is exactly LIMIT frames long.
        assert!(batch.frames.len() <= 8_192);
        assert_eq!(batch.frames_since(previous).unwrap().count(), 1);
        if time == 100_010 || time == 100_020 {
            eprintln!("stable preloaded t={time}: legacy={before}, forward={after}");
        }
    }
    // A real rewind must discard both exposed history and prefetched futures.
    let mut next = (**memory.memory.bytes.load()).clone();
    put_i32(&mut next, 0x350, 500);
    memory.memory.bytes.store(Arc::new(next));
    let rewound = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
    assert_eq!((rewound.start_index, rewound.index), (0, 51));
}

#[test]
fn stable_replay_reads_size_before_a_relocated_pointer_array() {
    let (fixture, _) = counting_replay(9, 8, i32::MAX);
    let mut bytes = (**fixture.memory.bytes.load()).clone();
    bytes.resize(bytes.len() + 64, 0);
    // Place the abandoned array at the very end: pairing it with size 9
    // would read outside the fixture, rather than accidentally decoding zero.
    let old_items = bytes.len() - LEADER_START - 8 * 4;
    let old_pointers = bytes[0x2000 + LEADER_START..0x2000 + LEADER_START + 8 * 4].to_vec();
    bytes[old_items + LEADER_START..].copy_from_slice(&old_pointers);
    put_u32(&mut bytes, 0x1804, old_items as u32);
    let memory = Arc::new(GrowingListMemory {
        bytes: parking_lot::Mutex::new(bytes),
        header: [0x1804, 0x180c],
        grow: parking_lot::Mutex::new(Some(Box::new(|bytes| {
            put_u32(bytes, 0x1804, 0x2000);
            put_i32(bytes, 0x180c, 9);
        }))),
    });
    let mut reader = StableMemory::new(
        ProcessHandle::new(memory, ProcessBitness::X86),
        fixture_patterns(),
    );
    assert_eq!(
        reader.replay_frame_batch(0x1500, 0).unwrap().unwrap().index,
        8
    );
    assert_eq!(
        reader.replay_frame_batch(0x1500, 0).unwrap().unwrap().index,
        9
    );
}

#[test]
fn stable_mania_retries_a_repaired_prefetched_frame_only_when_due() {
    let (memory, mut reader) = counting_replay(3, 3, 0);
    reader.gameplay_mode = 3;
    let mut next = (**memory.memory.bytes.load()).clone();
    let frame = u32::from_le_bytes(
        next[0x2000 + LEADER_START + 4..0x2000 + LEADER_START + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    put_f32(&mut next, frame + 4, f32::NAN);
    memory.memory.bytes.store(Arc::new(next));
    assert_eq!(
        reader.replay_frame_batch(0x1500, 0).unwrap().unwrap().index,
        1
    );
    let mut next = (**memory.memory.bytes.load()).clone();
    put_i32(&mut next, 0x350, 10);
    memory.memory.bytes.store(Arc::new(next));
    assert!(matches!(
        reader.replay_frame_batch(0x1500, 0),
        Err(MemoryError::Corrupt { .. })
    ));
    assert_eq!(reader.replay_frame_index, 1);
    let mut next = (**memory.memory.bytes.load()).clone();
    put_f32(&mut next, frame + 4, 1.0);
    memory.memory.bytes.store(Arc::new(next));
    assert_eq!(
        reader.replay_frame_batch(0x1500, 0).unwrap().unwrap().index,
        2
    );
}

#[test]
fn stable_degraded_replay_repositions_without_reusing_stale_lookahead() {
    for seek_time in [500, 250_000] {
        let (memory, mut reader) = counting_replay(30_000, 30_000, 100_000);
        assert_eq!(
            reader.replay_frame_batch(0x1500, 0).unwrap().unwrap().index,
            10_001
        );
        let mut next = (**memory.memory.bytes.load()).clone();
        let pointer = 0x2000 + LEADER_START + 10_001 * 4;
        let frame = u32::from_le_bytes(next[pointer..pointer + 4].try_into().unwrap()) as usize;
        put_f32(&mut next, frame + 4, f32::NAN);
        put_i32(&mut next, 0x350, 100_010);
        memory.memory.bytes.store(Arc::new(next));
        assert!(reader.replay_frame_batch(0x1500, 0).unwrap().is_none());
        assert_eq!(reader.replay_frame_index, 10_002);
        assert!(!reader.replay_pending.is_empty());
        let mut next = (**memory.memory.bytes.load()).clone();
        put_i32(&mut next, 0x350, seek_time);
        memory.memory.bytes.store(Arc::new(next));
        let batch = reader.replay_frame_batch(0x1500, 0).unwrap().unwrap();
        assert_eq!(batch.index, seek_time as i64 / 10 + 1);
        assert_eq!(batch.newest().unwrap().time, seek_time as i64);
        assert_eq!(
            batch.frames.iter().next().unwrap().time,
            batch.start_index * 10
        );
    }
}

#[test]
fn stable_replay_prefix_survives_a_play_clock_step_back() {
    let mut bytes = vec![0; 0x4000];
    put_u32(&mut bytes, 0x305, 0x350);
    put_i32(&mut bytes, 0x350, 1000);
    let (ruleset_anchor, ruleset, gameplay, score) = (0x1200usize, 0x1300, 0x1400, 0x1500);
    let (error_list, error_items) = (0x1600usize, 0x1700usize);
    let (replay_list, replay_items) = (0x1800usize, 0x1900usize);
    let frames = [0x1a00usize, 0x1b00, 0x1c00];
    put_u32(&mut bytes, 0x900 - 0xb, ruleset_anchor as u32);
    put_u32(&mut bytes, ruleset_anchor + 4, ruleset as u32);
    put_u32(&mut bytes, ruleset + 0x64, gameplay as u32);
    put_u32(&mut bytes, gameplay + 0x38, score as u32);
    put_u32(&mut bytes, score + 0x38, error_list as u32);
    put_u32(&mut bytes, error_list + 4, error_items as u32);
    put_u32(&mut bytes, score + 0x34, replay_list as u32);
    put_u32(&mut bytes, replay_list + 4, replay_items as u32);
    put_i32(&mut bytes, replay_list + 0xc, frames.len() as i32);
    for (index, (frame, keys, time)) in [
        (frames[0], 0.0, 100),
        (frames[1], 1.0, 200),
        (frames[2], 0.0, 300),
    ]
    .into_iter()
    .enumerate()
    {
        put_u32(
            &mut bytes,
            replay_items + LEADER_START + 4 * index,
            frame as u32,
        );
        put_f32(&mut bytes, frame + 4, keys);
        put_i32(&mut bytes, frame + 0x10, time);
    }
    let memory = Arc::new(MutableMemory {
        bytes: ArcSwap::from_pointee(bytes),
        batches: AtomicUsize::new(0),
    });
    let mut reader = StableMemory::new(
        ProcessHandle::new(memory.clone(), ProcessBitness::X86),
        fixture_patterns(),
    );
    reader.synchronize_precise(ReaderSynchronization::default(), 3);
    let batch = |reader: &mut StableMemory| {
        let ReadStatus::Ready(errors) = reader.hit_errors(0).unwrap() else {
            panic!("stable hit errors ready");
        };
        errors.stable_replay_frames.unwrap()
    };
    let before = batch(&mut reader);
    assert_eq!((before.start_index, before.index), (0, 3));

    // The play clock reads 5 ms behind the newest exposed frame.
    let mut next = (**memory.bytes.load()).clone();
    put_i32(&mut next, 0x350, 295);
    memory.bytes.store(Arc::new(next));
    assert_eq!(batch(&mut reader), before);
}

#[test]
fn stable_replay_newest_skips_only_seed_sentinels() {
    let seed = StableReplayFrame {
        time: -12_345,
        x: 256.0,
        y: -500.0,
        buttons: 0,
        buttons_raw: 0,
    };
    let cursor = StableReplayFrame {
        time: 100,
        x: 20.0,
        ..seed
    };
    let batch = StableReplayFrameBatch {
        start_index: 0,
        index: 2,
        frames: Arc::new(vec![seed, cursor].into()),
    };
    assert_eq!(batch.newest(), Some(&cursor));
    let seed_only = StableReplayFrameBatch {
        start_index: 0,
        index: 1,
        frames: Arc::new(vec![seed].into()),
    };
    assert_eq!(seed_only.newest(), None);
    let trailing_seed = StableReplayFrameBatch {
        start_index: 0,
        index: 3,
        frames: Arc::new(vec![seed, cursor, seed].into()),
    };
    assert_eq!(trailing_seed.newest(), Some(&cursor));
}

#[test]
fn stable_all_rulesets_decode_raw_replay_slots_and_bound_long_map_history() {
    for mode in 0..=3 {
        let total = 30_000usize;
        let items = 0x2000;
        let frame_base = items + total * 4 + 16;
        let mut bytes = vec![0; frame_base + total * 20];
        put_u32(&mut bytes, 0x305, 0x350);
        put_i32(&mut bytes, 0x350, i32::MAX);
        put_u32(&mut bytes, 0x900 - 0xb, 0x1200);
        put_u32(&mut bytes, 0x1204, 0x1300);
        put_u32(&mut bytes, 0x1364, 0x1400);
        put_u32(&mut bytes, 0x1438, 0x1500);
        put_u32(&mut bytes, 0x1538, 0x1600);
        put_u32(&mut bytes, 0x1604, 0x1700);
        put_u32(&mut bytes, 0x1534, 0x1800);
        put_u32(&mut bytes, 0x1804, items as u32);
        for index in 0..total {
            let frame = frame_base + index * 20;
            put_u32(&mut bytes, items + LEADER_START + index * 4, frame as u32);
            put_f32(
                &mut bytes,
                frame + 4,
                if mode == 3 {
                    (index % 16) as f32
                } else {
                    123.25
                },
            );
            put_f32(&mut bytes, frame + 8, 234.5);
            put_u32(&mut bytes, frame + 12, 0xaabb_cc00 | (index % 32) as u32);
            put_i32(&mut bytes, frame + 16, index as i32 * 50);
        }
        let memory = Arc::new(MutableMemory {
            bytes: ArcSwap::from_pointee(bytes),
            batches: AtomicUsize::new(0),
        });
        let mut reader = StableMemory::new(
            ProcessHandle::new(memory.clone(), ProcessBitness::X86),
            fixture_patterns(),
        );
        reader.synchronize_precise(ReaderSynchronization::default(), mode);
        let mut acknowledged_frame = 0;
        let mut previous: Option<StableReplayFrameBatch> = None;
        for size in (100..=total).step_by(100) {
            let mut next = (**memory.bytes.load()).clone();
            put_i32(&mut next, 0x180c, size as i32);
            memory.bytes.store(Arc::new(next));
            let ReadStatus::Ready(errors) = reader.hit_errors(0).unwrap() else {
                panic!("replay capture ready");
            };
            let batch = errors.stable_replay_frames.unwrap();
            assert_eq!(batch.index, size as i64);
            assert!(batch.frames.len() <= crate::model::memory::StableReplayFrames::LIMIT);
            assert_eq!(batch.frames_since(acknowledged_frame).unwrap().count(), 100);
            let newest = batch.newest().unwrap();
            assert_eq!(newest.time, (size - 1) as i64 * 50);
            assert_eq!(
                newest.x,
                if mode == 3 {
                    ((size - 1) % 16) as f32
                } else {
                    123.25
                }
            );
            assert_eq!(newest.y, 234.5);
            assert_eq!(newest.buttons, ((size - 1) % 32) as u8);
            assert_eq!(newest.buttons_raw, 0xaabb_cc00 | ((size - 1) % 32) as u32);
            if let Some(previous) = previous {
                assert_eq!(previous.newest().unwrap().time, (size - 101) as i64 * 50);
            }
            acknowledged_frame = batch.index;
            previous = Some(batch);
        }
        let batch = previous.unwrap();
        assert!(batch.start_index > 0);
        assert!(batch.frames_since(0).is_none());
        assert!(batch.frames_since(batch.index + 1).is_none());
        assert!(batch.frames_since(batch.index).unwrap().next().is_none());
        assert!(memory.batches.load(Ordering::Relaxed) >= 300);
        let mut late_reader = StableMemory::new(
            ProcessHandle::new(memory.clone(), ProcessBitness::X86),
            fixture_patterns(),
        );
        late_reader.synchronize_precise(ReaderSynchronization::default(), mode);
        let ReadStatus::Ready(late) = late_reader.hit_errors(0).unwrap() else {
            panic!("late capture ready");
        };
        let late = late.stable_replay_frames.unwrap();
        assert_eq!(
            late.frames.len(),
            crate::model::memory::StableReplayFrames::LIMIT
        );
        assert_eq!(late.index, total as i64);
        assert_eq!(late.start_index, total as i64 - late.frames.len() as i64);
        // A preloaded complete replay must retain the beginning while the clock
        // is there, rather than exposing only the final rolling window.
        let mut early = (**memory.bytes.load()).clone();
        put_i32(&mut early, 0x350, 500);
        memory.bytes.store(Arc::new(early));
        let ReadStatus::Ready(early) = reader.hit_errors(0).unwrap() else {
            panic!("preloaded replay capture ready");
        };
        let early = early.stable_replay_frames.unwrap();
        assert_eq!((early.start_index, early.index), (0, 11));
        assert_eq!(early.newest().unwrap().time, 500);
        let mut rewound = (**memory.bytes.load()).clone();
        put_i32(&mut rewound, 0x350, i32::MAX);
        put_i32(&mut rewound, 0x180c, 2);
        memory.bytes.store(Arc::new(rewound));
        let ReadStatus::Ready(rewound) = reader.hit_errors(0).unwrap() else {
            panic!("rewound capture ready");
        };
        let rewound = rewound.stable_replay_frames.unwrap();
        assert_eq!(rewound.start_index, 0);
        assert_eq!(rewound.index, 2);
        assert_eq!(rewound.frames.len(), 2);
        let mut invalid = (**memory.bytes.load()).clone();
        put_i32(&mut invalid, 0x180c, 3);
        put_f32(&mut invalid, frame_base + 2 * 20 + 4, f32::NAN);
        memory.bytes.store(Arc::new(invalid));
        if mode == 3 {
            assert!(matches!(
                reader.hit_errors(0),
                Err(MemoryError::Corrupt { .. })
            ));
        } else {
            // A bad spatial suffix loses only frame data, never timing errors.
            let mut next = (**memory.bytes.load()).clone();
            put_i32(&mut next, 0x160c, 1);
            put_i32(&mut next, 0x1700 + LEADER_START, 7);
            memory.bytes.store(Arc::new(next));
            let ReadStatus::Ready(errors) = reader.hit_errors(0).unwrap() else {
                panic!("hit errors survive invalid frames");
            };
            assert_eq!(errors.index, 1);
            assert_eq!(errors.errors.all, [7.0]);
            assert!(errors.stable_replay_frames.is_none());
            assert_eq!(reader.replay_frame_index, 3);

            let mut next = (**memory.bytes.load()).clone();
            put_i32(&mut next, 0x180c, 4);
            memory.bytes.store(Arc::new(next));
            let ReadStatus::Ready(later) = reader.hit_errors(1).unwrap() else {
                panic!("later frames ready");
            };
            let batch = later.stable_replay_frames.unwrap();
            assert_eq!((batch.start_index, batch.index), (3, 4));
            assert_eq!(batch.frames.len(), 1);
            assert_eq!(batch.newest().unwrap().time, 150);
            assert_eq!(
                reader.catcher(150.0).unwrap(),
                ReadStatus::Ready(crate::model::memory::Catcher {
                    x: 123.25,
                    dashing: false
                })
            );
        }
    }
}

#[test]
fn stable_session_clock_follows_instruction_operand() {
    let mut bytes = vec![0; 0x8000];
    // The A1 operand contains the clock address, not the clock value.
    put_u32(&mut bytes, fixture_patterns().game_time_ptr, 0x7000);
    put_u32(&mut bytes, 0x7000, 523_409);
    let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());

    let ReadStatus::Ready(global) = reader.global().unwrap() else {
        panic!("stable global ready");
    };
    assert_eq!(global.game_time, 523_409);
}

#[test]
fn every_public_method_has_a_deterministic_zero_memory_sentinel() {
    let mut reader = StableMemory::new(handle(0, vec![0; 0x8000]), fixture_patterns());
    assert!(matches!(
        reader.audio_velocity_base().unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(reader.user().unwrap(), ReadStatus::NotReady));
    assert!(matches!(
        reader.result_screen().unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.gameplay(0.0, None).unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.key_overlay(0).unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.hit_errors(0).unwrap(),
        ReadStatus::NotReady
    ));
    let _ = reader.global().unwrap();
    let _ = reader.global_precise().unwrap();
    assert!(matches!(reader.menu("").unwrap(), ReadStatus::NotReady));
    assert!(matches!(reader.mp3_length().unwrap(), ReadStatus::Ready(value) if value == 0.0));
    assert!(matches!(reader.tourney().unwrap(), ReadStatus::NotReady));
    assert!(matches!(
        reader.tourney_chat(&[]).unwrap(),
        ReadStatus::NotReady
    ));
    assert!(matches!(
        reader.tourney_user().unwrap(),
        ReadStatus::NotReady
    ));
    assert!(
        matches!(reader.leaderboard(0).unwrap(),ReadStatus::Ready((false,None,v)) if v.is_empty())
    );
    assert!(matches!(
        reader.ranked_play().unwrap(),
        ReadStatus::Unsupported("Stable: ranked play not implemented")
    ));
    assert!(matches!(reader.settings().unwrap(), ReadStatus::NotReady));
}
#[test]
fn key_overlay_decodes_short_mania_collections() {
    let buttons = key_overlay_buttons(&mut key_overlay_reader(3), 3);
    assert_eq!(
        buttons,
        [
            KeyOverlayButton {
                name: "K1".into(),
                is_pressed: true,
                count: 10,
            },
            KeyOverlayButton {
                name: "K2".into(),
                is_pressed: false,
                count: 11,
            },
            KeyOverlayButton {
                name: "K3".into(),
                is_pressed: true,
                count: 12,
            },
        ]
    );
}

#[test]
fn key_overlay_preserves_ruleset_names_and_returns_every_button() {
    let expected = [
        (0, ["K1", "K2", "M1", "M2", "K5"]),
        (1, ["K1", "K2", "M1", "M2", "K5"]),
        (2, ["L", "R", "D", "M2", "K5"]),
        (3, ["K1", "K2", "K3", "K4", "K5"]),
    ];
    let mut reader = key_overlay_reader(5);
    for (mode, names) in expected {
        let buttons = key_overlay_buttons(&mut reader, mode);
        assert_eq!(
            buttons
                .iter()
                .map(|button| button.name.as_ref())
                .collect::<Vec<_>>(),
            names
        );
    }
    let mania = key_overlay_buttons(&mut key_overlay_reader(18), 3);
    assert_eq!((mania.len(), mania[17].name.as_ref()), (18, "K18"));
    let mania_again = key_overlay_buttons(&mut key_overlay_reader(18), 3);
    assert!(Arc::ptr_eq(&mania[17].name, &mania_again[17].name));
}

#[test]
fn key_overlay_batches_all_button_states_after_pointer_decode() {
    let (mut reader, memory) = key_overlay_reader_with_memory(18);
    let buttons = key_overlay_buttons(&mut reader, 3);

    assert_eq!(buttons.len(), 18);
    assert_eq!(memory.batches.load(Ordering::Relaxed), 1);
}

#[test]
fn key_overlay_rejects_oversized_collections_before_reading_entries() {
    assert!(matches!(
        key_overlay_reader(19).key_overlay(3),
        Err(MemoryError::Corrupt {
            context: "stable key overlay",
            ..
        })
    ));
}

#[test]
fn accuracy_statistics_mod_order_and_defaults_match_source() {
    let no_mods = mods(0);
    assert_eq!(
        accuracy(0, &legacy_stats(0, 300, 0, 0, 0, 0, 0), &no_mods),
        100.0
    );
    let taiko = legacy_stats(1, 1, 2, 0, 3, 4, 5);
    assert_eq!(taiko.large_bonus, 9);
    let catch = legacy_stats(2, 1, 2, 3, 4, 0, 5);
    assert_eq!(catch.small_tick_miss, 5);
    let v2 = mods(1 << 29);
    let mania = legacy_stats(3, 1, 0, 0, 0, 1, 0);
    assert_eq!(accuracy(3, &mania, &v2), 99.18);
    assert_eq!(v2.name, "v2");
    assert_eq!(v2.array[0].acronym, "V2");
    let ordered = mods(1 | 8);
    assert_eq!(ordered.name, "HDNF");
    assert_eq!(
        ordered
            .array
            .iter()
            .map(|m| m.acronym.as_str())
            .collect::<Vec<_>>(),
        vec!["HD", "NF"]
    );
    assert_eq!(mods(128).checksum, "024809c484bde52b1b998b4bc529d8be");
    assert_eq!(default_stats().legacy_combo_increase, Some(0));
    assert_eq!(empty_stats().legacy_combo_increase, None);
    assert_eq!(shifted(0x100, -0xb).unwrap(), 0xf5);
    assert!(shifted(3, -4).is_err());
}
#[test]
fn legacy_dictionary_and_string_helpers_match_x86_layout() {
    let mut bytes = vec![0; 0x400];
    bytes[0x104..0x108].copy_from_slice(&2i32.to_le_bytes());
    bytes[0x108..0x10a].copy_from_slice(&(b'O' as u16).to_le_bytes());
    bytes[0x10a..0x10c].copy_from_slice(&(b'K' as u16).to_le_bytes());
    bytes[0x208..0x20c].copy_from_slice(&0x280u32.to_le_bytes());
    bytes[0x21c..0x220].copy_from_slice(&2i32.to_le_bytes());
    let h = handle(0, bytes);
    assert!(matches!(h.read_stable_string(0x100).unwrap(),ReadStatus::Ready(v) if v=="OK"));
    assert_eq!(
        h.read_stable_dictionary_slots(0x200).unwrap(),
        vec![0x288, 0x298]
    );
}
#[test]
fn date_mime_and_virtual_keys_match_source_tables() {
    assert_eq!(
        net_date(144670709, -139100160).unwrap(),
        "1970-01-01T00:00:00.000Z"
    );
    let epoch_ticks = 621_355_968_000_000_000_u64 - 9_999;
    assert_eq!(
        net_date((epoch_ticks >> 32) as i32, epoch_ticks as u32 as i32).unwrap(),
        "1970-01-01T00:00:00.000Z"
    );
    assert_eq!(mime("background.jpg"), "image/jpg; charset=utf-8");
    assert_eq!(mime("audio.mp3"), "audio/mpeg; charset=utf-8");
    assert_eq!(mime("image.webp"), "image/webp; charset=utf-8");
    assert_eq!(mime("IMAGE.JPG"), "");
    assert_eq!(mime("unknown.zzz"), "");
    assert_eq!(key_name(97), Some("NumPad1"));
    assert_eq!(key_name(160), Some("LeftShift"));
    assert_eq!(key_name(163), Some("RightControl"));
    assert_eq!(key_name(16), None);
}
#[test]
fn adjacent_score_fields_use_read_many_batch() {
    let mut bytes = vec![0; 0x400];
    for (i, value) in [1i16, 2, 3, 4, 5, 6, 7].into_iter().enumerate() {
        bytes[0x188 + 2 * i..0x18a + 2 * i].copy_from_slice(&value.to_le_bytes())
    }
    bytes[0x168..0x16a].copy_from_slice(&8i16.to_le_bytes());
    let memory = Arc::new(SparseMemory {
        base: 0,
        bytes,
        reads: AtomicUsize::new(0),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    let reader = StableMemory::new(process, fixture_patterns());
    assert_eq!(
        reader.score_counts(0x100).unwrap(),
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!(memory.batches.load(Ordering::Relaxed), 1);
}

#[test]
fn steady_ruleset_reads_validate_and_fetch_the_chain_in_one_backend_call() {
    let mut bytes = vec![0; 0x1400];
    put_u32(&mut bytes, 0x8f5, 0x1000);
    put_u32(&mut bytes, 0x1004, 0x1100);
    let memory = Arc::new(SparseMemory {
        base: 0,
        bytes,
        reads: AtomicUsize::new(0),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    let mut reader = StableMemory::new(process, fixture_patterns());

    assert_eq!(reader.ruleset().unwrap(), 0x1100);
    assert_eq!(memory.reads.load(Ordering::Relaxed), 2);
    assert_eq!(reader.ruleset().unwrap(), 0x1100);
    assert_eq!(memory.reads.load(Ordering::Relaxed), 3);
    assert_eq!(memory.batches.load(Ordering::Relaxed), 1);
}

#[test]
fn precise_global_reuses_validated_pointer_chains_without_changing_values() {
    let mut bytes = vec![0; 0x1400];
    put_u32(&mut bytes, 0xb00, 0x1200);
    put_u32(&mut bytes, 0x1200, 2);
    put_u32(&mut bytes, 0x305, 0x1210);
    put_i32(&mut bytes, 0x1210, -123);
    let memory = Arc::new(SparseMemory {
        base: 0,
        bytes,
        reads: AtomicUsize::new(0),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    let mut reader = StableMemory::new(process, fixture_patterns());

    let ReadStatus::Ready(first) = reader.global_precise().unwrap() else {
        panic!()
    };
    let ReadStatus::Ready(second) = reader.global_precise().unwrap() else {
        panic!()
    };
    assert_eq!(
        first,
        GlobalPrecise {
            status: 2,
            time: -123.0,
        }
    );
    assert_eq!(second, first);
    assert_eq!(memory.reads.load(Ordering::Relaxed), 6);
    assert_eq!(memory.batches.load(Ordering::Relaxed), 2);
}

#[test]
fn precise_global_pointer_replacement_never_returns_cached_values() {
    let mut initial = vec![0; 0x1600];
    put_u32(&mut initial, 0xb00, 0x1400);
    put_u32(&mut initial, 0x1400, 2);
    put_u32(&mut initial, 0x305, 0x1410);
    put_i32(&mut initial, 0x1410, 100);
    let memory = Arc::new(MutableMemory {
        bytes: ArcSwap::from_pointee(initial),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    let mut reader = StableMemory::new(process, fixture_patterns());
    assert_eq!(
        reader.global_precise().unwrap(),
        ReadStatus::Ready(GlobalPrecise {
            status: 2,
            time: 100.0,
        })
    );

    let mut replacement = vec![0; 0x1200];
    put_u32(&mut replacement, 0xb00, 0x1000);
    put_u32(&mut replacement, 0x1000, 4);
    put_u32(&mut replacement, 0x305, 0x1010);
    put_i32(&mut replacement, 0x1010, 200);
    memory.bytes.store(Arc::new(replacement));
    assert_eq!(
        reader.global_precise().unwrap(),
        ReadStatus::Ready(GlobalPrecise {
            status: 4,
            time: 200.0,
        })
    );
}
#[test]
fn ruleset_cache_tracks_replacement_invalidates_on_failure_and_recovers() {
    let mut initial = vec![0; 0x1600];
    put_u32(&mut initial, 0x8f5, 0x1400);
    put_u32(&mut initial, 0x1404, 0x1500);
    let memory = Arc::new(MutableMemory {
        bytes: ArcSwap::from_pointee(initial),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    let mut reader = StableMemory::new(process, fixture_patterns());
    assert_eq!(reader.ruleset().unwrap(), 0x1500);

    let mut replacement = vec![0; 0x1200];
    put_u32(&mut replacement, 0x8f5, 0x1000);
    put_u32(&mut replacement, 0x1004, 0x1100);
    memory.bytes.store(Arc::new(replacement));
    assert_eq!(reader.ruleset().unwrap(), 0x1100);
    assert_eq!(reader.ruleset_container, 0x1000);

    memory.bytes.store(Arc::new(vec![0; 0x8f5]));
    assert!(reader.ruleset().is_err());
    assert_eq!(reader.ruleset_container, 0);

    let mut recovered = vec![0; 0x1800];
    put_u32(&mut recovered, 0x8f5, 0x1400);
    put_u32(&mut recovered, 0x1404, 0x1500);
    memory.bytes.store(Arc::new(recovered));
    assert_eq!(reader.ruleset().unwrap(), 0x1500);

    reader.synchronize_capture_config(false, true);
    assert_eq!(reader.status_container, 0);
    assert_eq!(reader.play_time_container, 0);
    assert_eq!(reader.ruleset_container, 0);
}
#[test]
fn unknown_first_object_preserves_source_sentinel() {
    assert!(!score_has_started(10_000.0, None));
    assert!(!score_has_started(899.0, Some(1_000)));
    assert!(score_has_started(900.0, Some(1_000)));
}
#[test]
fn read_long_values_cross_javascript_number_boundary() {
    assert_eq!(js_long(9_007_199_254_740_991), 9_007_199_254_740_991.0);
    assert_eq!(js_long(9_007_199_254_740_993), 9_007_199_254_740_992.0);
    assert_eq!(js_long(-9_007_199_254_740_993), -9_007_199_254_740_992.0);
}
#[test]
fn tourney_chat_skips_corrupt_channels_and_messages() {
    fn put_u32(bytes: &mut [u8], address: usize, value: u32) {
        bytes[address..address + 4].copy_from_slice(&value.to_le_bytes())
    }
    fn put_i32(bytes: &mut [u8], address: usize, value: i32) {
        bytes[address..address + 4].copy_from_slice(&value.to_le_bytes())
    }
    fn put_string(bytes: &mut [u8], address: usize, value: &str) {
        put_i32(bytes, address + 4, value.encode_utf16().count() as i32);
        for (index, unit) in value.encode_utf16().enumerate() {
            bytes[address + 8 + 2 * index..address + 10 + 2 * index]
                .copy_from_slice(&unit.to_le_bytes())
        }
    }
    let mut bytes = vec![0; 0x1000];
    let pattern = Pattern::parse(CHAT_PATTERN, false).unwrap();
    for (index, (&byte, &mask)) in pattern.bytes().iter().zip(pattern.mask()).enumerate() {
        bytes[0x100 + index] = if mask == 0 { 1 } else { byte }
    }
    put_u32(&mut bytes, 0x101, 0x200);
    put_u32(&mut bytes, 0x200, 0x300);
    put_u32(&mut bytes, 0x304, 0x400);
    put_i32(&mut bytes, 0x404, 2);
    put_u32(&mut bytes, 0x408, 0x500);
    put_u32(&mut bytes, 0x40c, 0xffff);
    put_u32(&mut bytes, 0x504, 0x600);
    put_string(&mut bytes, 0x600, "#multiplayer");
    put_u32(&mut bytes, 0x510, 0x700);
    put_u32(&mut bytes, 0x704, 0x800);
    put_i32(&mut bytes, 0x70c, 2);
    put_u32(&mut bytes, 0x808, 0x900);
    put_u32(&mut bytes, 0x80c, 0xffff);
    put_u32(&mut bytes, 0x904, 0xa00);
    put_u32(&mut bytes, 0x908, 0xb00);
    put_string(&mut bytes, 0xa00, "hello");
    put_string(&mut bytes, 0xb00, "12:34 User:");
    let mut oversized = bytes.clone();
    put_i32(
        &mut oversized,
        0x404,
        (MAX_TOURNEY_CHAT_CHANNELS + 1) as i32,
    );
    let mut bounded = StableMemory::new(handle(0, oversized), fixture_patterns());
    assert!(matches!(
        bounded.tourney_chat(&[]),
        Err(MemoryError::Corrupt {
            context: "tournament chat channels",
            ..
        })
    ));
    let mut oversized_messages = bytes.clone();
    put_i32(
        &mut oversized_messages,
        0x70c,
        (MAX_TOURNEY_CHAT_MESSAGES + 1) as i32,
    );
    let mut bounded_messages = StableMemory::new(handle(0, oversized_messages), fixture_patterns());
    assert!(matches!(
        bounded_messages.tourney_chat(&[]),
        Err(MemoryError::Corrupt {
            context: "tournament chat messages",
            ..
        })
    ));
    let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());
    let result = reader.tourney_chat(&[]).unwrap();
    let ReadStatus::Ready(messages) = result else {
        panic!("{result:?}")
    };
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].time, "12:34");
    assert_eq!(messages[0].name, "User");
    assert_eq!(messages[0].content, "hello");
}
#[test]
fn compound_mod_replacements_require_literal_adjacency() {
    let separated = mods(32 | 4096 | 16384);
    assert_eq!(separated.name, "SDSOPF");
    assert_eq!(
        separated
            .array
            .iter()
            .map(|item| item.acronym.as_str())
            .collect::<Vec<_>>(),
        vec!["SD", "SO", "PF"]
    );
    let adjacent = mods(32 | 16384);
    assert_eq!(adjacent.name, "PF");
    assert_eq!(adjacent.array[0].acronym, "PF");
    assert_eq!(mods(64 | 512).name, "NC");
    assert_eq!(mods(2048 | 4194304).name, "CN");
}
#[test]
fn random_seed_recalculates_the_full_mod_array() {
    let mut bytes = vec![0; 0x400];
    put_i32(&mut bytes, 0x218, 123);
    let reader = StableMemory::new(handle(0, bytes), fixture_patterns());
    let original = mods((1 << 21) | (1 << 25));
    assert_eq!(original.number, (1 << 21) | (1 << 25));
    let recalculated = reader.with_random_seed(original, 0x100, 3).unwrap();
    assert_eq!(recalculated.number, 1 << 21);
    assert_eq!(recalculated.name, "RD10K");
    assert_eq!(recalculated.checksum, "b28253a762eabb35ac8a6082e24f744e");
    assert_eq!(
        recalculated
            .array
            .iter()
            .map(|item| item.acronym.as_str())
            .collect::<Vec<_>>(),
        vec!["RD", "10", "K"]
    );
    assert_eq!(
        recalculated.array[0].settings.as_ref().unwrap().get("seed"),
        Some(&json!(123))
    );
}
#[test]
fn gameplay_missing_health_preserves_the_individual_field_error() {
    let mut bytes = vec![0; 0x141c];
    put_u32(&mut bytes, 0x8f5, 0x1000);
    put_u32(&mut bytes, 0x1004, 0x1100);
    put_u32(&mut bytes, 0x1164, 0x1200);
    put_u32(&mut bytes, 0x1238, 0x1300);
    put_u32(&mut bytes, 0x1240, 0x1400);
    let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());
    assert!(matches!(
        reader.gameplay(0.0, None),
        Err(MemoryError::Read {
            address: 0x141c,
            len: 8,
            ..
        })
    ));
}

#[test]
fn gameplay_boundary_receives_timing_and_reads_scores_after_first_object() {
    let mut bytes = vec![0; 0x2000];
    put_u32(&mut bytes, 0x8f5, 0x1000);
    put_u32(&mut bytes, 0x1004, 0x1100);
    put_u32(&mut bytes, 0x1164, 0x1200);
    put_u32(&mut bytes, 0x1238, 0x1300);
    put_u32(&mut bytes, 0x1240, 0x1400);
    put_u32(&mut bytes, 0x1248, 0x1700);
    put_u32(&mut bytes, 0x131c, 0x1500);
    put_u32(&mut bytes, 0x1cd, 0x1600);
    for (index, value) in [1i16, 2, 3, 4, 5, 6, 7].into_iter().enumerate() {
        bytes[0x1388 + 2 * index..0x138a + 2 * index].copy_from_slice(&value.to_le_bytes())
    }
    bytes[0x1368..0x136a].copy_from_slice(&8i16.to_le_bytes());
    bytes[0x141c..0x1424].copy_from_slice(&1.0f64.to_le_bytes());
    bytes[0x1414..0x141c].copy_from_slice(&0.5f64.to_le_bytes());
    bytes[0x170c..0x1714].copy_from_slice(&75.0f64.to_le_bytes());
    let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());
    let ReadStatus::Ready(before) = reader.gameplay(10_000_f64, None).unwrap() else {
        panic!()
    };
    assert_eq!(before.statistics.great, 0);
    assert_eq!(before.combo, 0);
    assert_eq!(before.max_combo, 0);
    let ReadStatus::Ready(after) = reader.gameplay(1_000_f64, Some(1_000)).unwrap() else {
        panic!()
    };
    assert_eq!(after.statistics.great, 2);
    assert_eq!(after.statistics.ok, 1);
    assert_eq!(after.combo, 7);
    assert_eq!(after.max_combo, 8);
}
#[test]
fn settings_rescan_empty_dictionary_batch_values_recover_errors_and_read_mania_fallback() {
    let mut bytes = vec![0; 0x2000];
    put_u32(&mut bytes, 0x700, 0x100);
    put_u32(&mut bytes, 0x100, 0x300);
    put_u32(&mut bytes, 0x800, 0x104);
    put_u32(&mut bytes, 0x104, 0x500);
    put_u32(&mut bytes, 0x308, 0x900);
    put_u32(&mut bytes, 0x508, 0xd00);
    let pattern = Pattern::parse(MANIA_PATTERN, false).unwrap();
    for (index, (&byte, &mask)) in pattern.bytes().iter().zip(pattern.mask()).enumerate() {
        bytes[0x1200 + index] = if mask == 0 { 1 } else { byte }
    }
    put_u32(&mut bytes, 0x1201, 0x1300);
    put_i32(&mut bytes, 0x1300, 17);
    let memory = Arc::new(MutableMemory {
        bytes: ArcSwap::from_pointee(bytes),
        batches: AtomicUsize::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    let mut reader = StableMemory::new(process, fixture_patterns());
    reader.gameplay_mode = 3;
    let ReadStatus::Ready(first) = reader.settings().unwrap() else {
        panic!()
    };
    assert_eq!(first.get("mania.scrollSpeed"), Some(&json!(17.0)));
    assert!(!first.contains_key("audio.volume.master"));
    {
        let mut bytes = (**memory.bytes.load()).clone();
        put_i32(&mut bytes, 0x31c, 3);
        put_u32(&mut bytes, 0x908, 0xa00);
        put_u32(&mut bytes, 0x90c, 0xb00);
        put_string(&mut bytes, 0xa00, "VolumeUniversal");
        bytes[0xb04..0xb0c].copy_from_slice(&42.0f64.to_le_bytes());
        put_u32(&mut bytes, 0x918, 0xc00);
        put_u32(&mut bytes, 0x91c, 0xffff);
        put_string(&mut bytes, 0xc00, "VolumeMusic");
        put_u32(&mut bytes, 0x928, 0xe00);
        put_u32(&mut bytes, 0x92c, 0xf00);
        put_string(&mut bytes, 0xe00, "ManiaSpeed");
        bytes[0xf04..0xf0c].copy_from_slice(&9.0f64.to_le_bytes());
        memory.bytes.store(Arc::new(bytes));
    }
    let before_batches = memory.batches.load(Ordering::Relaxed);
    let ReadStatus::Ready(second) = reader.settings().unwrap() else {
        panic!()
    };
    assert_eq!(second.get("audio.volume.master"), Some(&json!(42.0)));
    assert!(!second.contains_key("audio.volume.music"));
    assert_eq!(second.get("mania.scrollSpeed"), Some(&json!(17.0)));
    assert!(memory.batches.load(Ordering::Relaxed) >= before_batches + 2);
    {
        let mut bytes = (**memory.bytes.load()).clone();
        put_u32(&mut bytes, 0x1201, 0xffff);
        memory.bytes.store(Arc::new(bytes));
    }
    let ReadStatus::Ready(fallback) = reader.settings().unwrap() else {
        panic!()
    };
    assert_eq!(fallback.get("mania.scrollSpeed"), Some(&json!(9.0)));
    assert_eq!(fallback.get("audio.volume.master"), Some(&json!(42.0)));
}
#[test]
fn mp3_length_preserves_javascript_rounding_and_nonfinite_values() {
    fn read_length(value: f64) -> f64 {
        let mut bytes = vec![0; 0xe00];
        put_u32(&mut bytes, 0xd00, 0x100);
        put_u32(&mut bytes, 0x100, 0x200);
        bytes[0x204..0x20c].copy_from_slice(&value.to_le_bytes());
        let mut reader = StableMemory::new(handle(0, bytes), fixture_patterns());
        let ReadStatus::Ready(length) = reader.mp3_length().unwrap() else {
            panic!()
        };
        length
    }
    let tie = read_length(-0.5);
    assert_eq!(tie, 0.0);
    assert!(tie.is_sign_negative());
    let above = read_length(-0.499_999_999_999_999_94);
    assert_eq!(above, 0.0);
    assert!(above.is_sign_negative());
    assert_eq!(read_length(-0.500_000_000_000_000_1), -1.0);
    assert_eq!(read_length(-1.5), -1.0);
    assert_eq!(read_length(1.5), 2.0);
    assert!(read_length(f64::NAN).is_nan());
    assert_eq!(read_length(f64::INFINITY), f64::INFINITY);
    assert_eq!(read_length(f64::NEG_INFINITY), f64::NEG_INFINITY);
}

#[test]
fn catch_position_tracks_play_time_without_new_judgements() {
    let count = 3usize;
    let items = 0x2000;
    let frame_base = items + count * 4 + 16;
    let mut bytes = vec![0; frame_base + count * 20];
    put_u32(&mut bytes, 0x305, 0x350);
    put_u32(&mut bytes, 0x900 - 0xb, 0x1200);
    put_u32(&mut bytes, 0x1204, 0x1300);
    put_u32(&mut bytes, 0x1364, 0x1400);
    put_u32(&mut bytes, 0x1438, 0x1500);
    put_u32(&mut bytes, 0x1538, 0x1600);
    put_u32(&mut bytes, 0x1604, 0x1700);
    put_u32(&mut bytes, 0x1534, 0x1800);
    put_u32(&mut bytes, 0x1804, items as u32);
    put_i32(&mut bytes, 0x180c, count as i32);
    for index in 0..count {
        let frame = frame_base + index * 20;
        put_u32(&mut bytes, items + LEADER_START + index * 4, frame as u32);
        put_f32(&mut bytes, frame + 4, index as f32);
        put_f32(&mut bytes, frame + 8, 0.0);
        put_u32(&mut bytes, frame + 12, (index % 2) as u32);
        put_i32(&mut bytes, frame + 16, index as i32 * 100);
    }
    let memory = Arc::new(MutableMemory {
        bytes: ArcSwap::from_pointee(bytes),
        batches: AtomicUsize::new(0),
    });
    let mut reader = StableMemory::new(
        ProcessHandle::new(memory.clone(), ProcessBitness::X86),
        fixture_patterns(),
    );
    reader.synchronize_precise(ReaderSynchronization::default(), 2);
    for (time, x, dashing) in [(50.0, 0.0, false), (150.0, 1.0, true), (250.0, 2.0, false)] {
        // Shared replay capture retains only frames at the native playback clock.
        let mut next = (**memory.bytes.load()).clone();
        put_i32(&mut next, 0x350, time as i32);
        memory.bytes.store(Arc::new(next));
        let ReadStatus::Ready(errors) = reader.hit_errors(0).unwrap() else {
            panic!("empty judgement suffix must still capture replay frames");
        };
        assert_eq!(errors.index, 0);
        assert!(errors.errors.all.is_empty());
        assert_eq!(
            reader.catcher(time).unwrap(),
            ReadStatus::Ready(crate::model::memory::Catcher { x, dashing })
        );
    }
    assert_eq!(reader.catcher(-1.0).unwrap(), ReadStatus::NotReady);
}

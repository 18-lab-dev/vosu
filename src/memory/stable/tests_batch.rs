//! Batched osu!stable readers against their sequential references.
//!
//! Every comparison runs a capture-shaped call sequence twice on identical memory: once
//! through the public (batched, falling back) readers and once through the sequential
//! readers. Results, errors, and reader caches must match exactly, both on a healthy graph
//! and with each byte range the sequential readers touch, or the bytes just past it, made
//! unreadable.
use super::*;

use super::batched::StringSlot;
use super::tests::fixture_patterns;
use crate::process::{MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};
use parking_lot::Mutex;
use std::sync::Arc;

struct FaultMemory {
    bytes: Vec<u8>,
    poisoned: Option<(usize, usize)>,
    log: Mutex<Vec<(usize, usize)>>,
    calls: Mutex<usize>,
}

impl FaultMemory {
    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.log.lock().push((address, dst.len()));
        let end = address
            .checked_add(dst.len())
            .ok_or(MemoryError::AddressOverflow {
                base: address,
                offset: dst.len(),
            })?;
        let failure = || MemoryError::Read {
            address,
            len: dst.len(),
            message: "unreadable fixture range".into(),
        };
        if self
            .poisoned
            .is_some_and(|(start, stop)| address < stop && start < end)
        {
            return Err(failure());
        }
        let source = self.bytes.get(address..end).ok_or_else(failure)?;
        dst.copy_from_slice(source);
        Ok(())
    }
}

impl ProcessMemory for FaultMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        *self.calls.lock() += 1;
        self.copy(address, dst)
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        *self.calls.lock() += 1;
        let result = reads
            .iter_mut()
            .try_for_each(|read| self.copy(read.address, read.dst));
        if result.is_err() {
            for read in reads {
                read.dst.fill(0);
            }
        }
        result
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.bytes.len(),
        }])
    }
}

const SIZE: usize = 0x40000;

struct Graph {
    bytes: Vec<u8>,
    next: usize,
}

impl Graph {
    fn node(&mut self) -> usize {
        let address = self.next;
        self.next += 0x200;
        address
    }
    fn put(&mut self, address: usize, value: &[u8]) {
        self.bytes[address..address + value.len()].copy_from_slice(value);
    }
    fn u32(&mut self, address: usize, value: usize) {
        self.put(address, &(value as u32).to_le_bytes());
    }
    fn i32(&mut self, address: usize, value: i32) {
        self.put(address, &value.to_le_bytes());
    }
    fn i16(&mut self, address: usize, value: i16) {
        self.put(address, &value.to_le_bytes());
    }
    fn f32(&mut self, address: usize, value: f32) {
        self.put(address, &value.to_le_bytes());
    }
    fn f64(&mut self, address: usize, value: f64) {
        self.put(address, &value.to_le_bytes());
    }
    fn child(&mut self, holder: usize) -> usize {
        let node = self.node();
        self.u32(holder, node);
        node
    }
    fn chain(&mut self, holder: usize, value: usize) {
        let node = self.child(holder);
        self.u32(node, value);
    }
    fn string_at(&mut self, holder: usize, address: usize, text: &str) {
        self.u32(holder, address);
        let units = text.encode_utf16().collect::<Vec<_>>();
        self.i32(address + 4, units.len() as i32);
        for (index, unit) in units.iter().enumerate() {
            self.put(address + 8 + 2 * index, &unit.to_le_bytes());
        }
    }
    fn string(&mut self, holder: usize, text: &str) {
        let address = self.next;
        self.next += (8 + 2 * text.encode_utf16().count()).next_multiple_of(0x200);
        self.string_at(holder, address, text);
    }
}

struct Variant {
    status: i32,
    watching: bool,
    gameplay_mode: i32,
    mod_bits: i32,
    checksum_matches: bool,
    first_object_time: Option<i64>,
}

fn graph(variant: &Variant) -> Vec<u8> {
    let p = fixture_patterns();
    let mut g = Graph {
        bytes: vec![0; SIZE],
        next: 0x2000,
    };
    g.chain(p.menu_mods_ptr, 72);
    let chat = g.child(p.chat_checker_ptr);
    g.put(chat, &[0xfe]);
    let watching = g.child(p.can_run_slowly_addr + 0x46);
    g.put(watching, &[u8::from(variant.watching)]);
    g.chain(p.game_time_ptr, 999);
    let status = g.child(p.status_ptr);
    g.i32(status, variant.status);
    let play_time = g.child(p.play_time_addr + 5);
    g.i32(play_time, -4321);
    let sc = g.child(p.settings_class_addr + 8);
    let songs = g.child(sc + 0xb8);
    g.string(songs + 4, "C:\\osu!\\Songs");
    let show = g.child(sc + 4);
    g.put(show + 0xc, &[1]);
    let skin = g.child(p.skin_data_addr + 7);
    let skin_object = g.child(skin);
    g.string(skin_object + 0x44, "Skin ☆ name");

    let container = g.child(p.rulesets_addr - 0xb);
    let ruleset = g.child(container + 4);
    g.put(ruleset + 0x1d8, &[1]);
    g.i32(ruleset + 0xf8, 7_654_321);
    g.i32(ruleset + 0x118, 1337);

    let beat = g.node();
    g.chain(p.base_addr - 0xc, beat);
    // The checksum's length ends a page: the whole payload needs a second read.
    g.string_at(beat + 0x6c, 0x30ff8, "0123456789abcdef0123456789abcdef");
    // The filename's length straddles a page boundary.
    g.string_at(beat + 0x90, 0x31ffa, "Artist - Title (Mapper) [Expert].osu");
    let long_folder = "folder ".repeat(48);
    for (offset, text) in [
        (0x64, "audio.mp3"),
        (0x68, "bg.JPG"),
        (0x18, "Artist"),
        (0x1c, "Ärtist"),
        (0x24, "Title"),
        (0x28, "タイトル"),
        (0x78, long_folder.as_str()),
        (0x7c, "Mapper"),
        (0xac, ""),
    ] {
        g.string(beat + offset, text);
    }
    for (offset, value) in [(0x2c, 9.5), (0x30, 4.0), (0x34, 5.0), (0x38, 8.25)] {
        g.f32(beat + offset, value);
    }
    g.i32(beat + 0xc8, 123);
    g.i32(beat + 0xcc, 456);
    g.i32(beat + 0xf8, 500);
    g.i32(beat + 0x12c, 4);

    let audio = g.node();
    g.chain(p.get_audio_length_ptr, audio);
    g.f64(audio + 4, 120_000.4);
    let user = g.node();
    g.chain(p.user_profile_ptr, user);
    g.chain(p.raw_login_status_ptr, 0x8000_0001);
    g.string(user + 0x30, "Player");
    g.f64(user + 4, 98.5);
    g.put(user + 0xc, &(9_007_199_254_740_993i64).to_le_bytes());
    g.i32(user + 0x70, 42);
    g.f32(user + 0x74, 100.25);
    g.i32(user + 0x7c, 2345);
    g.i32(user + 0x80, 3);
    g.i32(user + 0x84, 1234);
    g.i32(user + 0x88, 7000);
    g.put(user + 0x8c, &[0xff]);
    g.i32(user + 0x9c, 31);
    g.u32(user + 0xac, 0xffee_ddcc);

    let av = g.child(ruleset + 0x44);
    let array = g.child(av + 0x10);
    g.i32(array + 4, 40);
    for index in 0..40 {
        g.f32(array + 8 + index * 4, index as f32 / 40.0);
    }

    let score = |g: &mut Graph, name: &str, user: usize, base: i16| {
        let score = g.node();
        let mods = g.child(score + 0x1c);
        g.i32(mods + 8, 0x5a5a);
        g.i32(mods + 0xc, 0x5a5a ^ variant.mod_bits);
        g.string(score + 0x28, name);
        g.i32(score + 0x64, variant.gameplay_mode);
        g.i16(score + 0x68, base + 321);
        g.i32(score + 0x78, 123_456);
        for index in 0..7 {
            g.i16(score + 0x88 + 2 * index, base + index as i16 * 11);
        }
        g.u32(score + 0x48, user);
        score
    };
    let played = score(&mut g, "Player", user, 100);
    g.u32(ruleset + 0x38, played);
    let game = g.child(ruleset + 0x64);
    g.u32(game + 0x38, played);
    let hp = g.child(game + 0x40);
    g.f64(hp + 0x14, f64::NAN);
    g.f64(hp + 0x1c, 150.0);
    let accuracy = g.child(game + 0x48);
    g.f64(accuracy + 0xc, 99.75);
    let retries = g.child(p.base_addr - 0x33);
    g.i32(retries + 8, 2);
    g.i32(retries, 1);
    g.i32(retries + 0xc, 77);

    let board = g.child(ruleset + 0x78);
    let addr = g.child(board + 0x24);
    let entry = |g: &mut Graph, name: &str, score: usize, position: i32| {
        let entry = g.node();
        g.string(entry + 8, name);
        g.u32(entry + 0x20, score);
        g.i32(entry + 0x2c, position);
        g.i32(entry + 0x30, 1000 * position);
        g.i32(entry + 0x40, position % 2);
        g.put(entry + 0x4b, &[u8::from(position != 2)]);
        entry
    };
    let current = entry(&mut g, "Player", played, 1);
    let visible = g.child(current + 0x24);
    g.put(visible + 0x20, &[1]);
    g.u32(addr + 0x10, current);
    let other_score = score(&mut g, "Rival", 0, 40);
    let rival = entry(&mut g, "Rival", other_score, 2);
    let absent = entry(&mut g, "Absent", 0, 3);
    let list = g.child(addr + 4);
    g.i32(list + 0xc, 4);
    let items = g.child(list + 4);
    for (index, entry) in [current, rival, absent, rival].into_iter().enumerate() {
        g.u32(items + 8 + 4 * index, entry);
    }

    let config = g.node();
    g.chain(p.configuration_addr, config);
    let entries = g.child(config + 8);
    let settings: [(&str, &str); 8] = [
        ("VolumeUniversal", "number"),
        ("Unlisted", "number"),
        ("ShowStoryboard", "bool"),
        ("ScoreMeter", "enum"),
        ("Skin", "Skin ☆ name"),
        ("LastVersion", ""),
        ("ManiaSpeed", "number"),
        ("DimLevel", "number"),
    ];
    g.i32(config + 0x1c, settings.len() as i32);
    for (index, (name, kind)) in settings.into_iter().enumerate() {
        let slot = entries + 8 + index * 16;
        g.string(slot, name);
        let bind = g.child(slot + 4);
        match kind {
            "number" => g.f64(bind + 4, 10.5 * index as f64),
            "bool" => g.put(bind + 0xc, &[1]),
            "enum" => g.i32(bind + 0xc, -2),
            "" => g.u32(bind + 4, 0),
            text => g.string(bind + 4, text),
        }
    }
    let bindings = g.node();
    g.chain(p.bindings_addr, bindings);
    let slots = g.child(bindings + 8);
    g.i32(bindings + 0x1c, 4);
    for (index, (key, value)) in [(2, 90), (99, 65), (23, 0x20), (3, 0x58)]
        .into_iter()
        .enumerate()
    {
        g.i32(slots + 8 + index * 16, key);
        g.i32(slots + 8 + index * 16 + 12, value);
    }
    g.bytes
}

const PREVIOUS: &str = "0123456789abcdef0123456789abcdef";

fn reader(bytes: Vec<u8>, poisoned: Option<(usize, usize)>) -> (StableMemory, Arc<FaultMemory>) {
    let memory = Arc::new(FaultMemory {
        bytes,
        poisoned,
        log: Mutex::new(Vec::new()),
        calls: Mutex::new(0),
    });
    let process = ProcessHandle::new(memory.clone(), ProcessBitness::X86);
    (StableMemory::new(process, fixture_patterns()), memory)
}

/// One capture-shaped pass; `batched` selects the public readers.
fn capture(reader: &mut StableMemory, variant: &Variant, batched: bool) -> Vec<String> {
    let prev = if variant.checksum_matches {
        PREVIOUS
    } else {
        ""
    };
    let time = 4321.0;
    let first = variant.first_object_time;
    let mut out = Vec::new();
    for _ in 0..2 {
        reader.synchronize_capture_config(false, true);
        if batched {
            out.push(format!("{:?}", reader.global()));
            out.push(format!("{:?}", reader.menu(prev)));
            out.push(format!("{:?}", reader.gameplay(time, first)));
            out.push(format!(
                "{:?}",
                reader.leaderboard(variant.gameplay_mode as i64)
            ));
            out.push(format!("{:?}", reader.audio_velocity_base()));
            out.push(format!("{:?}", reader.user()));
            out.push(format!("{:?}", reader.settings()));
            out.push(format!("{:?}", reader.leaderboard(3)));
        } else {
            out.push(format!("{:?}", reader.global_sequential()));
            out.push(format!("{:?}", reader.menu_sequential(prev)));
            out.push(format!("{:?}", reader.gameplay_sequential(time, first)));
            out.push(format!(
                "{:?}",
                reader.leaderboard_sequential(variant.gameplay_mode as i64)
            ));
            out.push(format!("{:?}", reader.audio_velocity_base_sequential()));
            out.push(format!("{:?}", reader.user_sequential()));
            out.push(format!("{:?}", reader.settings_sequential()));
            out.push(format!("{:?}", reader.leaderboard_sequential(3)));
        }
        out.push(format!(
            "mode={} ruleset={:#x} status={:#x} play_time={:#x}",
            reader.gameplay_mode,
            reader.ruleset_container,
            reader.status_container,
            reader.play_time_container
        ));
    }
    out
}

fn assert_equivalent(variant: Variant) {
    let bytes = graph(&variant);
    let (mut sequential, memory) = reader(bytes.clone(), None);
    let expected = capture(&mut sequential, &variant, false);
    assert!(
        expected
            .iter()
            .all(|line| !line.contains("Err") && !line.contains("NotReady")),
        "healthy graph must decode: {expected:#?}"
    );
    let (mut batched, batched_memory) = reader(bytes.clone(), None);
    assert_eq!(capture(&mut batched, &variant, true), expected);
    // Equivalence alone would also hold if every batched attempt fell back.
    assert!(
        *batched_memory.calls.lock() < *memory.calls.lock() / 3,
        "batched readers must merge independent reads"
    );

    let mut ranges = memory
        .log
        .lock()
        .iter()
        .flat_map(|&(address, len)| {
            [
                (address, address + len),
                (address + len, address + len + 16),
            ]
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    ranges.dedup();
    for range in ranges {
        let (mut sequential, _) = reader(bytes.clone(), Some(range));
        let (mut batched, _) = reader(bytes.clone(), Some(range));
        assert_eq!(
            capture(&mut batched, &variant, true),
            capture(&mut sequential, &variant, false),
            "unreadable range {range:#x?}"
        );
    }
}

#[test]
fn batched_gameplay_capture_matches_sequential_readers_under_every_read_failure() {
    assert_equivalent(Variant {
        status: 2,
        watching: false,
        gameplay_mode: 0,
        mod_bits: 8 | 64,
        checksum_matches: true,
        first_object_time: Some(1000),
    });
}

#[test]
fn batched_menu_capture_matches_sequential_readers_under_every_read_failure() {
    assert_equivalent(Variant {
        status: 0,
        watching: true,
        gameplay_mode: 1,
        mod_bits: 0,
        checksum_matches: false,
        first_object_time: None,
    });
}

#[test]
fn batched_mania_capture_matches_sequential_readers_under_every_read_failure() {
    // Status 3 skips play time; mania with Random reads the seed from the ruleset.
    assert_equivalent(Variant {
        status: 3,
        watching: true,
        gameplay_mode: 3,
        mod_bits: 2_097_152 | 32768,
        checksum_matches: false,
        first_object_time: Some(5000),
    });
}

#[test]
fn global_prefetch_serves_only_the_capture_that_read_it() {
    let variant = Variant {
        status: 2,
        watching: false,
        gameplay_mode: 0,
        mod_bits: 0,
        checksum_matches: false,
        first_object_time: None,
    };
    let (mut stable, memory) = reader(graph(&variant), None);
    let p = fixture_patterns();
    let anchors = [p.configuration_addr, p.rulesets_addr - 0xb];
    let anchor_reads = |memory: &FaultMemory| {
        memory
            .log
            .lock()
            .iter()
            .filter(|(address, _)| anchors.contains(address))
            .count()
    };
    let ready = |read: &str| assert!(read.starts_with("Ok(Ready("), "{read}");
    stable.synchronize_capture_config(false, true);
    ready(&format!("{:?}", stable.global()));
    let after_global = anchor_reads(&memory);
    assert_eq!(after_global, 2, "global reads each holder once");
    ready(&format!("{:?}", stable.settings()));
    ready(&format!("{:?}", stable.audio_velocity_base()));
    assert_eq!(
        anchor_reads(&memory),
        after_global,
        "same-capture readers reuse the prefetch"
    );
    // Settings takes its entry once; the ruleset serves the whole capture.
    ready(&format!("{:?}", stable.settings()));
    ready(&format!("{:?}", stable.audio_velocity_base()));
    assert_eq!(anchor_reads(&memory), after_global + 1);
    // A new capture walks both chains again.
    stable.synchronize_capture_config(false, true);
    ready(&format!("{:?}", stable.settings()));
    ready(&format!("{:?}", stable.audio_velocity_base()));
    assert_eq!(anchor_reads(&memory), after_global + 3);
}

#[test]
fn string_prefix_stays_on_the_length_page_and_long_strings_finish() {
    let mut g = Graph {
        bytes: vec![0; 0x3000],
        next: 0x100,
    };
    let long = "x".repeat(1000);
    g.string_at(0x10, 0x1ff0, "near page end");
    g.string_at(0x14, 0x200, &long);
    // A prefix reaching past its length's page would hit this unreadable byte.
    let (stable, _) = reader(g.bytes, Some((0x2040, 0x2041)));
    let mut slots = [
        StringSlot::new(0x1ff0).unwrap(),
        StringSlot::new(0x200).unwrap(),
    ];
    stable
        .read_batch(slots.iter_mut().filter_map(StringSlot::request))
        .unwrap();
    stable.finish_strings(&mut slots).unwrap();
    assert_eq!(slots[0].decode().unwrap(), "near page end");
    assert_eq!(slots[1].decode().unwrap(), long);
}

/// Memory whose bytes and unreadable range change between reads.
struct ChangingMemory {
    bytes: Mutex<Vec<u8>>,
    poisoned: Mutex<Option<(usize, usize)>>,
}

impl ChangingMemory {
    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let end = address + dst.len();
        let failure = || MemoryError::Read {
            address,
            len: dst.len(),
            message: "unreadable fixture range".into(),
        };
        if self
            .poisoned
            .lock()
            .is_some_and(|(start, stop)| address < stop && start < end)
        {
            return Err(failure());
        }
        let bytes = self.bytes.lock();
        dst.copy_from_slice(bytes.get(address..end).ok_or_else(failure)?);
        Ok(())
    }
}

impl ProcessMemory for ChangingMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.copy(address, dst)
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        reads
            .iter_mut()
            .try_for_each(|read| self.copy(read.address, read.dst))
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.bytes.lock().len(),
        }])
    }
}

#[test]
fn failed_batched_reads_recover_through_the_ruleset_chain_not_the_carried_ruleset() {
    let variant = Variant {
        status: 2,
        watching: false,
        gameplay_mode: 0,
        mod_bits: 0,
        checksum_matches: true,
        first_object_time: Some(1000),
    };
    let original = graph(&variant);
    let p = fixture_patterns();
    let word = |bytes: &[u8], address: usize| {
        u32::from_le_bytes(bytes[address..address + 4].try_into().unwrap()) as usize
    };
    let container = word(&original, p.rulesets_addr - 0xb);
    let ruleset = word(&original, container + 4);
    // The game replaces its ruleset after `global` carried the old one: the container now
    // points at a copy and the old object is unmapped.
    let moved = SIZE - 0x200;
    let mut replaced = original.clone();
    replaced.copy_within(ruleset..ruleset + 0x200, moved);
    replaced[container + 4..container + 8].copy_from_slice(&(moved as u32).to_le_bytes());

    let memory = Arc::new(ChangingMemory {
        bytes: Mutex::new(original),
        poisoned: Mutex::new(None),
    });
    let mut stable = StableMemory::new(
        ProcessHandle::new(memory.clone(), ProcessBitness::X86),
        fixture_patterns(),
    );
    stable.synchronize_capture_config(false, true);
    assert!(matches!(stable.global(), Ok(ReadStatus::Ready(_))));
    assert_eq!(stable.capture.ruleset, Some(ruleset));
    // The readers' take-once carries are spent; later calls start from the ruleset.
    let _ = (
        stable.gameplay(4321.0, Some(1000)),
        stable.leaderboard(0),
        stable.audio_velocity_base(),
    );
    *memory.bytes.lock() = replaced.clone();
    *memory.poisoned.lock() = Some((ruleset, ruleset + 0x200));

    let (mut reference, _) = reader(replaced, Some((ruleset, ruleset + 0x200)));
    let expected = (
        format!("{:?}", reference.gameplay_sequential(4321.0, Some(1000))),
        format!("{:?}", reference.leaderboard_sequential(0)),
        format!("{:?}", reference.audio_velocity_base_sequential()),
    );
    assert!(expected.0.starts_with("Ok(Ready("), "{expected:?}");
    let actual = (
        format!("{:?}", stable.gameplay(4321.0, Some(1000))),
        format!("{:?}", stable.leaderboard(0)),
        format!("{:?}", stable.audio_velocity_base()),
    );
    assert_eq!(actual, expected);
    assert_eq!(stable.capture.ruleset, None);
}

#[test]
fn remembered_mod_decodes_equal_fresh_decodes_past_the_cache_size() {
    // More distinct bitfields than the cache holds, revisited in an evicting order.
    let bits = [
        0i64,
        8 | 64,
        16,
        1 << 29,
        2_097_152 | 32768,
        576,
        72,
        0,
        16,
        576,
    ];
    for _ in 0..3 {
        for bits in bits {
            assert_eq!(mods(bits), super::helpers::decode_mods(bits), "bits {bits}");
        }
    }
}

#[test]
fn ruleset_recovery_drops_every_link_carried_from_the_old_ruleset() {
    let variant = Variant {
        status: 2,
        watching: false,
        gameplay_mode: 0,
        mod_bits: 0,
        checksum_matches: true,
        first_object_time: Some(1000),
    };
    let original = graph(&variant);
    let p = fixture_patterns();
    let word = |bytes: &[u8], address: usize| {
        u32::from_le_bytes(bytes[address..address + 4].try_into().unwrap()) as usize
    };
    let container = word(&original, p.rulesets_addr - 0xb);
    let ruleset = word(&original, container + 4);
    let game = word(&original, ruleset + 0x64);
    let health = word(&original, game + 0x40);
    // After `global`, the game swaps in a ruleset without a leaderboard while the old one,
    // and its board, stay readable; the shared health object becomes unreadable, so the
    // batched gameplay read fails and recovers through the ruleset chain.
    let moved = SIZE - 0x200;
    let mut replaced = original.clone();
    replaced.copy_within(ruleset..ruleset + 0x200, moved);
    replaced[moved + 0x78..moved + 0x7c].fill(0);
    replaced[container + 4..container + 8].copy_from_slice(&(moved as u32).to_le_bytes());
    let poisoned = Some((health, health + 0x100));

    let memory = Arc::new(ChangingMemory {
        bytes: Mutex::new(original),
        poisoned: Mutex::new(None),
    });
    let mut stable = StableMemory::new(
        ProcessHandle::new(memory.clone(), ProcessBitness::X86),
        fixture_patterns(),
    );
    stable.synchronize_capture_config(false, true);
    assert!(matches!(stable.global(), Ok(ReadStatus::Ready(_))));
    *memory.bytes.lock() = replaced.clone();
    *memory.poisoned.lock() = poisoned;

    let (mut reference, _) = reader(replaced, poisoned);
    let expected = (
        format!("{:?}", reference.gameplay_sequential(4321.0, Some(1000))),
        format!("{:?}", reference.leaderboard_sequential(0)),
        format!("{:?}", reference.audio_velocity_base_sequential()),
    );
    assert_eq!(expected.1, "Ok(Ready((false, None, [])))");
    let actual = (
        format!("{:?}", stable.gameplay(4321.0, Some(1000))),
        format!("{:?}", stable.leaderboard(0)),
        format!("{:?}", stable.audio_velocity_base()),
    );
    assert_eq!(actual, expected);
}

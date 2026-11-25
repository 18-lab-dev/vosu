//! The two prototype read-plan shapes of `docs/v2/brokered-reads.md`, shared
//! verbatim by `benches/brokered_reads_bench.rs` (every variant submits these
//! exact plans) and `examples/brokered_reads_target.rs` (which evaluates them
//! over its own memory for the independent expected result hash).
//!
//! The shapes are frozen by the M5 D17 contract: precise is 8 stages, 43
//! reads, 358 bytes; regular is 5 stages, 8 reads, 116 bytes.

/// Where a read's base address comes from.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// The plan root (the target's reported base address).
    Root,
    /// The pointer resolved by `(stage, index)` of an earlier stage.
    Prev(usize, usize),
}

/// One read: `len` bytes at `base(source) + offset`; `as_pointer` reads
/// resolve to a little-endian 8-byte pointer later stages may name.
#[derive(Clone, Copy, Debug)]
pub struct ReadOp {
    pub source: Source,
    pub offset: i64,
    pub len: u32,
    pub as_pointer: bool,
}

impl ReadOp {
    pub const fn new(source: Source, offset: i64, len: u32, as_pointer: bool) -> Self {
        Self {
            source,
            offset,
            len,
            as_pointer,
        }
    }
}

/// Dependent stages of independent reads.
pub struct Plan {
    pub stages: Vec<Vec<ReadOp>>,
}

impl Plan {
    pub fn reads(&self) -> usize {
        self.stages.iter().map(Vec::len).sum()
    }

    pub fn bytes(&self) -> usize {
        self.stages.iter().flatten().map(|op| op.len as usize).sum()
    }
}

/// Models lazer `key_overlay`/`global_precise`'s pointer chain (see
/// `benches/brokered_reads_bench.rs`'s module doc).
pub fn precise_plan() -> Plan {
    let overlay = ReadOp::new(Source::Root, 0, 8, true);
    let controller = ReadOp::new(Source::Prev(0, 0), 0, 8, true);
    let trigger_list = ReadOp::new(Source::Prev(1, 0), 0, 8, true);
    let list_count = ReadOp::new(Source::Prev(2, 0), 0x10, 4, false);
    let trigger_array = ReadOp::new(Source::Prev(2, 0), 8, 8, true);
    let per_key_pointer = (0..18)
        .map(|index| ReadOp::new(Source::Prev(3, 1), 0x10 + i64::from(index) * 8, 8, true))
        .collect::<Vec<_>>();
    let per_key_fields = (0..18)
        .map(|index| ReadOp::new(Source::Prev(4, index), 0x14, 9, false))
        .collect::<Vec<_>>();
    let clock_source = ReadOp::new(Source::Root, 8, 8, true);
    let clock_time = ReadOp::new(Source::Prev(6, 0), 0, 8, false);

    Plan {
        stages: vec![
            vec![overlay],
            vec![controller],
            vec![trigger_list],
            vec![list_count, trigger_array],
            per_key_pointer,
            per_key_fields,
            vec![clock_source],
            vec![clock_time],
        ],
    }
}

/// Models `capture_regular`'s fan-out across independent decoder calls.
pub fn regular_plan() -> Plan {
    let beatmap = ReadOp::new(Source::Root, 0x40, 8, true);
    let settings = ReadOp::new(Source::Root, 0x48, 8, true);
    let menu = ReadOp::new(Source::Root, 0x50, 8, true);
    let mp3 = ReadOp::new(Source::Root, 0x58, 8, true);
    let beatmap_fields = ReadOp::new(Source::Prev(0, 0), 0x20, 8, false);
    let settings_fields = ReadOp::new(Source::Prev(0, 1), 0x40, 64, false);
    let menu_field = ReadOp::new(Source::Prev(0, 2), 0x10, 8, false);
    let mp3_field = ReadOp::new(Source::Prev(0, 3), 8, 4, false);

    Plan {
        stages: vec![
            vec![beatmap, settings, menu, mp3],
            vec![beatmap_fields],
            vec![settings_fields],
            vec![menu_field],
            vec![mp3_field],
        ],
    }
}

/// Both shapes by their metric name, in run order.
pub fn shapes() -> [(&'static str, Plan); 2] {
    [("precise", precise_plan()), ("regular", regular_plan())]
}

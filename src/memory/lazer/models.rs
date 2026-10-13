use super::*;

/// Immutable scan input supplied by the lifecycle/offset loader.
#[derive(Debug, Clone)]
pub struct LazerPattern {
    pub scaling_container_target_draw_size: Pattern,
    pub offset: isize,
}

impl LazerPattern {
    /// Pinned production scaling-container signature from the source daemon.
    #[must_use]
    pub fn source_default() -> Self {
        Self {
            scaling_container_target_draw_size: Pattern::parse(
                "01 01 00 00 00 00 80 44 00 00 40 44",
                false,
            )
            .expect("pinned lazer signature is valid"),
            offset: 0,
        }
    }
}

pub(super) const OSU_MOD_CATEGORIES: [&[&str]; 6] = [
    &["EZ", "NF", "HT", "DC"],
    &[
        "HR", "SD", "PF", "DT", "NC", "HD", "TC", "FL", "BL", "ST", "AC",
    ],
    &["TP", "DA", "CL", "RD", "MR", "AL", "SG"],
    &["AT", "CN", "RX", "AP", "SO"],
    &[
        "TR", "WG", "SI", "GR", "DF", "WU", "WD", "BR", "AD", "MU", "NS", "MG", "RP", "AS", "FR",
        "BU", "SY", "DP", "BM",
    ],
    &["TD", "SV2"],
];
pub(super) const OSU_OLD_MOD_CATEGORIES: [&[&str]; 6] = [
    &["EZ", "NF", "HT", "DC"],
    &["HR", "SD", "PF", "DT", "NC", "HD", "FL", "BL", "ST", "AC"],
    &["TP", "DA", "CL", "RD", "MR", "AL", "SG"],
    &["AT", "CN", "RX", "AP", "SO"],
    &[
        "TR", "WG", "SI", "GR", "DF", "WU", "WD", "TC", "BR", "AD", "MU", "NS", "MG", "RP", "AS",
        "FR", "BU", "SY", "DP", "BM",
    ],
    &["TD", "SV2"],
];
pub(super) const TAIKO_MOD_CATEGORIES: [&[&str]; 6] = [
    &["EZ", "NF", "HT", "DC", "SR"],
    &["HR", "SD", "PF", "DT", "NC", "HD", "FL", "AC"],
    &["RD", "DA", "CL", "SW", "SG", "CS"],
    &["AT", "CN", "RX"],
    &["WU", "WD", "MU", "AS"],
    &["TD", "SV2"],
];
pub(super) const FRUITS_MOD_CATEGORIES: [&[&str]; 6] = [
    &["EZ", "NF", "HT", "DC"],
    &["HR", "SD", "PF", "DT", "NC", "HD", "FL", "AC"],
    &["DA", "CL", "MR"],
    &["AT", "CN", "RX"],
    &["WU", "WD", "FF", "MU", "NS", "MF"],
    &["TD", "SV2"],
];
pub(super) const MANIA_MOD_CATEGORIES: [&[&str]; 6] = [
    &["EZ", "NF", "HT", "DC", "NR"],
    &["HR", "SD", "PF", "DT", "NC", "FI", "HD", "CO", "FL", "AC"],
    &[
        "RD", "DS", "MR", "DA", "CL", "IN", "CS", "HO", "1K", "2K", "3K", "4K", "5K", "6K", "7K",
        "8K", "9K", "10K",
    ],
    &["AT", "CN"],
    &["WU", "WD", "MU", "AS"],
    &["TD", "SV2"],
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum HitErrorEvent {
    Timing { value: f64, object: usize },
    ManiaHoldAggregate { tail: usize },
}

#[derive(Clone, Copy)]
pub(super) struct PrecisePlayerCache {
    pub(super) screen: usize,
    pub(super) player: usize,
    pub(super) key_used: bool,
    pub(super) hit_used: bool,
}

/// Source-faithful osu!lazer managed-memory decoder.
pub struct LazerMemory {
    pub(super) process: ProcessHandle,
    pub(super) offsets: Arc<LazerOffsets>,
    pub(super) pattern: LazerPattern,
    pub(super) game_base_address: RwLock<usize>,
    pub(super) validated_game_base: AtomicUsize,
    /// Per-capture game roots (see `roots.rs`); `UNCACHED_POINTER` until read.
    pub(super) capture_roots: [AtomicUsize; GAME_ROOTS],
    pub(super) capture_roots_loaded: AtomicBool,
    /// Per-capture player classification of `capture_player_screen` (0: none cached).
    pub(super) capture_player_screen: AtomicUsize,
    pub(super) capture_player: AtomicUsize,
    /// Skin name from the last batched configuration read, for `settings` (see
    /// `configuration.rs`); `None` when that read was sequential.
    pub(super) batched_skin: Option<Option<String>>,
    /// Per-capture snapshot of the current screen's field span (see `roots.rs`).
    pub(super) screen_span: (usize, usize),
    pub(super) capture_screen: AtomicUsize,
    pub(super) capture_screen_pending: AtomicBool,
    pub(super) capture_screen_words: [AtomicU64; SCREEN_SPAN_WORDS],
    /// Pointers the batched `global` reads earlier in the capture for later readers:
    /// `multiplayer_room()`, the beatmap track, the local API user, and the ranked-play state;
    /// `UNCACHED_POINTER` until then (see `roots.rs`).
    pub(super) capture_multiplayer_room: AtomicUsize,
    pub(super) capture_track: AtomicUsize,
    pub(super) capture_user: AtomicUsize,
    pub(super) capture_ranked_state: AtomicUsize,
    /// The personal leaderboard entry the batched `gameplay` decoded this capture, keyed by
    /// player screen, for `leaderboard`.
    pub(super) captured_personal: Option<(usize, LeaderboardPlayer)>,
    /// The beatmap head, gamemode, and (when the player path read it) MD5 string the batched
    /// `global` read this capture, for `menu`; and the track length, for `mp3_length`.
    pub(super) captured_beatmap: Option<(BeatmapHead, i64, Option<String>)>,
    pub(super) captured_track_length: Option<f64>,
    pub(super) current_screen: usize,
    pub(super) precise_player: Option<PrecisePlayerCache>,
    pub(super) selected_gamemode: i64,
    pub(super) scoring_display_mode: i32,
    pub(super) watching_replay: bool,
    pub(super) spectating: bool,
    pub(super) player_loading: bool,
    pub(super) status: i64,
    pub(super) menu_mods: CalculateMods,
    pub(super) replay_settings_overlay: bool,
    pub(super) show_interface: bool,
    pub(super) leaderboard_visible: bool,
    pub(super) mod_mappings: BTreeMap<(i64, usize), &'static str>,
    pub(super) mapped_modes: [bool; 4],
    /// Reused destination for batched configuration dictionary entries.
    pub(super) config_scratch: Vec<u8>,
}

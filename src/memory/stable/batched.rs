//! Batched osu!stable regular readers.
//!
//! Each reader here walks its pointer graph one dependency level at a time, issuing every
//! independent read of a level as one backend batch, and reads whole object spans instead of
//! separate fields. It decodes exactly the values its sequential counterpart in `readers.rs`
//! publishes. Any batched error discards the attempt and reruns that sequential reader, which
//! therefore alone decides error precedence, `NotReady` ordering, and cache invalidation on
//! failure. A batch may touch more memory than the sequential reader (object spans,
//! page-bounded string prefixes, fields a short-circuit would skip); such extra reads can only
//! fail into the fallback, never change a published value.
use super::readers::{MAX_LEADERBOARD_NAME_BYTES, MAX_LEADERBOARD_PLAYERS};
use super::*;

/// Readability is page-granular: a range is readable exactly when every page it touches is.
const PAGE: usize = 4096;
/// UTF-16 bytes requested together with a string's length, so most strings take one read.
const STRING_PREFETCH: usize = 256;
/// Requests packed on the stack before a batch spills to the heap.
const PACKED_REQUESTS: usize = 24;

/// Bytes `[base + START, base + START + LEN)` of one object, decoded by the same absolute
/// field offsets the sequential readers use.
pub(super) struct Span<const START: usize, const LEN: usize> {
    bytes: [u8; LEN],
}

/// One pointer-sized word read directly at an address.
type Word = Span<0, 4>;
/// A stable score object, as used by gameplay, result, and leaderboard entries.
type ScoreSpan = Span<0x1c, 0x7a>;
/// A stable leaderboard entry object.
type PlayerSpan = Span<8, 0x44>;
/// The two mod bitfields of a score's mod container.
type ModSpan = Span<8, 8>;
/// A stable ruleset from its audio-velocity holder (+0x44) through its score (+0xf8):
/// the game object at +0x64 and the leaderboard at +0x78 lie between.
pub(super) type RulesetSpan = Span<0x44, 0xb8>;
/// A stable game object's score (+0x38), health (+0x40), and accuracy (+0x48) pointers.
pub(super) type GameSpan = Span<0x38, 0x14>;

/// Pointers later readers of a regular capture walk first, read by `global_batched` in its
/// own batches. `global_batched` and `synchronize_capture_config` reset them, so a value
/// never outlives the capture that read it, and each reader except the shared ruleset takes
/// its entry once. Absent entries leave the reader to its own reads.
///
/// The chains of `user`, `menu`, and `mp3_length`, and the audio-velocity array pointer,
/// would fit too; they are left out because `tests/autoresearch_regular_reader_compat.rs`
/// requires more than 20 native calls per successful capture, and the stable menu capture
/// makes 21.
#[derive(Default)]
pub(super) struct CapturePrefetch {
    /// What the sequential `ruleset` returns.
    pub(super) ruleset: Option<usize>,
    /// Gameplay retries, at +8 in the mode container.
    pub(super) retries: Option<i64>,
    /// The configuration and bindings dictionaries.
    pub(super) settings: Option<(usize, usize)>,
    /// The ruleset's fields from the audio-velocity holder through the score, and the game
    /// object's score, health, and accuracy pointers when it is non-null.
    pub(super) gameplay: Option<(RulesetSpan, Option<GameSpan>)>,
    /// The leaderboard base and, when it is non-null, its board.
    pub(super) leaderboard: Option<(usize, usize)>,
    /// The audio-velocity holder.
    pub(super) audio: Option<usize>,
}

impl<const START: usize, const LEN: usize> Span<START, LEN> {
    pub(super) const fn new() -> Self {
        Self { bytes: [0; LEN] }
    }
    pub(super) fn request(&mut self, base: usize) -> Result<ReadRequest<'_>, MemoryError> {
        Ok(ReadRequest::new(
            checked_address_add(base, START)?,
            &mut self.bytes,
        ))
    }
    fn field<const WIDTH: usize>(&self, offset: usize) -> [u8; WIDTH] {
        let at = offset - START;
        self.bytes[at..at + WIDTH].try_into().unwrap()
    }
    pub(super) fn ptr(&self, offset: usize) -> usize {
        u32::from_le_bytes(self.field(offset)) as usize
    }
    pub(super) fn u32(&self, offset: usize) -> i64 {
        u32::from_le_bytes(self.field(offset)) as i64
    }
    pub(super) fn i32(&self, offset: usize) -> i64 {
        i32::from_le_bytes(self.field(offset)) as i64
    }
    pub(super) fn i16(&self, offset: usize) -> i64 {
        i16::from_le_bytes(self.field(offset)) as i64
    }
    pub(super) fn u8(&self, offset: usize) -> u8 {
        self.bytes[offset - START]
    }
    pub(super) fn i64(&self, offset: usize) -> i64 {
        i64::from_le_bytes(self.field(offset))
    }
    pub(super) fn f32(&self, offset: usize) -> f32 {
        f32::from_le_bytes(self.field(offset))
    }
    pub(super) fn f64(&self, offset: usize) -> f64 {
        f64::from_le_bytes(self.field(offset))
    }
}

/// One stable string read: its length and a page-bounded prefix of its UTF-16 payload come
/// from one request, and only a longer string needs a second read for the remainder.
///
/// The prefix never leaves the page holding the length field, so it is readable exactly when
/// the length the sequential reader reads first is.
pub(super) struct StringSlot {
    address: usize,
    /// Bytes requested from `address + 4`; zero for a null string.
    len: usize,
    prefix: [u8; 4 + STRING_PREFETCH],
    /// The complete UTF-16 payload of a string longer than its prefix.
    long: Vec<u8>,
}

impl StringSlot {
    pub(super) fn new(address: usize) -> Result<Self, MemoryError> {
        let mut len = 0;
        if address != 0 {
            let start = checked_address_add(address, 4)?;
            let length_end = checked_address_add(start, 4)?;
            let page_end = ((length_end - 1) | (PAGE - 1)).saturating_add(1);
            len = page_end.min(start.saturating_add(4 + STRING_PREFETCH)) - start;
        }
        Ok(Self {
            address,
            len,
            prefix: [0; 4 + STRING_PREFETCH],
            long: Vec::new(),
        })
    }
    pub(super) fn request(&mut self) -> Option<ReadRequest<'_>> {
        (self.len != 0).then(|| ReadRequest::new(self.address + 4, &mut self.prefix[..self.len]))
    }
    fn byte_len(&self) -> Result<usize, MemoryError> {
        let length = i32::from_le_bytes(self.prefix[..4].try_into().unwrap());
        if !(0..4096).contains(&length) {
            return Err(MemoryError::Corrupt {
                context: "stable string",
                address: self.address,
                message: "invalid UTF-16 length",
            });
        }
        Ok(length as usize * 2)
    }
    /// The read completing a string longer than its prefix. An invalid length requests
    /// nothing; [`Self::decode`] reports it.
    fn remainder_request(&mut self) -> Option<ReadRequest<'_>> {
        if self.len == 0 {
            return None;
        }
        let have = self.len - 4;
        let need = self.byte_len().ok()?;
        if need <= have {
            return None;
        }
        self.long = vec![0; need];
        self.long[..have].copy_from_slice(&self.prefix[4..self.len]);
        Some(ReadRequest::new(
            self.address + 4 + self.len,
            &mut self.long[have..],
        ))
    }
    /// Decodes like `StableMemory::string`: null is empty, other failures are errors.
    pub(super) fn decode(&self) -> Result<String, MemoryError> {
        if self.len == 0 {
            return Ok(String::new());
        }
        let need = self.byte_len()?;
        let bytes = if need <= self.len - 4 {
            &self.prefix[4..4 + need]
        } else if self.long.len() == need {
            &self.long[..]
        } else {
            return Err(MemoryError::Corrupt {
                context: "stable string",
                address: self.address,
                message: "string remainder was not read",
            });
        };
        std::char::decode_utf16(
            bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]])),
        )
        .collect::<Result<String, _>>()
        .map_err(|_| MemoryError::InvalidUtf16 {
            address: self.address,
        })
    }
}

pub(super) fn string_slots<const N: usize>(
    addresses: [usize; N],
) -> Result<[StringSlot; N], MemoryError> {
    let mut error = None;
    let slots = std::array::from_fn(|index| {
        StringSlot::new(addresses[index]).unwrap_or_else(|failure| {
            error.get_or_insert(failure);
            StringSlot::new(0).unwrap()
        })
    });
    error.map_or(Ok(slots), Err)
}

fn decode_all<const N: usize>(slots: &[StringSlot; N]) -> Result<[String; N], MemoryError> {
    let mut error = None;
    let values = std::array::from_fn(|index| {
        slots[index].decode().unwrap_or_else(|failure| {
            error.get_or_insert(failure);
            String::new()
        })
    });
    error.map_or(Ok(values), Err)
}

fn player(
    mode: i64,
    fields: &PlayerSpan,
    score: &ScoreSpan,
    mod_fields: &ModSpan,
    user_id: i64,
    name: String,
) -> LeaderboardPlayer {
    let m = mods(mod_fields.i32(8) ^ mod_fields.i32(0xc));
    let s = legacy_stats(
        mode,
        score.i16(0x8a),
        score.i16(0x88),
        score.i16(0x8c),
        score.i16(0x92),
        0,
        0,
    );
    LeaderboardPlayer {
        user_id,
        name,
        score: fields.i32(0x30) as f64,
        combo: score.i16(0x94),
        accuracy: accuracy(mode, &s, &m),
        max_combo: score.i16(0x68),
        mods: m,
        statistics: s,
        team: fields.i32(0x40),
        position: fields.i32(0x2c),
        is_passing: fields.u8(0x4b) != 0,
    }
}

impl StableMemory {
    /// Issues independent reads as one backend batch; nothing is read for no requests.
    pub(super) fn read_batch<'a>(
        &self,
        requests: impl IntoIterator<Item = ReadRequest<'a>>,
    ) -> Result<(), MemoryError> {
        let mut requests = requests.into_iter();
        let mut count = 0;
        let mut packed: [ReadRequest<'a>; PACKED_REQUESTS] = std::array::from_fn(|_| {
            requests.next().map_or_else(
                || ReadRequest::new(0, &mut []),
                |request| {
                    count += 1;
                    request
                },
            )
        });
        let mut spilled = requests.collect::<Vec<_>>();
        if spilled.is_empty() {
            return self.read_requests(&mut packed[..count]);
        }
        spilled.splice(0..0, packed.into_iter().take(count));
        self.read_requests(&mut spilled)
    }

    /// Issues collected requests as one backend batch; nothing is read for none.
    fn read_requests(&self, requests: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        match requests {
            [] => Ok(()),
            [request] => self.process.read_exact(request.address, request.dst),
            requests => self.process.read_many(requests),
        }
    }

    /// Reads the remainders of long strings whose prefixes were read already.
    pub(super) fn finish_strings(&self, slots: &mut [StringSlot]) -> Result<(), MemoryError> {
        self.read_batch(slots.iter_mut().filter_map(StringSlot::remainder_request))
    }

    fn strings<const N: usize>(&self, addresses: [usize; N]) -> Result<[String; N], MemoryError> {
        let mut slots = string_slots(addresses)?;
        self.read_batch(slots.iter_mut().filter_map(StringSlot::request))?;
        self.finish_strings(&mut slots)?;
        decode_all(&slots)
    }

    pub(super) fn user_batched(&self) -> MemoryRead<User> {
        let (mut profile, mut login) = (Word::new(), Word::new());
        self.read_batch([
            profile.request(self.patterns.user_profile_ptr)?,
            login.request(self.patterns.raw_login_status_ptr)?,
        ])?;
        let (mut profile_object, mut login_status) = (Word::new(), Word::new());
        self.read_batch([
            profile_object.request(profile.ptr(0))?,
            login_status.request(login.ptr(0))?,
        ])?;
        let (b, login_status) = (profile_object.ptr(0), login_status.ptr(0));
        if b == 0 {
            return self.not_ready();
        }
        let mut fields = Span::<4, 0xac>::new();
        self.read_batch([fields.request(b)?])?;
        let [name] = self.strings([fields.ptr(0x30)])?;
        Ok(ReadStatus::Ready(User {
            name,
            accuracy: fields.f64(4),
            ranked_score: js_long(fields.i64(0xc)),
            id: fields.i32(0x70),
            level: fields.f32(0x74) as f64,
            play_count: fields.i32(0x7c),
            play_mode: fields.i32(0x80),
            rank: fields.i32(0x84),
            country_code: fields.i32(0x9c),
            performance_points: fields.i32(0x88) as f64,
            raw_bancho_status: fields.u8(0x8c) as i8 as i64,
            background_colour: fields.u32(0xac),
            raw_login_status: login_status as i64,
            matchmaking: None,
        }))
    }

    pub(super) fn menu_batched(&self, prev: &str) -> MemoryRead<Menu> {
        let base = self.patterns.base_addr;
        let beat_anchor = base
            .checked_sub(0xc)
            .ok_or(MemoryError::AddressOverflow { base, offset: 0xc })?;
        let mode_anchor = base
            .checked_sub(0x33)
            .ok_or(MemoryError::AddressOverflow { base, offset: 0x33 })?;
        let (mut beat_holder, mut mode_holder) = (Word::new(), Word::new());
        self.read_batch([
            beat_holder.request(beat_anchor)?,
            mode_holder.request(mode_anchor)?,
        ])?;
        // The mode container also holds the play count at +0xc.
        let (mut beat_word, mut mode_object) = (Word::new(), Span::<0, 0x10>::new());
        self.read_batch([
            beat_word.request(beat_holder.ptr(0))?,
            mode_object.request(mode_holder.ptr(0))?,
        ])?;
        let beat = beat_word.ptr(0);
        if beat == 0 {
            return self.not_ready();
        }
        let mode = mode_object.ptr(0) as i64;
        let mut fields = Span::<0x18, 0x118>::new();
        self.read_batch([fields.request(beat)?])?;
        let ranked = stable_ranked_status(fields.i32(0x12c));
        let [checksum, filename] = self.strings([fields.ptr(0x6c), fields.ptr(0x90)])?;
        if checksum == prev || !filename.ends_with(".osu") {
            return Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
                gamemode: mode,
                ranked_status: ranked,
            })));
        }
        let [
            audio,
            bg,
            artist,
            artist_original,
            title,
            title_original,
            folder,
            creator,
            difficulty,
        ] = self.strings(
            [0x64, 0x68, 0x18, 0x1c, 0x24, 0x28, 0x78, 0x7c, 0xac].map(|offset| fields.ptr(offset)),
        )?;
        Ok(ReadStatus::Ready(Menu::Full(MenuFull {
            gamemode: mode,
            checksum,
            filename,
            plays: mode_object.i32(0xc),
            artist,
            artist_original,
            title,
            title_original,
            ar: fields.f32(0x2c) as f64,
            cs: fields.f32(0x30) as f64,
            hp: fields.f32(0x34) as f64,
            od: fields.f32(0x38) as f64,
            audio_file_mimetype: mime(&audio),
            audio_filename: audio,
            background_file_mimetype: mime(&bg),
            background_filename: bg,
            folder,
            creator,
            difficulty,
            map_id: fields.i32(0xc8),
            set_id: fields.i32(0xcc),
            ranked_status: ranked,
            object_count: fields.i32(0xf8),
        })))
    }

    pub(super) fn global_batched(&mut self) -> MemoryRead<Global> {
        self.capture = CapturePrefetch::default();
        let patterns = &self.patterns;
        let ruleset_anchor =
            patterns
                .rulesets_addr
                .checked_sub(0xb)
                .ok_or(MemoryError::AddressOverflow {
                    base: patterns.rulesets_addr,
                    offset: 0xb,
                })?;
        let base = patterns.base_addr;
        let mode_anchor = base
            .checked_sub(0x33)
            .ok_or(MemoryError::AddressOverflow { base, offset: 0x33 })?;
        let (mut menu_mods, mut chat, mut game_time) = (Word::new(), Word::new(), Word::new());
        let (mut status_holder, mut rulesets) = (Word::new(), Word::new());
        let mut watching_holder = Span::<0x46, 4>::new();
        let mut play_time_holder = Span::<5, 4>::new();
        let mut settings_holder = Span::<8, 4>::new();
        let mut skin_holder = Span::<7, 4>::new();
        // Holders of the pointers `settings` and gameplay retries walk first; see
        // `CapturePrefetch`.
        let mut mode_holder = Word::new();
        let (mut config_holder, mut bindings_holder) = (Word::new(), Word::new());
        self.read_batch([
            menu_mods.request(patterns.menu_mods_ptr)?,
            chat.request(patterns.chat_checker_ptr)?,
            watching_holder.request(patterns.can_run_slowly_addr)?,
            game_time.request(patterns.game_time_ptr)?,
            status_holder.request(patterns.status_ptr)?,
            play_time_holder.request(patterns.play_time_addr)?,
            settings_holder.request(patterns.settings_class_addr)?,
            skin_holder.request(patterns.skin_data_addr)?,
            rulesets.request(ruleset_anchor)?,
            mode_holder.request(mode_anchor)?,
            config_holder.request(patterns.configuration_addr)?,
            bindings_holder.request(patterns.bindings_addr)?,
        ])?;
        let [mode_holder, config_holder, bindings_holder] =
            [mode_holder, config_holder, bindings_holder].map(|holder| holder.ptr(0));
        let status_container = status_holder.ptr(0);
        let play_time_container = play_time_holder.ptr(5);
        let sc = settings_holder.ptr(8);
        let skin_address = skin_holder.ptr(7);
        let container = rulesets.ptr(0);

        let (mut menu_bits, mut game_time_value) = (Word::new(), Word::new());
        let (mut status_word, mut play_time_word, mut skin_object) =
            (Word::new(), Word::new(), Word::new());
        let (mut chat_value, mut watching_value) = (Span::<0, 1>::new(), Span::<0, 1>::new());
        let mut songs_holder = Span::<0xb8, 4>::new();
        let mut show_holder = Span::<4, 4>::new();
        let mut ruleset_word = Span::<4, 4>::new();
        let mut retries = Span::<8, 4>::new();
        let (mut config_word, mut bindings_word) = (Word::new(), Word::new());
        // Prefetches follow only non-null holders: a null one is left to its reader.
        let settings = config_holder != 0 && bindings_holder != 0;
        self.read_batch(
            [
                Some(menu_bits.request(menu_mods.ptr(0))?),
                Some(chat_value.request(chat.ptr(0))?),
                Some(watching_value.request(watching_holder.ptr(0x46))?),
                Some(game_time_value.request(game_time.ptr(0))?),
                Some(status_word.request(status_container)?),
                Some(play_time_word.request(play_time_container)?),
                Some(songs_holder.request(sc)?),
                Some(show_holder.request(sc)?),
                (skin_address != 0)
                    .then(|| skin_object.request(skin_address))
                    .transpose()?,
                (container != 0)
                    .then(|| ruleset_word.request(container))
                    .transpose()?,
                (mode_holder != 0)
                    .then(|| retries.request(mode_holder))
                    .transpose()?,
                settings
                    .then(|| config_word.request(config_holder))
                    .transpose()?,
                settings
                    .then(|| bindings_word.request(bindings_holder))
                    .transpose()?,
            ]
            .into_iter()
            .flatten(),
        )?;
        let status = status_word.u32(0);
        let watching = watching_value.u8(0) == 1;
        let ruleset = if !watching {
            0
        } else if container != 0 {
            ruleset_word.ptr(4)
        } else {
            self.ptr(self.add(container, 4)?)?
        };
        let skin_base = if skin_address == 0 {
            0
        } else {
            skin_object.ptr(0)
        };

        // The prefetched ruleset's gameplay, leaderboard, and audio holders; see
        // `CapturePrefetch`.
        let prefetched_ruleset = if container != 0 {
            ruleset_word.ptr(4)
        } else {
            0
        };
        let mut songs_object = Span::<4, 4>::new();
        let mut show_value = Span::<0xc, 1>::new();
        let mut skin_name = Span::<0x44, 4>::new();
        let mut hidden_value = Span::<0x1d8, 1>::new();
        let mut ruleset_fields = RulesetSpan::new();
        self.read_batch(
            [
                Some(songs_object.request(songs_holder.ptr(0xb8))?),
                Some(show_value.request(show_holder.ptr(4))?),
                (skin_base != 0)
                    .then(|| skin_name.request(skin_base))
                    .transpose()?,
                (ruleset != 0)
                    .then(|| hidden_value.request(ruleset))
                    .transpose()?,
                (prefetched_ruleset != 0)
                    .then(|| ruleset_fields.request(prefetched_ruleset))
                    .transpose()?,
            ]
            .into_iter()
            .flatten(),
        )?;
        let skin_string = if skin_base == 0 {
            0
        } else {
            skin_name.ptr(0x44)
        };
        let (game, board) = if prefetched_ruleset == 0 {
            (0, 0)
        } else {
            (ruleset_fields.ptr(0x64), ruleset_fields.ptr(0x78))
        };
        let mut game_fields = GameSpan::new();
        let mut board_fields = Span::<0x24, 4>::new();
        let mut texts = string_slots([songs_object.ptr(4), skin_string])?;
        let prefetches = [
            (game != 0).then(|| game_fields.request(game)).transpose()?,
            (board != 0)
                .then(|| board_fields.request(board))
                .transpose()?,
        ];
        self.read_batch(
            texts
                .iter_mut()
                .filter_map(StringSlot::request)
                .chain(prefetches.into_iter().flatten()),
        )?;
        self.finish_strings(&mut texts)?;
        let [songs, skin] = decode_all(&texts)?;

        self.status_container = status_container;
        let play_time = if status == 3 {
            0.0
        } else {
            self.play_time_container = play_time_container;
            play_time_word.i32(0) as f64
        };
        // A ruleset container the sequential `ruleset` would accept serves this capture.
        if container != 0 {
            self.ruleset_container = container;
        }
        self.capture = CapturePrefetch {
            ruleset: (container != 0).then(|| ruleset_word.ptr(4)),
            retries: (mode_holder != 0).then(|| retries.i32(8)),
            settings: settings.then(|| (config_word.ptr(0), bindings_word.ptr(0))),
            audio: (prefetched_ruleset != 0).then(|| ruleset_fields.ptr(0x44)),
            leaderboard: (prefetched_ruleset != 0).then(|| {
                (
                    board,
                    if board == 0 {
                        0
                    } else {
                        board_fields.ptr(0x24)
                    },
                )
            }),
            gameplay: (prefetched_ruleset != 0)
                .then(|| (ruleset_fields, (game != 0).then_some(game_fields))),
        };
        Ok(ReadStatus::Ready(Global {
            is_watching_replay: watching,
            is_replay_ui_hidden: ruleset != 0 && hidden_value.u8(0x1d8) != 0,
            is_multi_spectating: false,
            show_interface: show_value.u8(0xc) != 0,
            chat_status: chat_value.u8(0) as i8 as i64,
            status,
            paused: None,
            play_time,
            game_time: game_time_value.ptr(0) as i64,
            menu_mods: mods(menu_bits.ptr(0) as i64),
            skin_folder: skin,
            current_skin: None,
            memory_songs_folder: songs,
        }))
    }

    pub(super) fn audio_velocity_base_batched(&mut self) -> MemoryRead<AudioVelocityBase> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let holder = match self.capture.audio.take() {
            Some(holder) => holder,
            None => {
                let mut holder = Span::<0x44, 4>::new();
                self.read_batch([holder.request(r)?])?;
                holder.ptr(0x44)
            }
        };
        let mut array_holder = Span::<0x10, 4>::new();
        self.read_batch([array_holder.request(holder)?])?;
        // The array length at +4 directly precedes its 40 float elements.
        let mut array = Span::<4, { 4 + 4 * 40 }>::new();
        self.read_batch([array.request(array_holder.ptr(0x10))?])?;
        if array.i32(4) < 40 {
            return self.not_ready();
        }
        Ok(ReadStatus::Ready(
            (0..40)
                .map(|index| array.f32(LEADER_START + 4 * index) as f64)
                .collect(),
        ))
    }

    pub(super) fn gameplay_batched(
        &mut self,
        play_time: f64,
        first_object_time: Option<i64>,
    ) -> MemoryRead<Gameplay> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        // Gameplay batches stay within one object, so the Windows backend merges each into
        // one native read (pinned by `tests/performance_invariants.rs`); objects are read
        // in turn.
        let (g, ruleset_score, prefetched_game) = match self.capture.gameplay.take() {
            Some((fields, game)) => (fields.ptr(0x64), fields.i32(0xf8), game),
            None => {
                let mut game_holder = Span::<0x64, 4>::new();
                let mut ruleset_score = Span::<0xf8, 4>::new();
                self.read_batch([game_holder.request(r)?, ruleset_score.request(r)?])?;
                (game_holder.ptr(0x64), ruleset_score.i32(0xf8), None)
            }
        };
        if g == 0 {
            return self.not_ready();
        }
        // Score at +0x38, health at +0x40, accuracy at +0x48.
        let game = match prefetched_game {
            Some(game) => game,
            None => {
                let mut game = GameSpan::new();
                self.read_batch([game.request(g)?])?;
                game
            }
        };
        let (b, hp) = (game.ptr(0x38), game.ptr(0x40));
        if b == 0 || hp == 0 {
            return self.not_ready();
        }
        let mut references = Span::<0x1c, 0x10>::new();
        let mut score_processor = Span::<0x54, 4>::new();
        let mut mode_and_max_combo = Span::<0x64, 6>::new();
        let mut score_value = Span::<0x78, 4>::new();
        let mut counts = Span::<0x88, 14>::new();
        self.read_batch([
            references.request(b)?,
            score_processor.request(b)?,
            mode_and_max_combo.request(b)?,
            score_value.request(b)?,
            counts.request(b)?,
        ])?;
        let mode = mode_and_max_combo.i32(0x64);
        let mut health = Span::<0x14, 16>::new();
        self.read_batch([health.request(hp)?])?;
        let retries = match self.capture.retries.take() {
            Some(retries) => retries,
            None => {
                let base = self.patterns.base_addr;
                let retries_anchor = base
                    .checked_sub(0x33)
                    .ok_or(MemoryError::AddressOverflow { base, offset: 0x33 })?;
                let mut retries_holder = Word::new();
                self.read_batch([retries_holder.request(retries_anchor)?])?;
                let mut retries = Span::<8, 4>::new();
                self.read_batch([retries.request(retries_holder.ptr(0))?])?;
                retries.i32(8)
            }
        };
        let [player_name] = self.strings([references.ptr(0x28)])?;
        let mut mod_fields = ModSpan::new();
        self.read_batch([mod_fields.request(references.ptr(0x1c))?])?;
        let mut accuracy = Span::<0xc, 8>::new();
        self.read_batch([accuracy.request(game.ptr(0x48))?])?;

        let vals = if score_has_started(play_time, first_object_time) {
            let mut values = [0i64; 8];
            for (index, value) in values[..7].iter_mut().enumerate() {
                *value = counts.i16(0x88 + 2 * index);
            }
            values[7] = mode_and_max_combo.i16(0x68);
            values
        } else {
            [0; 8]
        };
        let st = legacy_stats(mode, vals[1], vals[0], vals[2], vals[5], vals[3], vals[4]);
        let score_processor = score_processor.i32(0x54);
        let total = if score_processor == 0 {
            score_value.i32(0x78)
        } else {
            ruleset_score
        } as f64;
        let h = health.f64(0x1c);
        let smooth = health.f64(0x14);
        let smooth = if smooth == 0.0 || smooth.is_nan() {
            0.0
        } else {
            smooth
        };
        let mut bits = (mod_fields.i32(0xc) as i32 ^ mod_fields.i32(8) as i32) as i64;
        // ScoreV2 is a win condition and is not represented in the
        // score's regular mod bitfields.
        if score_processor != 0 {
            bits |= crate::calculation::osu_mods::SCORE_V2;
        }
        let mods = self.with_random_seed(mods(bits), r, mode)?;
        self.gameplay_mode = mode;
        Ok(ReadStatus::Ready(Gameplay {
            failed: h <= 0.0,
            retries,
            player_name,
            mods,
            mode,
            score: total,
            player_hp_smooth: smooth,
            player_hp: h,
            accuracy: accuracy.f64(0xc),
            statistics: st,
            maximum_statistics: default_stats(),
            combo: vals[6],
            max_combo: vals[7],
            pp: None,
        }))
    }

    pub(super) fn leaderboard_batched(&mut self, mode: i64) -> MemoryRead<Leaderboard> {
        let r = self.ruleset()?;
        let (base, addr) = match self.capture.leaderboard.take() {
            Some(prefetched) => prefetched,
            None => {
                let mut base_holder = Span::<0x78, 4>::new();
                self.read_batch([base_holder.request(r)?])?;
                let base = base_holder.ptr(0x78);
                if base == 0 {
                    (0, 0)
                } else {
                    let mut addr_holder = Span::<0x24, 4>::new();
                    self.read_batch([addr_holder.request(base)?])?;
                    (base, addr_holder.ptr(0x24))
                }
            }
        };
        if base == 0 || addr == 0 {
            return Ok(ReadStatus::Ready((false, None, vec![])));
        }
        // List at +4, current player at +0x10.
        let mut board = Span::<4, 0x10>::new();
        self.read_batch([board.request(addr)?])?;
        let (list, current) = (board.ptr(4), board.ptr(0x10));
        // Items at +4, size at +0xc.
        let mut current_fields = PlayerSpan::new();
        let mut list_fields = Span::<4, 0xc>::new();
        self.read_batch([current_fields.request(current)?, list_fields.request(list)?])?;
        let raw_n = list_fields.i32(0xc);
        if !(0..=MAX_LEADERBOARD_PLAYERS as i64).contains(&raw_n) {
            return Err(MemoryError::Corrupt {
                context: "leaderboard",
                address: list,
                message: "invalid count",
            });
        }
        let n = raw_n as usize;
        let items = list_fields.ptr(4);
        let items_address = if n == 0 {
            0
        } else {
            self.add(items, LEADER_START)?
        };
        let current_entry = current_fields.ptr(0x20);
        let mut visible = Span::<0x20, 1>::new();
        let mut item_bytes = [0u8; 4 * MAX_LEADERBOARD_PLAYERS];
        let mut current_score = ScoreSpan::new();
        self.read_batch(
            [
                Some(visible.request(current_fields.ptr(0x24))?),
                (n != 0).then(|| ReadRequest::new(items_address, &mut item_bytes[..4 * n])),
                (current_entry != 0)
                    .then(|| current_score.request(current_entry))
                    .transpose()?,
            ]
            .into_iter()
            .flatten(),
        )?;

        let mut fields = (0..n).map(|_| PlayerSpan::new()).collect::<Vec<_>>();
        let mut requests = fields
            .iter_mut()
            .zip(item_bytes.chunks_exact(4))
            .map(|(span, pointer)| {
                span.request(u32::from_le_bytes(pointer.try_into().unwrap()) as usize)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.read_requests(&mut requests)?;
        drop(requests);
        // The sequential reader stops at the first entry without a score.
        let used = fields
            .iter()
            .position(|span| span.ptr(0x20) == 0)
            .unwrap_or(n);
        let mut scores = (0..used).map(|_| ScoreSpan::new()).collect::<Vec<_>>();
        let mut requests = scores
            .iter_mut()
            .zip(&fields)
            .map(|(score, span)| score.request(span.ptr(0x20)))
            .collect::<Result<Vec<_>, _>>()?;
        self.read_requests(&mut requests)?;
        drop(requests);

        // Entry 0 is the current player when it has a score; the rest follow the list.
        let entries = (current_entry != 0)
            .then_some((&current_fields, &current_score))
            .into_iter()
            .chain(fields.iter().zip(&scores))
            .collect::<Vec<_>>();
        let mut mod_fields = (0..entries.len())
            .map(|_| ModSpan::new())
            .collect::<Vec<_>>();
        let mut user_ids = (0..entries.len())
            .map(|_| Span::<0x70, 4>::new())
            .collect::<Vec<_>>();
        let mut names = entries
            .iter()
            .map(|(span, _)| StringSlot::new(span.ptr(8)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut requests = Vec::with_capacity(entries.len() * 3);
        for (((_, score), mod_span), (user_id, name)) in entries
            .iter()
            .zip(mod_fields.iter_mut())
            .zip(user_ids.iter_mut().zip(names.iter_mut()))
        {
            requests.push(mod_span.request(score.ptr(0x1c))?);
            let user = score.ptr(0x48);
            if user != 0 {
                requests.push(user_id.request(user)?);
            }
            requests.extend(name.request());
        }
        self.read_requests(&mut requests)?;
        drop(requests);
        self.finish_strings(&mut names)?;

        let mut decoded = Vec::with_capacity(entries.len());
        for (index, (span, score)) in entries.iter().enumerate() {
            let user_id = if score.ptr(0x48) == 0 {
                0
            } else {
                user_ids[index].i32(0x70)
            };
            decoded.push(player(
                mode,
                span,
                score,
                &mod_fields[index],
                user_id,
                names[index].decode()?,
            ));
        }
        let mut decoded = decoded.into_iter();
        let current = if current_entry == 0 {
            None
        } else {
            decoded.next()
        };
        let mut out = Vec::with_capacity(used);
        let mut name_bytes = 0usize;
        for entry in decoded {
            name_bytes = name_bytes.saturating_add(entry.name.len());
            if name_bytes > MAX_LEADERBOARD_NAME_BYTES {
                return Err(MemoryError::Corrupt {
                    context: "leaderboard",
                    address: items_address,
                    message: "aggregate name budget exceeded",
                });
            }
            out.push(entry);
        }
        Ok(ReadStatus::Ready((visible.u8(0x20) != 0, current, out)))
    }

    pub(super) fn settings_batched(&mut self) -> MemoryRead<Settings> {
        let (cp, bp) = match self.capture.settings.take() {
            Some(prefetched) => prefetched,
            None => {
                let (mut config_holder, mut bindings_holder) = (Word::new(), Word::new());
                self.read_batch([
                    config_holder.request(self.patterns.configuration_addr)?,
                    bindings_holder.request(self.patterns.bindings_addr)?,
                ])?;
                let (mut config_word, mut bindings_word) = (Word::new(), Word::new());
                self.read_batch([
                    config_word.request(config_holder.ptr(0))?,
                    bindings_word.request(bindings_holder.ptr(0))?,
                ])?;
                (config_word.ptr(0), bindings_word.ptr(0))
            }
        };
        if cp == 0 || bp == 0 {
            return self.not_ready();
        }
        if cp != self.config_identity || self.config_positions.is_empty() {
            self.config_positions = self.setting_positions(cp, true)?;
            self.config_identity = cp
        }
        if bp != self.binding_identity || self.binding_positions.is_empty() {
            self.binding_positions = self.setting_positions(bp, false)?;
            self.binding_identity = bp
        }

        let (mut config_items, mut binding_items) = (Span::<8, 4>::new(), Span::<8, 4>::new());
        self.read_batch(
            [
                (!self.config_positions.is_empty())
                    .then(|| config_items.request(cp))
                    .transpose()?,
                (!self.binding_positions.is_empty())
                    .then(|| binding_items.request(bp))
                    .transpose()?,
            ]
            .into_iter()
            .flatten(),
        )?;
        let mut config_records = vec![[0u8; 16]; self.config_positions.len()];
        let mut binding_records = vec![[0u8; 16]; self.binding_positions.len()];
        let mut requests = Vec::with_capacity(config_records.len() + binding_records.len());
        for (positions, items, records) in [
            (&self.config_positions, &config_items, &mut config_records),
            (
                &self.binding_positions,
                &binding_items,
                &mut binding_records,
            ),
        ] {
            for (position, record) in positions.iter().zip(records.iter_mut()) {
                requests.push(ReadRequest::new(
                    self.add(items.ptr(8), 8 + 0x10 * position)?,
                    record,
                ));
            }
        }
        self.read_requests(&mut requests)?;
        drop(requests);

        // Each key with every value field its kind may select: a bool or enum at +0xc,
        // a number or string pointer at +4.
        let mut keys = config_records
            .iter()
            .map(|record| {
                StringSlot::new(u32::from_le_bytes(record[0..4].try_into().unwrap()) as usize)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut values = (0..config_records.len())
            .map(|_| Span::<4, 0xc>::new())
            .collect::<Vec<_>>();
        let mut requests = Vec::with_capacity(2 * config_records.len());
        for ((record, key), value) in config_records.iter().zip(&mut keys).zip(&mut values) {
            requests.extend(key.request());
            requests.push(
                value.request(u32::from_le_bytes(record[4..8].try_into().unwrap()) as usize)?,
            );
        }
        self.read_requests(&mut requests)?;
        drop(requests);
        self.finish_strings(&mut keys)?;

        let mut candidates = Vec::with_capacity(keys.len());
        for (index, key) in keys.iter().enumerate() {
            // Unreadable or unknown keys are skipped, as `config_values` skips them.
            let Ok(key) = key.decode() else {
                continue;
            };
            let Some((_, kind, path)) = CONFIG.iter().find(|(name, _, _)| *name == key) else {
                continue;
            };
            if matches!(*kind, "bool" | "enum" | "int" | "double" | "bstring") {
                candidates.push((*kind, *path, &values[index]));
            }
        }
        let mut texts = candidates
            .iter()
            .map(|(kind, _, value)| {
                StringSlot::new(if *kind == "bstring" { value.ptr(4) } else { 0 })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.read_batch(texts.iter_mut().filter_map(StringSlot::request))?;
        self.finish_strings(&mut texts)?;

        let mut out = IndexMap::new();
        for ((kind, path, value), text) in candidates.into_iter().zip(&texts) {
            let value = match kind {
                "bool" => json!(value.u8(0xc) != 0),
                "enum" => json!(value.i32(0xc) as i32),
                "int" | "double" => json!(value.f64(4)),
                _ => {
                    let Ok(text) = text.decode() else {
                        continue;
                    };
                    json!(text)
                }
            };
            // False, zero, and empty-string settings are valid source values.
            out.insert(path.into(), value);
        }
        for record in binding_records {
            let key = i32::from_le_bytes(record[0..4].try_into().unwrap());
            let value = i32::from_le_bytes(record[12..16].try_into().unwrap());
            if key == 0 {
                continue;
            }
            if let Some((_, path)) = BINDINGS.iter().find(|(id, _)| *id == key)
                && let Some(name) = key_name(value)
            {
                out.insert((*path).into(), Value::String(name.into()));
            }
        }
        if self.read_mania_scroll_speed {
            let global = out.get("mania.scrollSpeed").and_then(Value::as_f64);
            if let Some(speed) = self.beatmap_scroll_speed(global).unwrap_or(global) {
                out.insert("mania.scrollSpeed".into(), json!(speed));
            }
        }
        Ok(ReadStatus::Ready(out))
    }
}

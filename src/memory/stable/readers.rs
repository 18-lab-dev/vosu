const MAX_KEY_OVERLAY_BUTTONS: usize = 18;
const MAX_STABLE_HIT_ERRORS: i64 = 100_000;
const MAX_STABLE_HIT_ERROR_SUFFIX: i64 = 4_096;
pub(super) const MAX_TOURNEY_CHAT_CHANNELS: usize = 256;
pub(super) const MAX_TOURNEY_CHAT_MESSAGES: usize = 2_048;
pub(super) const MAX_LEADERBOARD_PLAYERS: usize = 256;
pub(super) const MAX_LEADERBOARD_NAME_BYTES: usize = 256 * 1024;
const MAX_REPLAY_FRAMES: i64 = 500_000;

use super::*;
use std::sync::Arc;

impl SynchronizedMemoryReader for StableMemory {
    fn synchronize_capture_config(
        &mut self,
        show_mp_commands: bool,
        read_mania_scroll_speed: bool,
    ) {
        self.show_mp_commands = show_mp_commands;
        self.read_mania_scroll_speed = read_mania_scroll_speed;
        self.ruleset_container = 0;
        self.status_container = 0;
        self.play_time_container = 0;
        self.capture = super::batched::CapturePrefetch::default();
    }

    fn synchronize_precise(&mut self, _state: ReaderSynchronization, mode: i64) {
        self.gameplay_mode = mode;
    }
}

impl StableMemory {
    pub(super) fn result_screen_sequential(&mut self) -> MemoryRead<ResultScreen> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let b = self.ptr(self.add(r, 0x38)?)?;
        if b == 0 {
            return self.not_ready();
        }
        let mode = self.i32(self.add(b, 0x64)?)?;
        let x = self.ptr(self.add(b, 0x1c)?)?;
        let m = self.with_random_seed(
            mods(self.i32(self.add(x, 0xc)?)? ^ self.i32(self.add(x, 8)?)?),
            r,
            mode,
        )?;
        let counts = self.score_counts(b)?;
        let s = legacy_stats(
            mode, counts[1], counts[0], counts[2], counts[5], counts[3], counts[4],
        );
        let accuracy = accuracy(mode, &s, &m);
        Ok(ReadStatus::Ready(ResultScreen {
            online_id: js_long(self.process.read_i64(self.add(b, 4)?)?),
            player_name: self.string_ptr(self.add(b, 0x28)?)?,
            mods: m,
            mode,
            accuracy,
            max_combo: counts[7],
            score: self.i32(self.add(b, 0x78)?)? as f64,
            statistics: s,
            maximum_statistics: default_stats(),
            date: net_date(
                self.process.read_i32(self.add(b, 0xa4)?)?,
                self.process.read_i32(self.add(b, 0xa0)?)?,
            )?,
        }))
    }
    fn tourney_sequential(&mut self) -> MemoryRead<Tourney> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let l = self.ptr(self.add(r, 0x1c)?)?;
        let q = self.ptr(self.add(r, 0x20)?)?;
        let team = |b| -> Result<String, MemoryError> {
            self.string(self.ptr(self.add(self.ptr(self.add(b, 0x20)?)?, 0x144)?)?)
        };
        Ok(ReadStatus::Ready(Tourney {
            ipc_state: self.i32(self.add(r, 0x54)?)?,
            left_stars: self.i32(self.add(l, 0x2c)?)?,
            right_stars: self.i32(self.add(q, 0x2c)?)?,
            best_of: self.i32(self.add(q, 0x30)?)?,
            stars_visible: self.process.read_u8(self.add(q, 0x38)?)? != 0,
            score_visible: self.process.read_u8(self.add(q, 0x39)?)? != 0,
            first_team_name: team(l)?,
            second_team_name: team(q)?,
            first_team_score: self.i32(self.add(l, 0x28)?)?,
            second_team_score: self.i32(self.add(q, 0x28)?)?,
        }))
    }
    fn reset_replay_frames(&mut self, score: usize, error_index: i64) {
        self.replay_score = score;
        self.replay_error_index = error_index;
        self.replay_frame_index = 0;
        self.replay_batch = None;
        self.replay_pending.clear();
    }

    pub(super) fn replay_frame_batch(
        &mut self,
        score: usize,
        acknowledged_error_index: i64,
    ) -> Result<Option<StableReplayFrameBatch>, MemoryError> {
        match self.read_replay_frame_batch(score, acknowledged_error_index) {
            Ok(batch) => Ok(batch),
            Err(error) if self.gameplay_mode == 3 => Err(error),
            Err(_) => {
                // Cursor/spatial data must not hold up ordinary hit-error publication.
                self.replay_batch = None;
                Ok(None)
            }
        }
    }

    fn read_replay_frame_batch(
        &mut self,
        score: usize,
        acknowledged_error_index: i64,
    ) -> Result<Option<StableReplayFrameBatch>, MemoryError> {
        if self.replay_score != score || acknowledged_error_index < self.replay_error_index {
            self.reset_replay_frames(score, acknowledged_error_index);
        }
        self.replay_error_index = acknowledged_error_index;

        let list = self.ptr(self.add(score, 0x34)?)?;
        if list == 0 {
            return Ok(None);
        }
        // Size before items: see `hit_errors`.
        let size = self.process.read_i32(self.add(list, 0xc)?)?.max(0) as i64;
        let items = self.ptr(self.add(list, 4)?)?;
        if size > MAX_REPLAY_FRAMES {
            return Err(MemoryError::Corrupt {
                context: "stable replay frames",
                address: list,
                message: "invalid frame count",
            });
        }
        // A preloaded replay needs one bounded positioning search on attachment or
        // rewind. Normal polling walks forward through the same batched frames we
        // publish; it never reads individual pointers/timestamps to find the prefix.
        let play_time = self.play_time_value()?;
        if size < self.replay_frame_index
            || self
                .replay_batch
                .as_ref()
                .and_then(StableReplayFrameBatch::newest)
                .is_some_and(|newest| {
                    newest.time - play_time
                        > crate::model::memory::StableReplayFrames::CLOCK_STEP_BACK_MS
                })
        {
            self.reset_replay_frames(score, acknowledged_error_index);
        }
        if size < self.replay_frame_index + self.replay_pending.len() as i64 {
            self.replay_pending.clear();
        }
        let limit = crate::model::memory::StableReplayFrames::LIMIT as i64;
        let mut end = size;
        if self.replay_batch.is_none() && size > limit {
            let mut low = 0;
            let mut high = size;
            while low < high {
                let middle = low + (high - low) / 2;
                let frame = self.ptr(self.add(items, LEADER_START + 4 * middle as usize)?)?;
                let time = self.process.read_i32(self.add(frame, 16)?)? as i64;
                if time <= play_time {
                    low = middle + 1;
                } else {
                    high = middle;
                }
            }
            end = low;
            if end < self.replay_frame_index {
                self.reset_replay_frames(score, acknowledged_error_index);
            }
            let start = (end - limit).max(self.replay_frame_index);
            if start != self.replay_frame_index {
                self.replay_pending.clear();
                self.replay_frame_index = start;
            }
        }
        loop {
            if self.replay_pending.is_empty() && self.replay_frame_index < end {
                let remaining = end - self.replay_frame_index;
                // Look ahead only a small chunk in long preloaded replays. Keep
                // future raw frames until their time arrives (validate on exposure).
                // Live appends up to the retention limit still take just two reads.
                let count = if remaining > limit {
                    64
                } else {
                    remaining as usize
                };
                let start = self.replay_frame_index;
                let pointer_address = self.add(items, LEADER_START + 4 * start as usize)?;
                if self.gameplay_mode != 3 {
                    // A failed suffix is lost, not retried forever. Mania remains
                    // strict and retryable, including native read failures.
                    self.replay_frame_index += count as i64;
                }
                let mut pointer_bytes = vec![0; count * 4];
                self.process
                    .read_exact(pointer_address, &mut pointer_bytes)?;
                let mut frame_bytes = vec![[0; 16]; count];
                let addresses = pointer_bytes
                    .chunks_exact(4)
                    .map(|bytes| {
                        let frame = u32::from_le_bytes(bytes.try_into().unwrap()) as usize;
                        self.add(frame, 4)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut requests = addresses
                    .iter()
                    .copied()
                    .zip(frame_bytes.iter_mut())
                    .map(|(address, bytes)| ReadRequest::new(address, bytes))
                    .collect::<Vec<_>>();
                self.process.read_many(&mut requests)?;
                self.replay_frame_index = start;
                self.replay_pending = frame_bytes.into();
            }
            let count = self
                .replay_pending
                .iter()
                .take_while(|bytes| {
                    i32::from_le_bytes(bytes[12..].try_into().unwrap()) as i64 <= play_time
                })
                .count();
            if count == 0 {
                break;
            }
            let mut frames = Vec::with_capacity(count);
            let mut invalid = false;
            for bytes in self.replay_pending.iter().take(count) {
                let x = f32::from_le_bytes(bytes[..4].try_into().unwrap());
                let y = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
                invalid |= !x.is_finite()
                    || !y.is_finite()
                    || (self.gameplay_mode == 3
                        && (x.fract() != 0.0 || !(0.0..=262_143.0).contains(&x)));
                frames.push(StableReplayFrame {
                    time: i32::from_le_bytes(bytes[12..].try_into().unwrap()) as i64,
                    x,
                    buttons: bytes[8],
                    buttons_raw: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
                    y,
                });
            }
            if !invalid || self.gameplay_mode != 3 {
                self.replay_pending.drain(..count);
                self.replay_frame_index += count as i64;
            }
            if invalid {
                if self.gameplay_mode == 3 {
                    self.replay_pending.clear();
                }
                return Err(MemoryError::Corrupt {
                    context: "stable replay frames",
                    address: list,
                    message: "invalid replay frame",
                });
            }
            let mut retained = self
                .replay_batch
                .as_ref()
                .map(|batch| (*batch.frames).clone())
                .unwrap_or_default();
            retained.extend(frames);
            self.replay_batch = Some(StableReplayFrameBatch {
                start_index: self.replay_frame_index - retained.len() as i64,
                index: self.replay_frame_index,
                frames: Arc::new(retained),
            });
            if !self.replay_pending.is_empty() || self.replay_frame_index >= end {
                break;
            }
        }
        if let Some(batch) = &self.replay_batch {
            return Ok(Some(batch.clone()));
        }

        let batch = StableReplayFrameBatch {
            start_index: self.replay_frame_index,
            index: self.replay_frame_index,
            frames: Arc::default(),
        };
        self.replay_batch = Some(batch.clone());
        Ok(Some(batch))
    }
    /// Unbatched reference reader; also the fallback deciding every batched error.
    pub(super) fn audio_velocity_base_sequential(&mut self) -> MemoryRead<AudioVelocityBase> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let av = self.ptr(self.add(self.ptr(self.add(r, 0x44)?)?, 0x10)?)?;
        if self.process.read_i32(self.add(av, 4)?)? < 40 {
            return self.not_ready();
        }
        let mut out = Vec::with_capacity(40);
        for i in 0..40 {
            out.push(self.process.read_f32(self.add(av, LEADER_START + 4 * i)?)? as f64)
        }
        Ok(ReadStatus::Ready(out))
    }
    pub(super) fn user_sequential(&mut self) -> MemoryRead<User> {
        let b = self.ptr2(self.patterns.user_profile_ptr)?;
        if b == 0 {
            return self.not_ready();
        }
        let raw_login = self.ptr2(self.patterns.raw_login_status_ptr)? as i64;
        Ok(ReadStatus::Ready(User {
            name: self.string_ptr(self.add(b, 0x30)?)?,
            accuracy: self.process.read_f64(self.add(b, 4)?)?,
            ranked_score: js_long(self.process.read_i64(self.add(b, 0xc)?)?),
            id: self.i32(self.add(b, 0x70)?)?,
            level: self.process.read_f32(self.add(b, 0x74)?)? as f64,
            play_count: self.i32(self.add(b, 0x7c)?)?,
            play_mode: self.i32(self.add(b, 0x80)?)?,
            rank: self.i32(self.add(b, 0x84)?)?,
            country_code: self.i32(self.add(b, 0x9c)?)?,
            performance_points: self.i32(self.add(b, 0x88)?)? as f64,
            raw_bancho_status: self.process.read_i8(self.add(b, 0x8c)?)? as i64,
            background_colour: self.process.read_u32(self.add(b, 0xac)?)? as i64,
            raw_login_status: raw_login,
            matchmaking: None,
        }))
    }
    pub(super) fn gameplay_sequential(
        &mut self,
        play_time: f64,
        first_object_time: Option<i64>,
    ) -> MemoryRead<Gameplay> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let g = self.ptr(self.add(r, 0x64)?)?;
        if g == 0 {
            return self.not_ready();
        }
        let mut b_bytes = [0; 4];
        let mut hp_bytes = [0; 4];
        self.process.read_many(&mut [
            ReadRequest::new(self.add(g, 0x38)?, &mut b_bytes),
            ReadRequest::new(self.add(g, 0x40)?, &mut hp_bytes),
        ])?;
        let b = u32::from_le_bytes(b_bytes) as usize;
        let hp = u32::from_le_bytes(hp_bytes) as usize;
        if b == 0 || hp == 0 {
            return self.not_ready();
        }
        let mut mode_bytes = [0; 4];
        let mut x_bytes = [0; 4];
        self.process.read_many(&mut [
            ReadRequest::new(self.add(b, 0x1c)?, &mut x_bytes),
            ReadRequest::new(self.add(b, 0x64)?, &mut mode_bytes),
        ])?;
        let mode = i32::from_le_bytes(mode_bytes) as i64;
        self.gameplay_mode = mode;
        let x = u32::from_le_bytes(x_bytes) as usize;
        let vals = if score_has_started(play_time, first_object_time) {
            self.score_counts(b)?
        } else {
            [0; 8]
        };
        let st = legacy_stats(mode, vals[1], vals[0], vals[2], vals[5], vals[3], vals[4]);
        let score = if self.ptr(self.add(b, 0x54)?)? == 0 {
            self.i32(self.add(b, 0x78)?)?
        } else {
            self.i32(self.add(r, 0xf8)?)?
        } as f64;
        let mut health_bytes = [0; 16];
        let smooth_address = self.add(hp, 0x14)?;
        let health_address = self.add(hp, 0x1c)?;
        if self
            .process
            .read_exact(smooth_address, &mut health_bytes)
            .is_err()
        {
            let (smooth_bytes, h_bytes) = health_bytes.split_at_mut(8);
            self.process.read_many(&mut [
                ReadRequest::new(smooth_address, smooth_bytes),
                ReadRequest::new(health_address, h_bytes),
            ])?;
        }
        let h = f64::from_le_bytes(health_bytes[8..].try_into().unwrap());
        let smooth = f64::from_le_bytes(health_bytes[..8].try_into().unwrap());
        let smooth = if smooth == 0.0 || smooth.is_nan() {
            0.0
        } else {
            smooth
        };
        Ok(ReadStatus::Ready(Gameplay {
            failed: h <= 0.0,
            retries: self.i32(self.add(
                self.ptr(self.patterns.base_addr.checked_sub(0x33).ok_or(
                    MemoryError::AddressOverflow {
                        base: self.patterns.base_addr,
                        offset: 0x33,
                    },
                )?)?,
                8,
            )?)?,
            player_name: self.string_ptr(self.add(b, 0x28)?)?,
            mods: {
                let mut mod_bytes = [0; 8];
                let right_address = self.add(x, 8)?;
                let left_address = self.add(x, 0xc)?;
                if self
                    .process
                    .read_exact(right_address, &mut mod_bytes)
                    .is_err()
                {
                    let (right_bytes, left_bytes) = mod_bytes.split_at_mut(4);
                    self.process.read_many(&mut [
                        ReadRequest::new(right_address, right_bytes),
                        ReadRequest::new(left_address, left_bytes),
                    ])?;
                }
                let right_bytes = mod_bytes[..4].try_into().unwrap();
                let left_bytes = mod_bytes[4..].try_into().unwrap();
                let score_processor = self.i32(self.add(b, 0x54)?)?;
                let mut bits =
                    (i32::from_le_bytes(left_bytes) ^ i32::from_le_bytes(right_bytes)) as i64;
                // ScoreV2 is a win condition and is not represented in the
                // score's regular mod bitfields.
                if score_processor != 0 {
                    bits |= crate::calculation::osu_mods::SCORE_V2;
                }
                self.with_random_seed(mods(bits), r, mode)?
            },
            mode,
            score,
            player_hp_smooth: smooth,
            player_hp: h,
            accuracy: self
                .process
                .read_f64(self.add(self.ptr(self.add(g, 0x48)?)?, 0xc)?)?,
            statistics: st,
            maximum_statistics: default_stats(),
            combo: vals[6],
            max_combo: vals[7],
            pp: None,
        }))
    }
    pub(super) fn global_sequential(&mut self) -> MemoryRead<Global> {
        let menu = mods(self.ptr2(self.patterns.menu_mods_ptr)? as i64);
        let chat = self
            .process
            .read_i8(self.ptr(self.patterns.chat_checker_ptr)?)? as i64;
        let watching = self
            .process
            .read_u8(self.ptr(self.add(self.patterns.can_run_slowly_addr, 0x46)?)?)?
            == 1;
        let game_time = self.ptr2(self.patterns.game_time_ptr)? as i64;
        let status = self.status_value()?;
        let play_time = if status == 3 {
            0.0
        } else {
            self.play_time_value()? as f64
        };
        let sc = self.ptr(self.add(self.patterns.settings_class_addr, 8)?)?;
        let songs_ptr = self.ptr(self.add(self.ptr(self.add(sc, 0xb8)?)?, 4)?)?;
        let songs = self.string(songs_ptr)?;
        let show = self
            .process
            .read_u8(self.add(self.ptr(self.add(sc, 4)?)?, 0xc)?)?
            != 0;
        let mut hidden = false;
        if watching {
            let r = self.ruleset()?;
            if r != 0 {
                hidden = self.process.read_u8(self.add(r, 0x1d8)?)? != 0
            }
        }
        let skin_addr = self.ptr(self.add(self.patterns.skin_data_addr, 7)?)?;
        let skin = if skin_addr == 0 {
            String::new()
        } else {
            let b = self.ptr(skin_addr)?;
            if b == 0 {
                String::new()
            } else {
                self.string_ptr(self.add(b, 0x44)?)?
            }
        };
        Ok(ReadStatus::Ready(Global {
            is_watching_replay: watching,
            is_replay_ui_hidden: hidden,
            is_multi_spectating: false,
            show_interface: show,
            chat_status: chat,
            status,
            paused: None,
            play_time,
            game_time,
            menu_mods: menu,
            skin_folder: skin,
            memory_songs_folder: songs,
        }))
    }
    pub(super) fn menu_sequential(&mut self, prev: &str) -> MemoryRead<Menu> {
        let beat = self.ptr2(self.patterns.base_addr.checked_sub(0xc).ok_or(
            MemoryError::AddressOverflow {
                base: self.patterns.base_addr,
                offset: 0xc,
            },
        )?)?;
        if beat == 0 {
            return self.not_ready();
        }
        let mode = self.ptr2(self.patterns.base_addr.checked_sub(0x33).ok_or(
            MemoryError::AddressOverflow {
                base: self.patterns.base_addr,
                offset: 0x33,
            },
        )?)? as i64;
        let checksum = self.string_ptr(self.add(beat, 0x6c)?)?;
        let filename = self.string_ptr(self.add(beat, 0x90)?)?;
        let ranked = stable_ranked_status(self.i32(self.add(beat, 0x12c)?)?);
        if checksum == prev || !filename.ends_with(".osu") {
            return Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
                gamemode: mode,
                ranked_status: ranked,
            })));
        }
        let strf = |o| self.string_ptr(self.add(beat, o)?);
        let audio = strf(0x64)?;
        let bg = strf(0x68)?;
        let base_mode = self.ptr(self.patterns.base_addr.checked_sub(0x33).unwrap())?;
        Ok(ReadStatus::Ready(Menu::Full(MenuFull {
            gamemode: mode,
            checksum,
            filename,
            plays: self.i32(self.add(base_mode, 0xc)?)?,
            artist: strf(0x18)?,
            artist_original: strf(0x1c)?,
            title: strf(0x24)?,
            title_original: strf(0x28)?,
            ar: self.process.read_f32(self.add(beat, 0x2c)?)? as f64,
            cs: self.process.read_f32(self.add(beat, 0x30)?)? as f64,
            hp: self.process.read_f32(self.add(beat, 0x34)?)? as f64,
            od: self.process.read_f32(self.add(beat, 0x38)?)? as f64,
            audio_file_mimetype: mime(&audio),
            audio_filename: audio,
            background_file_mimetype: mime(&bg),
            background_filename: bg,
            folder: strf(0x78)?,
            creator: strf(0x7c)?,
            difficulty: strf(0xac)?,
            map_id: self.i32(self.add(beat, 0xc8)?)?,
            set_id: self.i32(self.add(beat, 0xcc)?)?,
            ranked_status: ranked,
            object_count: self.i32(self.add(beat, 0xf8)?)?,
        })))
    }
    pub(super) fn leaderboard_sequential(&mut self, mode: i64) -> MemoryRead<Leaderboard> {
        let r = self.ruleset()?;
        let base = self.ptr(self.add(r, 0x78)?)?;
        if base == 0 {
            return Ok(ReadStatus::Ready((false, None, vec![])));
        }
        let addr = self.ptr(self.add(base, 0x24)?)?;
        if addr == 0 {
            return Ok(ReadStatus::Ready((false, None, vec![])));
        }
        let player = self.ptr(self.add(addr, 0x10)?)?;
        let visible = self
            .process
            .read_u8(self.add(self.ptr(self.add(player, 0x24)?)?, 0x20)?)?
            != 0;
        let current = self.leaderboard_player(player, mode)?;
        let list = self.ptr(self.add(addr, 4)?)?;
        let raw_n = self.process.read_i32(self.add(list, 0xc)?)?;
        if !(0..=MAX_LEADERBOARD_PLAYERS as i32).contains(&raw_n) {
            return Err(MemoryError::Corrupt {
                context: "leaderboard",
                address: list,
                message: "invalid count",
            });
        }
        let n = raw_n as usize;
        if n < 1 {
            return Ok(ReadStatus::Ready((visible, current, vec![])));
        }
        let items = self.ptr(self.add(list, 4)?)?;
        let mut out = Vec::with_capacity(n);
        let mut name_bytes = 0usize;
        for i in 0..n {
            let player =
                self.leaderboard_player(self.ptr(self.add(items, LEADER_START + 4 * i)?)?, mode)?;
            match player {
                Some(player) => {
                    name_bytes = name_bytes.saturating_add(player.name.len());
                    if name_bytes > MAX_LEADERBOARD_NAME_BYTES {
                        return Err(MemoryError::Corrupt {
                            context: "leaderboard",
                            address: items,
                            message: "aggregate name budget exceeded",
                        });
                    }
                    out.push(player);
                }
                None => break,
            }
        }
        Ok(ReadStatus::Ready((visible, current, out)))
    }
    pub(super) fn settings_sequential(&mut self) -> MemoryRead<Settings> {
        let cp = self.ptr2(self.patterns.configuration_addr)?;
        let bp = self.ptr2(self.patterns.bindings_addr)?;
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
        let mut out = IndexMap::new();
        for (path, value) in self.config_values(cp, &self.config_positions)? {
            // False, zero, and empty-string settings are valid source values;
            // only unreadable/null entries are omitted by config_values.
            out.insert(path, value);
        }
        for record in self.setting_records(bp, &self.binding_positions)? {
            let Some(record) = record else { continue };
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

impl MemoryReader for StableMemory {
    fn audio_velocity_base(&mut self) -> MemoryRead<AudioVelocityBase> {
        self.audio_velocity_base_batched().or_else(|_| {
            self.drop_carried_ruleset();
            self.audio_velocity_base_sequential()
        })
    }
    fn user(&mut self) -> MemoryRead<User> {
        self.user_batched().or_else(|_| self.user_sequential())
    }
    fn gameplay(&mut self, play_time: f64, first_object_time: Option<i64>) -> MemoryRead<Gameplay> {
        self.gameplay_batched(play_time, first_object_time)
            .or_else(|_| {
                self.drop_carried_ruleset();
                self.gameplay_sequential(play_time, first_object_time)
            })
    }
    fn global(&mut self) -> MemoryRead<Global> {
        self.global_batched().or_else(|_| self.global_sequential())
    }
    fn menu(&mut self, prev: &str) -> MemoryRead<Menu> {
        self.menu_batched(prev)
            .or_else(|_| self.menu_sequential(prev))
    }
    fn leaderboard(&mut self, mode: i64) -> MemoryRead<Leaderboard> {
        self.leaderboard_batched(mode).or_else(|_| {
            self.drop_carried_ruleset();
            self.leaderboard_sequential(mode)
        })
    }
    fn result_screen(&mut self) -> MemoryRead<ResultScreen> {
        self.result_screen_sequential().or_else(|_| {
            self.drop_carried_ruleset();
            self.result_screen_sequential()
        })
    }
    fn key_overlay(&mut self, mode: i64) -> MemoryRead<KeyOverlay> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let p = self.ptr(self.add(r, 0xac)?)?;
        if p == 0 {
            return self.not_ready();
        }
        let a = self.ptr(self.add(self.ptr(self.add(p, 0x10)?)?, 4)?)?;
        if a == 0 {
            return self.not_ready();
        }
        let length = self.i32(self.add(a, 4)?)?;
        if !(0..=MAX_KEY_OVERLAY_BUTTONS as i64).contains(&length) {
            return Err(MemoryError::Corrupt {
                context: "stable key overlay",
                address: a,
                message: "invalid button count",
            });
        }
        if length == 0 {
            return Ok(ReadStatus::Ready(vec![]));
        }
        let length = length as usize;
        let mut pointers = [0u8; MAX_KEY_OVERLAY_BUTTONS * 4];
        self.process
            .read_exact(self.add(a, 8)?, &mut pointers[..length * 4])?;
        let mut states = [[0u8; 9]; MAX_KEY_OVERLAY_BUTTONS];
        let mut addresses = [0usize; MAX_KEY_OVERLAY_BUTTONS];
        for (index, pointer) in pointers[..length * 4].chunks_exact(4).enumerate() {
            let button = u32::from_le_bytes(pointer.try_into().unwrap()) as usize;
            addresses[index] = self.add(button, 0x14)?;
        }
        let [
            state0,
            state1,
            state2,
            state3,
            state4,
            state5,
            state6,
            state7,
            state8,
            state9,
            state10,
            state11,
            state12,
            state13,
            state14,
            state15,
            state16,
            state17,
        ] = &mut states;
        {
            let mut requests = [
                ReadRequest::new(addresses[0], state0),
                ReadRequest::new(addresses[1], state1),
                ReadRequest::new(addresses[2], state2),
                ReadRequest::new(addresses[3], state3),
                ReadRequest::new(addresses[4], state4),
                ReadRequest::new(addresses[5], state5),
                ReadRequest::new(addresses[6], state6),
                ReadRequest::new(addresses[7], state7),
                ReadRequest::new(addresses[8], state8),
                ReadRequest::new(addresses[9], state9),
                ReadRequest::new(addresses[10], state10),
                ReadRequest::new(addresses[11], state11),
                ReadRequest::new(addresses[12], state12),
                ReadRequest::new(addresses[13], state13),
                ReadRequest::new(addresses[14], state14),
                ReadRequest::new(addresses[15], state15),
                ReadRequest::new(addresses[16], state16),
                ReadRequest::new(addresses[17], state17),
            ];
            self.process.read_many(&mut requests[..length])?;
        }

        let mut out = Vec::with_capacity(length);
        for (index, state) in states[..length].iter().enumerate() {
            out.push(KeyOverlayButton {
                name: KeyOverlayButton::stable_name(mode, index),
                is_pressed: state[8] != 0,
                count: i32::from_le_bytes(state[..4].try_into().unwrap()) as i64,
            })
        }
        Ok(ReadStatus::Ready(out))
    }
    fn hit_errors(&mut self, last: i64) -> MemoryRead<HitErrors> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let g = self.ptr(self.add(r, 0x64)?)?;
        if g == 0 {
            return self.not_ready();
        }
        let s = self.ptr(self.add(g, 0x38)?)?;
        if s == 0 {
            return self.not_ready();
        }
        let b = self.ptr(self.add(s, 0x38)?)?;
        // A growing .NET List publishes its new items array before its new size. Reading the
        // size first therefore always pairs it with an array that holds that many elements;
        // the reverse order can pair a grown size with the abandoned array and read past its
        // end (observed live as a fabricated 0 replacing the ninth error).
        let size = self.process.read_i32(self.add(b, 0xc)?)? as i64;
        let items = self.ptr(self.add(b, 4)?)?;
        if !(0..=MAX_STABLE_HIT_ERRORS).contains(&size) {
            return Err(MemoryError::Corrupt {
                context: "hit errors",
                address: b,
                message: "invalid count",
            });
        }
        let mut index = last.max(0);
        if index > size {
            return Ok(ReadStatus::Ready(HitErrors {
                index: size,
                list_len: size,
                errors: HitErrorArray::default(),
                release_corrections: Vec::new(),
                stable_replay_frames: self.replay_frame_batch(s, size)?,
            }));
        }
        let end = index.saturating_add(MAX_STABLE_HIT_ERROR_SUFFIX).min(size);
        let mut errors = HitErrorArray::default();
        while index < end {
            let e = self.i32(self.add(items, LEADER_START + 4 * index as usize)?)?;
            if !(-10_000..=10_000).contains(&e) {
                break;
            }
            let value = e as f64;
            errors.all.push(value);
            errors.press.push(value);
            index += 1
        }
        let stable_replay_frames = self.replay_frame_batch(s, last.max(0))?;
        Ok(ReadStatus::Ready(HitErrors {
            index,
            list_len: size,
            errors,
            release_corrections: Vec::new(),
            stable_replay_frames,
        }))
    }
    fn catcher(&mut self, play_time: f64) -> MemoryRead<crate::model::memory::Catcher> {
        use crate::model::memory::Catcher;
        // The ordinary suffix capture refreshes this batch even when no judgement arrived.
        if let Some(frame) = self
            .replay_batch
            .as_ref()
            .and_then(StableReplayFrameBatch::newest)
            .filter(|frame| frame.time <= play_time as i64)
        {
            return Ok(ReadStatus::Ready(Catcher {
                x: frame.x,
                dashing: frame.buttons == 1,
            }));
        }
        // Playback-prefix retention is shared by all stable frame consumers. A missing
        // predecessor means the current position is not available; never use a future frame.
        self.not_ready()
    }
    fn global_precise(&mut self) -> MemoryRead<GlobalPrecise> {
        let status = self.status_value()?;
        let time = if status == 3 {
            0.0
        } else {
            self.play_time_value()? as f64
        };
        Ok(ReadStatus::Ready(GlobalPrecise { status, time }))
    }
    fn mp3_length(&mut self) -> MemoryRead<Mp3Length> {
        let b = self.ptr2(self.patterns.get_audio_length_ptr)?;
        Ok(ReadStatus::Ready(js_round_number(
            self.process.read_f64(self.add(b, 4)?)?,
        )))
    }
    fn tourney(&mut self) -> MemoryRead<Tourney> {
        self.tourney_sequential().or_else(|_| {
            self.drop_carried_ruleset();
            self.tourney_sequential()
        })
    }
    fn tourney_chat(&mut self, messages: &[TourneyManagerChatItem]) -> MemoryRead<TourneyChat> {
        if self.chat_address == 0 {
            let p = Pattern::parse(CHAT_PATTERN, false).map_err(|e| MemoryError::Backend {
                operation: "parse chat pattern",
                message: e.to_string(),
            })?;
            self.chat_address = match self.process.scan_first(&p)? {
                Some(v) => v,
                None => return self.not_ready(),
            }
        }
        let list = self.ptr2(self.add(self.chat_address, 1)?)?;
        let items = self.ptr(self.add(list, 4)?)?;
        let len = self.process.read_i32(self.add(items, 4)?)?;
        if !(0..=MAX_TOURNEY_CHAT_CHANNELS as i32).contains(&len) {
            return Err(MemoryError::Corrupt {
                context: "tournament chat channels",
                address: items,
                message: "invalid count",
            });
        }
        let len = len as usize;
        for i in (0..len).rev() {
            let channel = match self.ptr(self.add(items, LEADER_START + 4 * i)?) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if channel == 0 {
                continue;
            }
            let Ok(tag) = self.string_ptr(self.add(channel, 4)?) else {
                continue;
            };
            if tag != "#multiplayer" {
                continue;
            }
            let Ok(msgs) = self.ptr(self.add(channel, 0x10)?) else {
                continue;
            };
            let Ok(mi) = self.ptr(self.add(msgs, 4)?) else {
                continue;
            };
            let size = self.process.read_i32(self.add(msgs, 0xc)?)?;
            if !(0..=MAX_TOURNEY_CHAT_MESSAGES as i32).contains(&size) {
                return Err(MemoryError::Corrupt {
                    context: "tournament chat messages",
                    address: msgs,
                    message: "invalid count",
                });
            }
            let n = size as usize;
            if messages.len() == n {
                continue;
            }
            let mut out = vec![];
            for j in 0..n {
                let item = match self.ptr(self.add(mi, LEADER_START + 4 * j)?) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Ok(content) = self.string_ptr(self.add(item, 4)?) else {
                    continue;
                };
                if content.is_empty() || (!self.show_mp_commands && content.starts_with("!mp")) {
                    continue;
                }
                let Ok(tn) = self.string_ptr(self.add(item, 8)?) else {
                    continue;
                };
                let time = tn.split_whitespace().next().unwrap_or("").to_string();
                let name = tn
                    .strip_prefix(&time)
                    .unwrap_or(&tn)
                    .trim()
                    .trim_end_matches(':')
                    .to_string();
                out.push(TourneyManagerChatItem {
                    time,
                    name,
                    content,
                })
            }
            return Ok(ReadStatus::Ready(out));
        }
        Ok(ReadStatus::Unchanged)
    }
    fn tourney_user(&mut self) -> MemoryRead<TourneyUser> {
        let b = self.ptr2(self.patterns.spectating_user_ptr)?;
        if b == 0 {
            return self.not_ready();
        }
        Ok(ReadStatus::Ready(TourneyUser {
            id: self.i32(self.add(b, 0x70)?)?,
            name: self.string_ptr(self.add(b, 0x30)?)?,
            country: self.string_ptr(self.add(b, 0x2c)?)?,
            accuracy: self.process.read_f64(self.add(b, 4)?)?,
            playcount: self.i32(self.add(b, 0x7c)?)?,
            ranked_score: js_long(self.process.read_i64(self.add(b, 0xc)?)?),
            global_rank: self.i32(self.add(b, 0x84)?)?,
            pp: self.i32(self.add(b, 0x9c)?)? as f64,
        }))
    }
    fn ranked_play(&mut self) -> MemoryRead<RankedPlay> {
        Ok(ReadStatus::Unsupported(
            "Stable: ranked play not implemented",
        ))
    }
    fn room(&mut self) -> MemoryRead<crate::model::memory::LazerRoom> {
        Ok(ReadStatus::Unsupported("lazer multiplayer room"))
    }
    fn spectating_data(
        &mut self,
        _show_mp_commands: bool,
    ) -> MemoryRead<crate::model::memory::LazerSpectator> {
        Ok(ReadStatus::Unsupported(
            "Stable: lazer spectating not implemented",
        ))
    }
    fn settings(&mut self) -> MemoryRead<Settings> {
        self.settings_batched()
            .or_else(|_| self.settings_sequential())
    }
}

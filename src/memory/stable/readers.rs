const MAX_KEY_OVERLAY_BUTTONS: usize = 18;

use super::*;

impl SynchronizedMemoryReader for StableMemory {
    fn synchronize_capture_config(
        &mut self,
        show_mp_commands: bool,
        read_mania_scroll_speed: bool,
    ) {
        self.show_mp_commands = show_mp_commands;
        self.read_mania_scroll_speed = read_mania_scroll_speed;
    }
}

impl MemoryReader for StableMemory {
    fn audio_velocity_base(&mut self) -> MemoryRead<AudioVelocityBase> {
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
    fn user(&mut self) -> MemoryRead<User> {
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
    fn result_screen(&mut self) -> MemoryRead<ResultScreen> {
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
    fn gameplay(&mut self, play_time: f64, first_object_time: Option<i64>) -> MemoryRead<Gameplay> {
        let r = self.ruleset()?;
        if r == 0 {
            return self.not_ready();
        }
        let g = self.ptr(self.add(r, 0x64)?)?;
        if g == 0 {
            return self.not_ready();
        }
        let b = self.ptr(self.add(g, 0x38)?)?;
        let hp = self.ptr(self.add(g, 0x40)?)?;
        if b == 0 || hp == 0 {
            return self.not_ready();
        }
        let mode = self.i32(self.add(b, 0x64)?)?;
        self.gameplay_mode = mode;
        let x = self.ptr(self.add(b, 0x1c)?)?;
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
        let h = self.process.read_f64(self.add(hp, 0x1c)?)?;
        let smooth = self.process.read_f64(self.add(hp, 0x14)?)?;
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
            mods: self.with_random_seed(
                mods(self.i32(self.add(x, 0xc)?)? ^ self.i32(self.add(x, 8)?)?),
                r,
                mode,
            )?,
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
        let mut requests = Vec::with_capacity(length);
        for (pointer, state) in pointers[..length * 4]
            .chunks_exact(4)
            .zip(states.iter_mut())
        {
            let button = u32::from_le_bytes(pointer.try_into().unwrap()) as usize;
            requests.push(ReadRequest::new(self.add(button, 0x14)?, state));
        }
        self.process.read_many(&mut requests)?;
        drop(requests);

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
        let items = self.ptr(self.add(b, 4)?)?;
        let size = self.process.read_i32(self.add(b, 0xc)?)?.max(0) as i64;
        let mut index = last.max(0);
        let mut errors = HitErrorArray::default();
        while index < size {
            let e = self.i32(self.add(items, LEADER_START + 4 * index as usize)?)?;
            if !(-10_000..=10_000).contains(&e) {
                break;
            }
            let value = e as f64;
            errors.all.push(value);
            errors.press.push(value);
            index += 1
        }
        Ok(ReadStatus::Ready(HitErrors {
            index,
            errors,
            release_correction: None,
        }))
    }
    fn global(&mut self) -> MemoryRead<Global> {
        let menu = mods(self.ptr2(self.patterns.menu_mods_ptr)? as i64);
        let chat = self
            .process
            .read_i8(self.ptr(self.patterns.chat_checker_ptr)?)? as i64;
        let watching = self
            .process
            .read_u8(self.ptr(self.add(self.patterns.can_run_slowly_addr, 0x46)?)?)?
            == 1;
        let game_time = self.ptr2(self.patterns.game_time_ptr)? as i64;
        let status = self.ptr2(self.patterns.status_ptr)? as i64;
        let play_time = if status == 3 {
            0.0
        } else {
            self.i32(self.ptr(self.add(self.patterns.play_time_addr, 5)?)?)? as f64
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
            play_time,
            game_time,
            menu_mods: menu,
            skin_folder: skin,
            memory_songs_folder: songs,
        }))
    }
    fn global_precise(&mut self) -> MemoryRead<GlobalPrecise> {
        let status = self.ptr2(self.patterns.status_ptr)? as i64;
        let time = if status == 3 {
            0.0
        } else {
            self.i32(self.ptr(self.add(self.patterns.play_time_addr, 5)?)?)? as f64
        };
        Ok(ReadStatus::Ready(GlobalPrecise { status, time }))
    }
    fn menu(&mut self, prev: &str) -> MemoryRead<Menu> {
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
    fn mp3_length(&mut self) -> MemoryRead<Mp3Length> {
        let b = self.ptr2(self.patterns.get_audio_length_ptr)?;
        Ok(ReadStatus::Ready(js_round_number(
            self.process.read_f64(self.add(b, 4)?)?,
        )))
    }
    fn tourney(&mut self) -> MemoryRead<Tourney> {
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
        let len = self.process.read_i32(self.add(items, 4)?)?.max(0) as usize;
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
            let Ok(size) = self.process.read_i32(self.add(msgs, 0xc)?) else {
                continue;
            };
            let n = size.max(0) as usize;
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
    fn leaderboard(&mut self, mode: i64) -> MemoryRead<Leaderboard> {
        let r = self.ruleset()?;
        let base = self.ptr(self.add(r, 0x78)?)?;
        if base == 0 {
            return Ok(ReadStatus::Ready((false, None, vec![])));
        }
        let addr = self.process.read_i32(self.add(base, 0x24)?)?.max(0) as usize;
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
        let n = self.process.read_i32(self.add(list, 0xc)?)?.max(0) as usize;
        if n < 1 {
            return Ok(ReadStatus::Ready((visible, current, vec![])));
        }
        let items = self.ptr(self.add(list, 4)?)?;
        let mut out = vec![];
        for i in 0..n {
            match self
                .leaderboard_player(self.ptr(self.add(items, LEADER_START + 4 * i)?)?, mode)?
            {
                Some(v) => out.push(v),
                None => break,
            }
        }
        Ok(ReadStatus::Ready((visible, current, out)))
    }
    fn ranked_play(&mut self) -> MemoryRead<RankedPlay> {
        Ok(ReadStatus::Unsupported(
            "Stable: ranked play not implemented",
        ))
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
            if setting_truthy(&value) {
                out.insert(path, value);
            }
        }
        for record in self.setting_records(bp, &self.binding_positions)? {
            let Some(record) = record else { continue };
            let key = i32::from_le_bytes(record[0..4].try_into().unwrap());
            let value = i32::from_le_bytes(record[12..16].try_into().unwrap());
            if key == 0 || value == 0 {
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

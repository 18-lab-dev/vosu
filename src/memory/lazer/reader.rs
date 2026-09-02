use super::*;

fn read_many_key_fields<const WIDTH: usize>(
    process: &ProcessHandle,
    addresses: &[usize; 18],
    destinations: &mut [[u8; WIDTH]; 18],
    len: usize,
) -> Result<(), MemoryError> {
    let [
        destination0,
        destination1,
        destination2,
        destination3,
        destination4,
        destination5,
        destination6,
        destination7,
        destination8,
        destination9,
        destination10,
        destination11,
        destination12,
        destination13,
        destination14,
        destination15,
        destination16,
        destination17,
    ] = destinations;
    let mut requests = [
        ReadRequest::new(addresses[0], destination0),
        ReadRequest::new(addresses[1], destination1),
        ReadRequest::new(addresses[2], destination2),
        ReadRequest::new(addresses[3], destination3),
        ReadRequest::new(addresses[4], destination4),
        ReadRequest::new(addresses[5], destination5),
        ReadRequest::new(addresses[6], destination6),
        ReadRequest::new(addresses[7], destination7),
        ReadRequest::new(addresses[8], destination8),
        ReadRequest::new(addresses[9], destination9),
        ReadRequest::new(addresses[10], destination10),
        ReadRequest::new(addresses[11], destination11),
        ReadRequest::new(addresses[12], destination12),
        ReadRequest::new(addresses[13], destination13),
        ReadRequest::new(addresses[14], destination14),
        ReadRequest::new(addresses[15], destination15),
        ReadRequest::new(addresses[16], destination16),
        ReadRequest::new(addresses[17], destination17),
    ];
    process.read_many(&mut requests[..len])
}

impl SynchronizedMemoryReader for LazerMemory {
    fn reader_synchronization(&self) -> ReaderSynchronization {
        ReaderSynchronization {
            game_base: self.game_base_address(),
            current_screen: self.current_screen_address(),
        }
    }
    fn synchronize_capture_config(
        &mut self,
        _show_mp_commands: bool,
        _read_mania_scroll_speed: bool,
    ) {
        self.validated_game_base.store(0, Ordering::Relaxed);
        self.capture_api.store(UNCACHED_POINTER, Ordering::Relaxed);
        self.capture_spectator_client
            .store(UNCACHED_POINTER, Ordering::Relaxed);
    }

    fn synchronize_precise(&mut self, state: ReaderSynchronization, mode: i64) {
        self.set_resolved_state(
            state.game_base.unwrap_or_default(),
            state.current_screen.unwrap_or_default(),
        );
        self.set_read_context_mode(mode);
    }
}

impl LazerMemory {
    fn precise_key_player(&mut self) -> Result<usize, MemoryError> {
        let screen = self.current_screen;
        if let Some(cache) = self.precise_player.as_mut()
            && !cache.key_used
        {
            cache.key_used = true;
            if cache.screen == screen {
                return Ok(cache.player);
            }
        }
        self.player()
    }

    fn precise_hit_player(&mut self) -> Result<usize, MemoryError> {
        let screen = self.current_screen;
        if let Some(cache) = self.precise_player.as_mut()
            && !cache.hit_used
        {
            cache.hit_used = true;
            if cache.screen == screen {
                return Ok(cache.player);
            }
        }
        self.player()
    }
}

impl MemoryReader for LazerMemory {
    fn audio_velocity_base(&mut self) -> MemoryRead<AudioVelocityBase> {
        let logo = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_game_osu_game.osu_logo,
        )?)?;
        let visualizer = self.ptr(add(
            logo,
            self.offsets.osu_game_screens_menu_osu_logo.visualizer,
        )?)?;
        let array = self.ptr(add(
            visualizer,
            self.offsets
                .osu_game_screens_menu_logo_visualisation
                .frequency_amplitudes,
        )?)?;
        if array == 0 {
            return self.not_ready();
        }
        let mut out = Vec::with_capacity(40);
        for index in 0..40 {
            out.push(self.process.read_f32(add(array, 0x10 + 4 * index)?)?.into());
        }
        Ok(ReadStatus::Ready(out))
    }
    fn user(&mut self) -> MemoryRead<User> {
        let api = self.api()?;
        if api == 0 {
            return self.not_ready();
        }
        let state = self.ptr(add(
            api,
            self.offsets.osu_game_online_api_apiaccess.local_user_state,
        )?)?;
        let bindable = self.ptr(add(
            state,
            self.offsets.osu_game_online_api_local_user_state.local_user,
        )?)?;
        let user = if bindable == 0 {
            0
        } else {
            self.ptr(add(bindable, 0x20)?)?
        };
        if user == 0 {
            return self.not_ready();
        }
        Ok(ReadStatus::Ready(self.read_user_at(user)?))
    }
    fn result_screen(&mut self) -> MemoryRead<ResultScreen> {
        if !self.check_result(self.current_screen)? {
            return self.not_ready();
        }
        let bindable = self.ptr(add(
            self.current_screen,
            self.offsets
                .osu_game_screens_ranking_solo_results_screen
                .selected_score,
        )?)?;
        let score = self.ptr(add(bindable, 0x20)?)?;
        if score == 0 {
            return self.not_ready();
        }
        let o = &self.offsets.osu_game_scoring_score_info;
        let id = self
            .process
            .read_i64(add(score, o.online_id_k_backing_field)?)?
            .max(
                self.process
                    .read_i64(add(score, o.legacy_online_id_k_backing_field)?)?,
            ) as f64;
        let date = net_date(&self.process, add(score, o.date_k_backing_field + 8)?)?;
        Ok(ReadStatus::Ready(self.result_from_score(score, id, date)?))
    }
    fn gameplay(
        &mut self,
        _play_time: f64,
        _first_object_time: Option<i64>,
    ) -> MemoryRead<Gameplay> {
        if self.player_loading {
            return self.not_ready();
        }
        let player = required_non_null(self.player()?, "gameplay player", self.current_screen)?;
        let hp = self.ptr(add(
            player,
            self.offsets
                .osu_game_screens_play_player
                .health_processor_k_backing_field,
        )?)?;
        let bind = self.ptr(add(
            hp,
            self.offsets
                .osu_game_rulesets_osu_scoring_osu_health_processor
                .health,
        )?)?;
        let health = self.process.read_f64(add(bind, 0x40)?)? * 200.0;
        let retries = self.i32(add(
            player,
            self.offsets.osu_game_screens_play_solo_player.restart_count,
        )?)?;
        Ok(ReadStatus::Ready(self.read_score(
            self.score_info(player)?,
            health,
            retries,
        )?))
    }
    fn key_overlay(&mut self, _mode: i64) -> MemoryRead<KeyOverlay> {
        if self.player_loading {
            return Ok(ReadStatus::Ready(Vec::new()));
        }
        let player = required_non_null(
            self.precise_key_player()?,
            "key overlay player",
            self.current_screen,
        )?;
        let overlay = self.ptr(add(
            player,
            self.offsets
                .osu_game_screens_play_player
                .hudoverlay_k_backing_field,
        )?)?;
        let controller = self.ptr(add(
            overlay,
            self.offsets
                .osu_game_screens_play_hudoverlay
                .input_count_controller,
        )?)?;
        let bindable = self.ptr(add(
            controller,
            self.offsets
                .osu_game_screens_play_hud_input_count_controller
                .triggers,
        )?)?;
        let collection = self.ptr(add(bindable, 0x18)?)?;
        let (size, items) = self.list_info(collection)?;
        const BUTTON_COUNT: usize = KeyOverlayButton::lazer_button_count();
        if size > BUTTON_COUNT {
            return Err(corrupt("managed collection", collection, "invalid size"));
        }

        let mut trigger_bytes = [0u8; BUTTON_COUNT * size_of::<usize>()];
        self.process
            .read_bytes_into(items, &mut trigger_bytes[..size * size_of::<usize>()])?;
        let mut triggers = [0; BUTTON_COUNT];
        for (trigger, bytes) in triggers[..size]
            .iter_mut()
            .zip(trigger_bytes.chunks_exact(size_of::<usize>()))
        {
            *trigger = usize::from_le_bytes(bytes.try_into().unwrap());
        }

        let mut count_bindable_addresses = [0; BUTTON_COUNT];
        let mut pressed_addresses = [0; BUTTON_COUNT];
        for (index, trigger) in triggers[..size].iter().copied().enumerate() {
            count_bindable_addresses[index] = add(trigger, 0x208)?;
            pressed_addresses[index] = add(trigger, 0x1f4)?;
        }
        let mut count_bindable_bytes = [[0u8; size_of::<usize>()]; BUTTON_COUNT];
        let mut pressed_bytes = [[0u8; 1]; BUTTON_COUNT];
        read_many_key_fields(
            &self.process,
            &count_bindable_addresses,
            &mut count_bindable_bytes,
            size,
        )?;
        read_many_key_fields(&self.process, &pressed_addresses, &mut pressed_bytes, size)?;

        let mut count_addresses = [0; BUTTON_COUNT];
        for (index, bytes) in count_bindable_bytes[..size].iter().enumerate() {
            count_addresses[index] = add(usize::from_le_bytes(*bytes), 0x40)?;
        }
        let mut count_bytes = [[0u8; 4]; BUTTON_COUNT];
        read_many_key_fields(&self.process, &count_addresses, &mut count_bytes, size)?;

        let mut out = Vec::with_capacity(size);
        for index in 0..size {
            out.push(KeyOverlayButton {
                name: KeyOverlayButton::lazer_name(index),
                is_pressed: pressed_bytes[index][0] == 1,
                count: i32::from_le_bytes(count_bytes[index]) as i64,
            })
        }
        Ok(ReadStatus::Ready(out))
    }
    fn hit_errors(&mut self, last: i64) -> MemoryRead<HitErrors> {
        if self.player_loading {
            return Ok(ReadStatus::Ready(HitErrors {
                index: 0,
                errors: HitErrorArray::default(),
                release_corrections: Vec::new(),
                stable_replay_frames: None,
            }));
        }
        let p = required_non_null(
            self.precise_hit_player()?,
            "hit errors player",
            self.current_screen,
        )?;
        let processor = self.ptr(add(
            p,
            self.offsets
                .osu_game_screens_play_player
                .score_processor_k_backing_field,
        )?)?;
        let list = self.ptr(add(
            processor,
            self.offsets
                .osu_game_rulesets_scoring_score_processor
                .hit_events,
        )?)?;
        let (size, items) = self.hybrid_list_info(list)?;
        Ok(ReadStatus::Ready(
            self.read_hit_error_suffix(items, size, last)?,
        ))
    }
    fn global(&mut self) -> MemoryRead<Global> {
        self.precise_player = None;
        self.selected_gamemode = self.read_gamemode()?;
        self.current_screen = self.get_current_screen()?;
        if self.current_screen == 0 {
            return self.not_ready();
        }
        if let Some(menu_mods) = self.selected_mods()? {
            self.menu_mods = menu_mods;
        }
        let player = self.player()?;
        let is_loader = if player == 0 {
            self.check_player_loader(self.current_screen)?
        } else {
            false
        };
        if player != 0 {
            let spectator = self.check_spectator(self.current_screen)?;
            let replay = self.check_watching_replay(self.current_screen, player)?;
            if replay && spectator {
                self.watching_replay = false;
                self.spectating = true
            } else if replay {
                self.watching_replay = true;
                self.spectating = false
            }
        } else if !is_loader {
            self.watching_replay = false;
            self.spectating = false
        }
        if player != 0 {
            let hud = self.ptr(add(
                player,
                self.offsets
                    .osu_game_screens_play_player
                    .hudoverlay_k_backing_field,
            )?)?;
            let show = self.ptr(add(
                hud,
                self.offsets
                    .osu_game_screens_play_hudoverlay
                    .show_hud_k_backing_field,
            )?)?;
            self.show_interface = self.process.read_u8(add(show, 0x40)?)? == 1
        }
        let is_multi_spectating =
            self.multi_room()? != 0 && self.check_multi_spectator(self.current_screen)?;
        let chat = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_game_osu_game.chat_overlay,
        )?)?;
        let state = self.ptr(add(
            chat,
            self.offsets.osu_game_overlays_chat_overlay.state,
        )?)?;
        let chat_status = self.i32(add(state, 0x40)?)?;
        let storage = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_game_osu_game_base.storage_k_backing_field,
        )?)?;
        let wrapped = self.ptr(add(
            storage,
            self.offsets
                .osu_game_io_wrapped_storage
                .underlying_storage_k_backing_field,
        )?)?;
        let base_path = self.string_at(add(
            wrapped,
            self.offsets
                .osu_framework_platform_storage
                .base_path_k_backing_field,
        )?)?;
        let files = Path::new(&base_path)
            .join("files")
            .to_string_lossy()
            .into_owned();
        let (status, play_time) = match self.global_precise()? {
            ReadStatus::Ready(value) => (value.status, value.time),
            ReadStatus::NotReady => return self.not_ready(),
            ReadStatus::Unchanged => return Ok(ReadStatus::Unchanged),
            ReadStatus::Unsupported(reason) => return Ok(ReadStatus::Unsupported(reason)),
        };
        self.precise_player = None;
        Ok(ReadStatus::Ready(Global {
            is_watching_replay: self.watching_replay,
            is_replay_ui_hidden: !self.replay_settings_overlay,
            is_multi_spectating,
            show_interface: self.show_interface,
            chat_status,
            status,
            paused: self.gameplay_paused_for_player(player)?,
            play_time,
            game_time: 0,
            menu_mods: self.menu_mods.clone(),
            skin_folder: String::new(),
            memory_songs_folder: files,
        }))
    }
    fn global_precise(&mut self) -> MemoryRead<GlobalPrecise> {
        self.precise_player = None;
        if self.current_screen == 0 {
            return self.not_ready();
        }
        let player = self.player()?;
        let is_loader = if player == 0 {
            self.check_player_loader(self.current_screen)?
        } else {
            false
        };
        let playing = player != 0 || is_loader;
        if playing {
            let spectator = self.check_spectator(self.current_screen)?;
            let replay = self.check_watching_replay(self.current_screen, player)?;
            if replay && spectator {
                self.watching_replay = false;
                self.spectating = true
            } else if replay {
                self.watching_replay = true;
                self.spectating = false
            }
        } else {
            self.watching_replay = false;
            self.spectating = false
        }
        let status = if playing {
            GameState::Play.value()
        } else {
            let is_result = self.check_result(self.current_screen)?;
            let is_song_select = self.check_song_select(self.current_screen)?;
            let is_editor = self.check_editor(self.current_screen)?;
            let is_multi_select = self.check_multi_select(self.current_screen)?;
            let multi_room = self.multi_room()?;
            if is_song_select {
                GameState::SelectPlay.value()
            } else if is_result {
                GameState::ResultScreen.value()
            } else if is_editor {
                GameState::Edit.value()
            } else if is_multi_select {
                GameState::SelectMulti.value()
            } else if multi_room != 0 {
                GameState::Lobby.value()
            } else {
                GameState::Menu.value()
            }
        };
        self.status = status as i64;
        self.player_loading = is_loader;
        let time = js_round_number(self.current_time_for_player(player)?);
        self.precise_player = (player != 0).then_some(PrecisePlayerCache {
            screen: self.current_screen,
            player,
            key_used: false,
            hit_used: false,
        });
        Ok(ReadStatus::Ready(GlobalPrecise {
            status: status as i64,
            time,
        }))
    }
    fn menu(&mut self, previous: &str) -> MemoryRead<Menu> {
        let (info, set) = self.current_beatmap()?;
        if info == 0 {
            return self.not_ready();
        }
        let gamemode = self.read_gamemode()?;
        let b = &self.offsets.osu_game_beatmaps_beatmap_info;
        let raw_status = self
            .process
            .read_i32(add(info, b.status_int_k_backing_field)?)?;
        let ranked = lazer_ranked_status(raw_status);
        let hash = self.string_at(add(info, b.md5_hash_k_backing_field)?)?;
        if hash == previous {
            return Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
                gamemode,
                ranked_status: ranked,
            })));
        }
        let meta = self.ptr(add(info, b.metadata_k_backing_field)?)?;
        let diff = self.ptr(add(info, b.difficulty_k_backing_field)?)?;
        let m = &self.offsets.osu_game_beatmaps_beatmap_metadata;
        let d = &self.offsets.osu_game_beatmaps_beatmap_difficulty;
        let author = self.ptr(add(meta, m.author_k_backing_field)?)?;
        let author = required_non_null(author, "beatmap author", meta)?;
        let creator = self.string_at(add(
            author,
            self.offsets
                .osu_game_models_realm_user
                .username_k_backing_field,
        )?)?;
        let files = self.beatmap_files(set)?;
        let audio_name = self.string_at(add(meta, m.audio_file_k_backing_field)?)?;
        let background_name = self.string_at(add(meta, m.background_file_k_backing_field)?)?;
        let audio_hash = files.get(&audio_name).map(String::as_str).unwrap_or("");
        let background_hash = files
            .get(&background_name)
            .map(String::as_str)
            .unwrap_or("");
        Ok(ReadStatus::Ready(Menu::Full(MenuFull {
            gamemode,
            checksum: hash,
            filename: lazer_path(&self.string_at(add(info, b.hash_k_backing_field)?)?),
            plays: 0,
            artist: self.string_at(add(meta, m.artist_k_backing_field)?)?,
            artist_original: self.string_at(add(meta, m.artist_unicode_k_backing_field)?)?,
            title: self.string_at(add(meta, m.title_k_backing_field)?)?,
            title_original: self.string_at(add(meta, m.title_unicode_k_backing_field)?)?,
            ar: self
                .process
                .read_f32(add(diff, d.approach_rate_k_backing_field)?)? as f64,
            cs: self
                .process
                .read_f32(add(diff, d.circle_size_k_backing_field)?)? as f64,
            hp: self
                .process
                .read_f32(add(diff, d.drain_rate_k_backing_field)?)? as f64,
            od: self
                .process
                .read_f32(add(diff, d.overall_difficulty_k_backing_field)?)? as f64,
            audio_filename: lazer_path(audio_hash),
            audio_file_mimetype: mime(&audio_name),
            background_filename: lazer_path(background_hash),
            background_file_mimetype: mime(&background_name),
            folder: String::new(),
            creator,
            difficulty: self.string_at(add(info, b.difficulty_name_k_backing_field)?)?,
            map_id: self.i32(add(info, b.online_id_k_backing_field)?)?,
            set_id: self.i32(add(
                set,
                self.offsets
                    .osu_game_beatmaps_beatmap_set_info
                    .online_id_k_backing_field,
            )?)?,
            ranked_status: ranked,
            object_count: self.i32(add(info, b.total_object_count_k_backing_field)?)?,
        })))
    }
    fn mp3_length(&mut self) -> MemoryRead<Mp3Length> {
        let clock = self.beatmap_clock()?;
        let dec = self.ptr(add(
            clock,
            self.offsets
                .osu_game_beatmaps_framed_beatmap_clock
                .decoupled_track,
        )?)?;
        let source = self.ptr(add(
            dec,
            self.offsets
                .osu_framework_timing_decoupling_framed_clock
                .source_k_backing_field,
        )?)?;
        Ok(ReadStatus::Ready(js_round_number(self.process.read_f64(
            add(source, self.offsets.osu_framework_audio_track_track.length)?,
        )?)))
    }
    fn tourney(&mut self) -> MemoryRead<Tourney> {
        Ok(ReadStatus::Unsupported("Lazer: tourney not implemented"))
    }
    fn tourney_chat(&mut self, _: &[TourneyManagerChatItem]) -> MemoryRead<TourneyChat> {
        Ok(ReadStatus::Unsupported(
            "Lazer: tourney chat not implemented",
        ))
    }
    fn tourney_user(&mut self) -> MemoryRead<TourneyUser> {
        Ok(ReadStatus::Unsupported(
            "Lazer: tourney user not implemented",
        ))
    }
    fn leaderboard(&mut self, _mode: i64) -> MemoryRead<Leaderboard> {
        let p = required_non_null(self.player()?, "leaderboard player", self.current_screen)?;
        let personal = self.leaderboard_score(self.score_info(p)?, -1)?;
        Ok(ReadStatus::Ready((
            self.leaderboard_visible,
            Some(personal),
            vec![],
        )))
    }
    // Direct comparisons intentionally treat NaN as neither below nor above the valid range.
    #[allow(clippy::manual_range_contains)]
    fn ranked_play(&mut self) -> MemoryRead<RankedPlay> {
        let room = self.multiplayer_room()?;
        if room == 0 {
            return self.not_ready();
        }
        let state = self.ptr(add(
            room,
            self.offsets
                .osu_game_online_multiplayer_multiplayer_room
                .match_state_k_backing_field,
        )?)?;
        if state == 0 {
            return self.not_ready();
        }
        let o = &self
            .offsets
            .osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state;
        let stage = self
            .process
            .read_i32(add(state, o.stage_k_backing_field)?)?;
        let Some(stage_enum) = RankedPlayStage::from_value(stage) else {
            return self.not_ready();
        };
        let round = self.i32(add(state, o.current_round_k_backing_field)?)?;
        let stars = {
            if !(1..=50).contains(&round) {
                return self.not_ready();
            }
            self.process
                .read_f64(add(state, o.star_rating_k_backing_field)?)?
        };
        let damage = {
            if stars < 0.0 || stars > 20.0 {
                return self.not_ready();
            }
            self.process
                .read_f64(add(state, o.damage_multiplier_k_backing_field)?)?
        };
        if !(1..=50).contains(&round) || stars < 0.0 || stars > 20.0 || damage < 0.0 {
            return self.not_ready();
        }
        let dict = self.ptr(add(state, o.users_k_backing_field)?)?;
        if dict == 0 {
            return self.not_ready();
        }
        let entries = self.ptr(add(dict, 0x10)?)?;
        if entries == 0 {
            return self.not_ready();
        }
        let count = self.process.read_i32(add(entries, 8)?)?;
        if !(0..=128).contains(&count) {
            return self.not_ready();
        }
        let uo = &self
            .offsets
            .osu_game_online_multiplayer_match_types_ranked_play_ranked_play_user_info;
        let mut users = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let e = add(entries, 0x10 + i * 0x18)?;
            if self.process.read_i32(add(e, 8)?)? == 0 {
                continue;
            }
            let u = required_non_null(self.ptr(e)?, "ranked play user", e)?;
            users.push(RankedPlayUserInfo {
                id: self.i32(add(e, 0x10)?)?,
                rating: self.i32(add(u, uo.rating_k_backing_field)?)? as f64,
                life: self.i32(add(u, uo.life_k_backing_field)?)? as f64,
                rounds_won: self.i32(add(u, uo.rounds_won_k_backing_field)?)?,
                damage_multiplier: self
                    .process
                    .read_f64(add(u, uo.damage_multiplier_k_backing_field)?)?,
            })
        }
        let active = self
            .process
            .read_nullable_i32(add(state, o.active_user_id_k_backing_field)?)?
            .map(i64::from)
            .filter(|id| *id != 0)
            .unwrap_or(-1);
        let winning = self
            .process
            .read_nullable_i32(add(state, o.winning_user_id_k_backing_field)?)?
            .map(i64::from)
            .filter(|id| *id != 0)
            .unwrap_or(-1);
        Ok(ReadStatus::Ready(RankedPlay {
            stage: stage_enum,
            current_round: round,
            star_rating: stars,
            damage_multiplier: damage,
            active_user_id: active,
            winning_user_id: winning,
            users,
        }))
    }
    fn spectating_data(
        &mut self,
        show_mp_commands: bool,
    ) -> MemoryRead<crate::model::memory::LazerSpectator> {
        LazerMemory::spectating_data(self, show_mp_commands)
    }
    fn settings(&mut self) -> MemoryRead<Settings> {
        let mut values = self.read_osu_config()?;
        values.insert(
            "client.version".into(),
            Value::String(self.offsets.osu_version.clone()),
        );
        let skin = || -> Result<String, MemoryError> {
            let manager = self.ptr(add(
                self.game_base()?,
                self.offsets
                    .osu_game_osu_game_base
                    .skin_manager_k_backing_field,
            )?)?;
            let current = self.ptr(add(
                manager,
                self.offsets.osu_game_skinning_skin_manager.current_skin,
            )?)?;
            let value = self.ptr(add(current, 0x20)?)?;
            self.string_at(add(
                value,
                self.offsets.osu_game_skinning_skin.name_k_backing_field,
            )?)
        };
        if let Ok(name) = skin() {
            values.insert("skin.name".into(), Value::String(name));
        }
        #[cfg(windows)]
        let _ = self.read_windows_input_settings(&mut values);
        Ok(ReadStatus::Ready(values))
    }
}

impl LazerMemory {
    pub(super) fn read_hit_error_suffix(
        &self,
        items: usize,
        size: usize,
        last: i64,
    ) -> Result<HitErrors, MemoryError> {
        let start_index = last.max(0) as usize;
        if start_index > size {
            return Ok(HitErrors {
                index: size as i64,
                errors: HitErrorArray::default(),
                release_corrections: Vec::new(),
                stable_replay_frames: None,
            });
        }
        let mut index = start_index;
        let mut suffix_events = Vec::new();
        while index < size {
            let event = self.read_hit_event(add(items, index * 0x40)?)?;
            match event {
                Some(HitErrorEvent::Timing { value, .. }) if hit_error_out_of_range(value) => {
                    break;
                }
                Some(event) => suffix_events.push(event),
                None => {}
            }
            index += 1;
        }

        let suffix_tails: Vec<_> = suffix_events
            .iter()
            .filter_map(|event| match event {
                HitErrorEvent::ManiaHoldAggregate { tail } => Some(*tail),
                HitErrorEvent::Timing { .. } => None,
            })
            .collect();
        let mut errors = HitErrorArray::default();
        for event in &suffix_events {
            if let HitErrorEvent::Timing { value, object } = *event {
                errors.all.push(value);
                if self.selected_gamemode == 3 && suffix_tails.contains(&object) {
                    errors.release.push(value);
                } else {
                    errors.press.push(value);
                }
            }
        }

        let mut correction_tails = Vec::new();
        if self.selected_gamemode == 3 && start_index > 0 {
            for tail in suffix_tails {
                let timing_is_in_suffix = suffix_events.iter().any(
                    |event| matches!(event, HitErrorEvent::Timing { object, .. } if *object == tail),
                );
                if !timing_is_in_suffix && !correction_tails.contains(&tail) {
                    correction_tails.push(tail);
                }
            }
        }

        let mut prior_events = Vec::new();
        let mut found_targets = vec![false; correction_tails.len()];
        let mut prior_index = start_index;
        while prior_index > 0 && found_targets.iter().any(|found| !found) {
            prior_index -= 1;
            if let Some(event) = self.read_hit_event(add(items, prior_index * 0x40)?)? {
                if let HitErrorEvent::Timing { value, object } = event
                    && !hit_error_out_of_range(value)
                {
                    for (tail, found) in correction_tails.iter().zip(&mut found_targets) {
                        if object == *tail {
                            *found = true;
                        }
                    }
                }
                prior_events.push(event);
            }
        }
        prior_events.reverse();

        let mut release_corrections = Vec::new();
        for tail in correction_tails {
            let Some(target_index) = prior_events.iter().rposition(
                |event| matches!(event, HitErrorEvent::Timing { object, .. } if *object == tail),
            ) else {
                continue;
            };
            let HitErrorEvent::Timing { value, .. } = prior_events[target_index] else {
                unreachable!();
            };
            if hit_error_out_of_range(value) {
                continue;
            }
            let interval = &prior_events[target_index + 1..];
            let interval_tails: Vec<_> = interval
                .iter()
                .filter_map(|event| match event {
                    HitErrorEvent::ManiaHoldAggregate { tail } => Some(*tail),
                    HitErrorEvent::Timing { .. } => None,
                })
                .collect();
            let press_offset = interval
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        HitErrorEvent::Timing { value, object }
                            if !hit_error_out_of_range(*value) && !interval_tails.contains(object)
                    )
                })
                .count();
            release_corrections.push(ReleaseCorrection {
                value,
                press_offset,
            });
        }

        Ok(HitErrors {
            index: index as i64,
            errors,
            release_corrections,
            stable_replay_frames: None,
        })
    }
}

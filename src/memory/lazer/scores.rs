use super::*;

impl LazerMemory {
    pub fn spectating_data(&mut self, show_mp_commands: bool) -> MemoryRead<LazerSpectator> {
        let gameplay = self.ptr(add(
            self.current_screen,
            self.offsets
                .osu_game_screens_spectate_spectator_screen
                .gameplay_states,
        )?)?;
        if gameplay == 0 {
            return self.not_ready();
        }
        let gameplay_entries = self.ptr(add(gameplay, 0x10)?)?;
        let gameplay_count = self.process.read_i32(add(gameplay, 0x38)?)?;
        if !(0..=MAX_COLLECTION as i32).contains(&gameplay_count) {
            return Err(corrupt(
                "spectator gameplay states",
                gameplay,
                "invalid count",
            ));
        }
        let client = self.ptr(add(
            self.game_base()?,
            self.offsets
                .osu_game_osu_game_base
                .multiplayer_client_k_backing_field,
        )?)?;
        let room = if client == 0 {
            0
        } else {
            self.ptr(add(
                client,
                self.offsets
                    .osu_game_online_multiplayer_multiplayer_client
                    .room,
            )?)?
        };
        if room == 0 {
            return self.not_ready();
        }
        let users = self.ptr(add(room, 0x10)?)?;
        let user_items = self.list_items(users, false, 8)?;
        let mut states = BTreeMap::new();
        let uo = &self
            .offsets
            .osu_game_online_multiplayer_multiplayer_room_user;
        for current in user_items {
            let id = self.i32(add(current, uo.user_id)?)?;
            let match_state = self.ptr(add(current, uo.match_state_k_backing_field)?)?;
            let team = if match_state == 0 {
                MultiplayerTeamType::None
            } else if self.process.read_i32(add(match_state, 8)?)? == 0 {
                MultiplayerTeamType::Red
            } else {
                MultiplayerTeamType::Blue
            };
            states.insert(
                id,
                (
                    team,
                    self.process
                        .read_i32(add(current, uo.state_k_backing_field)?)?,
                ),
            );
        }
        let mut clients = Vec::new();
        for index in 0..gameplay_count as usize {
            let current = add(gameplay_entries, 0x10 + index * 0x18)?;
            let state = self.ptr(current)?;
            if state == 0 {
                continue;
            }
            let score = required_non_null(self.ptr(add(state, 8)?)?, "spectator score", state)?;
            let info = required_non_null(self.ptr(add(score, 8)?)?, "spectator score info", score)?;
            let api_user = self.ptr(add(info, self.offsets.osu_game_scoring_score_info.user)?)?;
            let user = self.read_user_at(api_user)?;
            let (team, user_state) = spectator_user_state(&states, user.id)?;
            let score = self.read_score(info, 0.0, 0)?;
            let result_screen = if user_state == 7 {
                Some(self.result_from_score(
                    info,
                    -1.0,
                    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                )?)
            } else {
                None
            };
            clients.push(LazerSpectatorEntry {
                team,
                user,
                result_screen,
                score: Some(score),
            });
        }
        let channel_id = self.i32(add(
            room,
            self.offsets
                .osu_game_online_multiplayer_multiplayer_room
                .channel_id_k_backing_field,
        )?)?;
        let manager = required_non_null(
            self.ptr(add(
                self.game_base()?,
                self.offsets.osu_game_osu_game.channel_manager,
            )?)?,
            "spectator channel manager",
            room,
        )?;
        let joined = required_non_null(
            self.ptr(add(
                manager,
                self.offsets
                    .osu_game_online_chat_channel_manager
                    .joined_channels,
            )?)?,
            "spectator joined channels",
            manager,
        )?;
        let collection = required_non_null(
            self.ptr(add(joined, 0x18)?)?,
            "spectator channel collection",
            joined,
        )?;
        let channels = self.list_items(collection, false, 8)?;
        let mut channel = 0;
        for candidate in channels {
            if self.process.read_i64(add(
                candidate,
                self.offsets.osu_game_online_chat_channel.id,
            )?)? == channel_id
            {
                channel = candidate;
                break;
            }
        }
        let mut chat = Vec::new();
        if channel != 0 {
            let sorted = required_non_null(
                self.ptr(add(
                    channel,
                    self.offsets.osu_game_online_chat_channel.messages,
                )?)?,
                "spectator messages",
                channel,
            )?;
            let list =
                required_non_null(self.ptr(add(sorted, 8)?)?, "spectator message list", sorted)?;
            for message in self.list_items(list, false, 8)? {
                let content = self.string_at(add(
                    message,
                    self.offsets.osu_game_online_chat_message.content,
                )?)?;
                if !show_mp_commands && content.starts_with("!mp") {
                    continue;
                }
                let sender = self.ptr(add(
                    message,
                    self.offsets.osu_game_online_chat_message.sender,
                )?)?;
                let user = self.read_user_at(sender)?;
                let time = net_date(
                    &self.process,
                    add(
                        message,
                        self.offsets.osu_game_online_chat_message.timestamp + 8,
                    )?,
                )?;
                chat.push(TourneyManagerChatItem {
                    time,
                    name: user.name,
                    content,
                });
            }
        }
        Ok(ReadStatus::Ready(LazerSpectator {
            chat,
            spectating_clients: clients,
        }))
    }

    pub(super) fn read_score(
        &self,
        score_info: usize,
        health: f64,
        retries: i64,
    ) -> Result<Gameplay, MemoryError> {
        if score_info == 0 {
            return Err(corrupt("score info", score_info, "null pointer"));
        }
        let o = &self.offsets.osu_game_scoring_score_info;
        let statistics = self.statistics(score_info, false)?;
        let maximum_statistics = self.statistics(score_info, true)?;
        let realm_user = self.ptr(add(score_info, o.realm_user_k_backing_field)?)?;
        let mut username = self.string_at(add(
            realm_user,
            self.offsets
                .osu_game_models_realm_user
                .username_k_backing_field,
        )?)?;
        username = match username.as_str() {
            "Autoplay" => "osu!".into(),
            "osu!salad" => "salad!".into(),
            "osu!topus" => "osu!topus!".into(),
            _ => username,
        };
        let ruleset = self.ptr(add(score_info, o.ruleset_k_backing_field)?)?;
        let mode = self.i32(add(
            ruleset,
            self.offsets
                .osu_game_rulesets_ruleset_info
                .online_id_k_backing_field,
        )?)?;
        let mut combo = 0;
        let p = self.player()?;
        if p != 0 {
            let processor = self.ptr(add(
                p,
                self.offsets
                    .osu_game_screens_play_player
                    .score_processor_k_backing_field,
            )?)?;
            let bindable = self.ptr(add(
                processor,
                self.offsets
                    .osu_game_rulesets_osu_scoring_osu_score_processor
                    .combo,
            )?)?;
            combo = self.i32(add(bindable, 0x40)?)?
        }
        let mut raw_bytes = [0u8; 8];
        let mut accuracy_bytes = [0u8; 8];
        let mut max_combo_bytes = [0u8; 4];
        self.process.read_many(&mut [
            ReadRequest::new(
                add(score_info, o.total_score_k_backing_field)?,
                &mut raw_bytes,
            ),
            ReadRequest::new(
                add(score_info, o.accuracy_k_backing_field)?,
                &mut accuracy_bytes,
            ),
            ReadRequest::new(
                add(score_info, o.max_combo_k_backing_field)?,
                &mut max_combo_bytes,
            ),
        ])?;
        let raw = js_long(i64::from_le_bytes(raw_bytes));
        let accuracy = f64::from_le_bytes(accuracy_bytes) * 100.0;
        let max_combo = i32::from_le_bytes(max_combo_bytes) as i64;
        let score = if self.scoring_display_mode == 1 {
            display_score(mode, raw, object_count(&maximum_statistics))
        } else {
            raw
        };
        Ok(Gameplay {
            failed: health <= 0.0,
            retries,
            player_name: username,
            mods: self.mods(score_info)?,
            mode,
            score,
            player_hp_smooth: health,
            player_hp: health,
            accuracy,
            statistics,
            maximum_statistics,
            combo,
            max_combo,
            pp: None,
        })
    }

    pub(super) fn result_from_score(
        &self,
        score_info: usize,
        online_id: f64,
        date: String,
    ) -> Result<ResultScreen, MemoryError> {
        let s = self.read_score(score_info, 1.0, 0)?;
        Ok(ResultScreen {
            online_id,
            player_name: s.player_name,
            mods: s.mods,
            mode: s.mode,
            accuracy: s.accuracy,
            max_combo: s.max_combo,
            score: s.score,
            statistics: s.statistics,
            maximum_statistics: s.maximum_statistics,
            date,
        })
    }

    pub(super) fn read_user_at(&self, user: usize) -> Result<User, MemoryError> {
        let o = &self.offsets.osu_game_online_api_requests_responses_apiuser;
        let id = self.i32(add(user, o.id_k_backing_field)?)?;
        if id == 0 {
            return Ok(User {
                name: "Guest".into(),
                accuracy: 0.0,
                ranked_score: 0.0,
                id: 0,
                level: 0.0,
                play_count: 0,
                play_mode: 0,
                rank: 0,
                country_code: 0,
                performance_points: 0.0,
                raw_bancho_status: 0,
                background_colour: 0xffff_ffffu32 as i64,
                raw_login_status: 0,
                matchmaking: None,
            });
        }
        let stats = self.ptr(add(user, o.statistics)?)?;
        let (pp, accuracy, ranked, level, plays, rank) = if stats == 0 {
            (0.0, 0.0, 0.0, 0.0, 0, 0)
        } else {
            let u = &self.offsets.osu_game_users_user_statistics;
            (
                self.process.read_dotnet_decimal(add(stats, u.pp + 8)?)?,
                self.process.read_f64(add(stats, u.accuracy)?)?,
                self.process.read_i64(add(stats, u.ranked_score)?)? as f64,
                self.i32(add(stats, u.level)?)? as f64,
                self.i32(add(stats, u.play_count)?)?,
                self.i32(add(stats, u.global_rank + 4)?)?,
            )
        };
        let mode_name = self.string_at(add(
            user,
            self.offsets
                .osu_game_online_api_requests_responses_apime
                .play_mode,
        )?)?;
        let mode = match mode_name.as_str() {
            "taiko" => 1,
            "fruits" => 2,
            "mania" => 3,
            _ => 0,
        };
        let mm_array = self.ptr(add(user, o.matchmaking_statistics)?)?;
        let matchmaking = if mm_array == 0 || self.process.read_i32(add(mm_array, 8)?)? <= 0 {
            None
        } else {
            let mm = self.ptr(add(mm_array, 0x10)?)?;
            let m = &self
                .offsets
                .osu_game_online_api_requests_responses_apiuser_matchmaking_statistics;
            let rank = if self.process.read_u8(add(mm, m.rank_k_backing_field)?)? == 0 {
                None
            } else {
                Some(self.i32(add(mm, m.rank_k_backing_field + 4)?)?)
            };
            Some(MatchmakingStats {
                rating: self.i32(add(mm, m.rating_k_backing_field)?)? as f64,
                rank,
                plays: self.i32(add(mm, m.plays_k_backing_field)?)?,
                wins: self.i32(add(mm, m.first_placements_k_backing_field)?)?,
                is_provisional: self
                    .process
                    .read_u8(add(mm, m.is_rating_provisional_k_backing_field)?)?
                    != 0,
            })
        };
        Ok(User {
            name: self.string_at(add(user, o.username_k_backing_field)?)?,
            accuracy,
            ranked_score: ranked,
            id,
            level,
            play_count: plays,
            play_mode: mode,
            rank,
            country_code: country_code(&self.string_at(add(user, o.country_code_string)?)?),
            performance_points: pp,
            raw_bancho_status: 0,
            background_colour: 0xffff_ffffu32 as i64,
            raw_login_status: 0,
            matchmaking,
        })
    }

    pub(super) fn leaderboard_score(
        &self,
        score_info: usize,
        index: i64,
    ) -> Result<LeaderboardPlayer, MemoryError> {
        let o = &self.offsets.osu_game_scoring_score_info;
        let realm = self.ptr(add(score_info, o.realm_user_k_backing_field)?)?;
        let statistics = self.statistics(score_info, false)?;
        Ok(LeaderboardPlayer {
            user_id: self.i32(add(
                realm,
                self.offsets
                    .osu_game_models_realm_user
                    .online_id_k_backing_field,
            )?)?,
            name: self.string_at(add(
                realm,
                self.offsets
                    .osu_game_models_realm_user
                    .username_k_backing_field,
            )?)?,
            score: js_long(
                self.process
                    .read_i64(add(score_info, o.total_score_k_backing_field)?)?,
            ),
            combo: self.i32(add(score_info, o.combo_k_backing_field)?)?,
            accuracy: self
                .process
                .read_f64(add(score_info, o.accuracy_k_backing_field)?)?
                * 100.0,
            max_combo: self.i32(add(score_info, o.max_combo_k_backing_field)?)?,
            mods: self.mods(score_info)?,
            statistics,
            team: 0,
            position: index + 1,
            is_passing: self
                .process
                .read_u8(add(score_info, o.passed_k_backing_field)?)?
                != 0,
        })
    }

    pub(super) fn read_hit_event(
        &self,
        address: usize,
    ) -> Result<Option<HitErrorEvent>, MemoryError> {
        let mut object_bytes = [0u8; 8];
        let mut timing_result_bytes = [0u8; 12];
        let error_address = add(address, 0x10)?;
        let result_address = add(address, 0x18)?;
        if self
            .process
            .read_many(&mut [
                ReadRequest::new(address, &mut object_bytes),
                ReadRequest::new(error_address, &mut timing_result_bytes),
            ])
            .is_err()
        {
            // Preserve field-granular reads and their original error precedence.
            let (error_bytes, result_bytes) = timing_result_bytes.split_at_mut(8);
            self.process.read_many(&mut [
                ReadRequest::new(address, &mut object_bytes),
                ReadRequest::new(error_address, error_bytes),
                ReadRequest::new(result_address, result_bytes),
            ])?;
        }
        let object = u64::from_le_bytes(object_bytes) as usize;
        if object == 0 {
            return Ok(None);
        }
        let type_ptr = self.ptr(object)?;
        let size = self.process.read_i32(add(type_ptr, 4)?)?;
        let mode = self.selected_gamemode;
        let allowed_size = match mode {
            0 => matches!(size, 0xe8 | 0xe0),
            1 => matches!(size, 0x68 | 0x50),
            3 => matches!(size, 0x58 | 0x88),
            _ => false,
        };
        if !allowed_size {
            return Ok(None);
        }
        // DynamicMethod ldflda on installed osu! 2026.711/CoreCLR 8.0.28 and
        // 2026.726/CoreCLR 8.0.29 AppImages measured the 0x88 BaseSize
        // HoldNote.<Tail>k__BackingField at +0x60; +0x68 is Body.
        if mode == 3 && size == 0x88 {
            return Ok(Some(HitErrorEvent::ManiaHoldAggregate {
                tail: self.ptr(add(object, 0x60)?)?,
            }));
        }
        let result = i32::from_le_bytes(timing_result_bytes[8..].try_into().unwrap());
        let allowed = match mode {
            0 => matches!(result, 3 | 2 | 5 | 10),
            1 => matches!(result, 5 | 3 | 2),
            3 => matches!(result, 2..=6),
            _ => false,
        };
        if !allowed {
            return Ok(None);
        }
        Ok(Some(HitErrorEvent::Timing {
            value: f64::from_le_bytes(timing_result_bytes[..8].try_into().unwrap()),
            object,
        }))
    }
}

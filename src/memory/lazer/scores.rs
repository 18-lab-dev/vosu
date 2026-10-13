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
        if !(0..=MAX_ROOM_USERS as i32).contains(&gameplay_count) {
            return Err(corrupt(
                "spectator gameplay states",
                gameplay,
                "invalid count",
            ));
        }
        let client = self.root(GameRoot::MultiplayerClient)?;
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
        let user_items = self.list_items_bounded(users, false, 8, MAX_ROOM_USERS)?;
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
        let channels = self.list_items_bounded(collection, false, 8, MAX_ROOM_USERS)?;
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
        let mut retained_chat_bytes = 0usize;
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
            for message in self.list_items_bounded(list, false, 8, MAX_CHAT_MESSAGES)? {
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
                retained_chat_bytes = retained_chat_bytes
                    .saturating_add(time.len())
                    .saturating_add(user.name.len())
                    .saturating_add(content.len());
                if retained_chat_bytes > MAX_RETAINED_CHAT_BYTES {
                    return Err(corrupt(
                        "spectator messages",
                        list,
                        "retained text budget exceeded",
                    ));
                }
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
        let username = display_username(
            self.string_at(add(
                realm_user,
                self.offsets
                    .osu_game_models_realm_user
                    .username_k_backing_field,
            )?)?,
        );
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
        if let Some((user, _)) = self.read_user_batched(user, None) {
            return Ok(user);
        }
        self.read_user_sequential(user)
    }

    fn guest_user() -> User {
        User {
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
        }
    }

    /// [`Self::read_user_sequential`] with one native batch per pointer level, optionally
    /// also decoding the managed string at `avatar` (an offset on the same API user object;
    /// `None` for a null string). `None` defers to the sequential path.
    pub(super) fn read_user_batched(
        &self,
        user: usize,
        avatar: Option<usize>,
    ) -> Option<(User, Option<String>)> {
        if self.process.pointer_size() != 8 {
            return None;
        }
        let o = &self.offsets.osu_game_online_api_requests_responses_apiuser;
        let mut id = [0u8; 4];
        let mut objects = [[0u8; 8]; 6];
        let mut stage = Stage::<7>::new();
        let [stats, mode, mm, name, country, avatar_object] = &mut objects;
        stage.push(add(user, o.id_k_backing_field).ok()?, &mut id);
        stage.push(add(user, o.statistics).ok()?, stats);
        stage.push(
            add(
                user,
                self.offsets
                    .osu_game_online_api_requests_responses_apime
                    .play_mode,
            )
            .ok()?,
            mode,
        );
        stage.push(add(user, o.matchmaking_statistics).ok()?, mm);
        stage.push(add(user, o.username_k_backing_field).ok()?, name);
        stage.push(add(user, o.country_code_string).ok()?, country);
        if let Some(offset) = avatar {
            stage.push(add(user, offset).ok()?, avatar_object);
        }
        stage.read(&self.process)?;
        let id = le_i32(&id) as i64;
        let [stats, mode, mm_array, name, country, avatar_object] = pointers(objects)?;
        if id == 0 {
            let avatar = match avatar_object {
                0 => None,
                object => Some(self.string_prefetched(object)?),
            };
            return Some((Self::guest_user(), avatar));
        }

        let u = &self.offsets.osu_game_users_user_statistics;
        let stat_fields = [
            (u.accuracy, 8),
            (u.ranked_score, 8),
            (u.level, 4),
            (u.play_count, 4),
            (u.global_rank + 4, 4),
        ];
        let mut stat_bytes = [[0u8; 8]; 5];
        let mut mm_header = [0u8; 0x10];
        let mut heads = [[0u8; STRING_HEAD]; 4];
        let mut head_spans = [(0usize, 0usize); 4];
        for (span, object) in head_spans
            .iter_mut()
            .zip([mode, name, country, avatar_object])
        {
            if object != 0 {
                *span = self.string_head_span(object)?;
            }
        }
        {
            let mut requests: [ReadRequest<'_>; 10] =
                std::array::from_fn(|_| ReadRequest::new(0, &mut []));
            let mut count = 0;
            if stats != 0 {
                for (bytes, (offset, len)) in stat_bytes.iter_mut().zip(stat_fields) {
                    requests[count] = ReadRequest::new(add(stats, offset).ok()?, &mut bytes[..len]);
                    count += 1;
                }
            }
            for (head, (address, len)) in heads.iter_mut().zip(head_spans) {
                if len != 0 {
                    requests[count] = ReadRequest::new(address, &mut head[..len]);
                    count += 1;
                }
            }
            if mm_array != 0 {
                // The count at +8 and first element at +0x10, read together.
                requests[count] = ReadRequest::new(add(mm_array, 8).ok()?, &mut mm_header);
                count += 1;
            }
            self.process.read_many(&mut requests[..count]).ok()?;
        }
        let (pp, accuracy, ranked, level, plays, rank) = if stats == 0 {
            (0.0, 0.0, 0.0, 0.0, 0, 0)
        } else {
            (
                self.process
                    .read_dotnet_decimal(add(stats, u.pp + 8).ok()?)
                    .ok()?,
                le_f64(&stat_bytes[0]),
                le_i64(&stat_bytes[1]) as f64,
                le_i32(&stat_bytes[2]) as f64,
                le_i32(&stat_bytes[3]) as i64,
                le_i32(&stat_bytes[4]) as i64,
            )
        };
        let mut strings = [mode, name, country, avatar_object]
            .into_iter()
            .zip(heads.iter().zip(head_spans))
            .map(|(object, (head, (_, len)))| {
                if object == 0 {
                    Some(String::new())
                } else {
                    self.string_from_head(object, &head[..len])
                }
            });
        let mode_name = strings.next()??;
        let name = strings.next()??;
        let country = strings.next()??;
        let avatar = strings.next()??;
        let avatar = (avatar_object != 0).then_some(avatar);
        let mode = match mode_name.as_str() {
            "taiko" => 1,
            "fruits" => 2,
            "mania" => 3,
            _ => 0,
        };
        let matchmaking = if mm_array == 0 || le_i32(&mm_header) <= 0 {
            None
        } else {
            let mm = le_pointer(&mm_header[8..])?;
            let m = &self
                .offsets
                .osu_game_online_api_requests_responses_apiuser_matchmaking_statistics;
            let mut fields = [[0u8; 4]; 6];
            {
                let [has_rank, rank, rating, plays, wins, provisional] = &mut fields;
                self.process
                    .read_many(&mut [
                        ReadRequest::new(add(mm, m.rank_k_backing_field).ok()?, &mut has_rank[..1]),
                        ReadRequest::new(add(mm, m.rank_k_backing_field + 4).ok()?, rank),
                        ReadRequest::new(add(mm, m.rating_k_backing_field).ok()?, rating),
                        ReadRequest::new(add(mm, m.plays_k_backing_field).ok()?, plays),
                        ReadRequest::new(add(mm, m.first_placements_k_backing_field).ok()?, wins),
                        ReadRequest::new(
                            add(mm, m.is_rating_provisional_k_backing_field).ok()?,
                            &mut provisional[..1],
                        ),
                    ])
                    .ok()?;
            }
            Some(MatchmakingStats {
                rating: le_i32(&fields[2]) as f64,
                rank: (fields[0][0] != 0).then(|| le_i32(&fields[1]) as i64),
                plays: le_i32(&fields[3]) as i64,
                wins: le_i32(&fields[4]) as i64,
                is_provisional: fields[5][0] != 0,
            })
        };
        let user = User {
            name,
            accuracy,
            ranked_score: ranked,
            id,
            level,
            play_count: plays,
            play_mode: mode,
            rank,
            country_code: country_code(&country),
            performance_points: pp,
            raw_bancho_status: 0,
            background_colour: 0xffff_ffffu32 as i64,
            raw_login_status: 0,
            matchmaking,
        };
        Some((user, avatar))
    }

    fn read_user_sequential(&self, user: usize) -> Result<User, MemoryError> {
        let o = &self.offsets.osu_game_online_api_requests_responses_apiuser;
        let id = self.i32(add(user, o.id_k_backing_field)?)?;
        if id == 0 {
            return Ok(Self::guest_user());
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
        // The bounded prefix trades eight intervening bytes for one contiguous read.
        let mut event_bytes = [0u8; 28];
        let error_address = add(address, 0x10)?;
        let result_address = add(address, 0x18)?;
        if self.process.read_exact(address, &mut event_bytes).is_err() {
            // Preserve field-granular reads and their original error precedence.
            let (object_bytes, rest) = event_bytes.split_at_mut(8);
            let (_, timing_result_bytes) = rest.split_at_mut(8);
            let (error_bytes, result_bytes) = timing_result_bytes.split_at_mut(8);
            self.process.read_many(&mut [
                ReadRequest::new(address, object_bytes),
                ReadRequest::new(error_address, error_bytes),
                ReadRequest::new(result_address, result_bytes),
            ])?;
        }
        let object = u64::from_le_bytes(event_bytes[..8].try_into().unwrap()) as usize;
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
        let result = i32::from_le_bytes(event_bytes[24..].try_into().unwrap());
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
            value: f64::from_le_bytes(event_bytes[16..24].try_into().unwrap()),
            object,
        }))
    }
}

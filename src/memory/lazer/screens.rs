use super::*;

impl LazerMemory {
    pub(super) fn check_song_select(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        let offset = self
            .offsets
            .osu_game_screens_select_solo_song_select
            .as_ref()
            .map(|g| g.game_k_backing_field)
            .or_else(|| {
                self.offsets
                    .osu_game_screens_select_v2_solo_song_select
                    .as_ref()
                    .map(|g| g.game_k_backing_field)
            })
            .unwrap_or(
                self.offsets
                    .osu_game_screens_select_song_select
                    .game_k_backing_field,
            );
        Ok(self.screen_ptr(address, offset)? == self.game_base()?)
    }
    pub(super) fn check_player(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        let submitting = &self.offsets.osu_game_screens_play_submitting_player;
        let submitting_api = self.screen_ptr(address, submitting.api_k_backing_field)?;
        let api = self.api()?;
        if submitting_api == api
            && self.screen_ptr(address, submitting.spectator_client_k_backing_field)?
                == self.spectator_client()?
        {
            return Ok(true);
        }
        let replay = &self.offsets.osu_game_screens_play_player;
        Ok(self.screen_ptr(address, replay.api_k_backing_field)? == api
            && self.screen_ptr(address, replay.score_manager_k_backing_field)?
                == self.root(GameRoot::ScoreManager)?)
    }
    pub(super) fn check_player_loader(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        Ok(self.screen_ptr(
            address,
            self.offsets
                .osu_game_screens_osu_screen
                .logo_k_backing_field,
        )? == self.root(GameRoot::OsuLogo)?
            && self.screen_ptr(
                address,
                self.offsets
                    .osu_game_screens_play_player_loader
                    .leaderboard_manager_k_backing_field,
            )? == self.root(GameRoot::LeaderboardManager)?)
    }
    pub(super) fn check_result(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        Ok(self.screen_ptr(
            address,
            self.offsets
                .osu_game_screens_ranking_solo_results_screen
                .api_k_backing_field,
        )? == self.api()?
            && self.screen_ptr(
                address,
                self.offsets
                    .osu_game_screens_osu_screen
                    .logo_k_backing_field,
            )? == self.root(GameRoot::OsuLogo)?)
    }
    pub(super) fn check_spectator(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        let o = &self.offsets.osu_game_screens_play_spectator_player;
        Ok(self.screen_ptr(address, 0x3f0)? == self.api()?
            && self.screen_ptr(address, o.spectator_client_k_backing_field)?
                == self.spectator_client()?
            && self.screen_ptr(address, o.score)? != self.root(GameRoot::SessionStatics)?)
    }
    pub(super) fn check_watching_replay(
        &self,
        address: usize,
        player: usize,
    ) -> Result<bool, MemoryError> {
        if player == 0 || address == 0 {
            return Ok(false);
        }
        let drawable = self.screen_ptr(
            player,
            self.offsets
                .osu_game_screens_play_player
                .drawable_ruleset_k_backing_field,
        )?;
        Ok(drawable != 0
            && self.ptr(add(
                drawable,
                self.offsets
                    .osu_game_rulesets_ui_drawable_ruleset
                    .replay_score_k_backing_field,
            )?)? != 0
            && self.screen_ptr(
                address,
                self.offsets
                    .osu_game_screens_play_spectator_player
                    .spectator_client_k_backing_field,
            )? != self.spectator_client()?)
    }
    pub(super) fn check_editor(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        let o = &self.offsets.osu_game_screens_edit_editor;
        Ok(
            self.screen_ptr(address, o.api_k_backing_field)? == self.api()?
                && self.screen_ptr(address, o.realm_k_backing_field)?
                    == self.root(GameRoot::Realm)?,
        )
    }
    pub(super) fn check_multi_select(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        let client = self.root(GameRoot::MultiplayerClient)?;
        if client == 0 {
            return Ok(false);
        }
        let connected = self.ptr(add(
            client,
            self.offsets
                .osu_game_online_multiplayer_online_multiplayer_client
                .is_connected_k_backing_field,
        )?)?;
        if connected == 0 || self.process.read_u8(add(connected, 0x40)?)? != 1 {
            return Ok(false);
        }
        if self.ptr(add(
            client,
            self.offsets
                .osu_game_online_multiplayer_multiplayer_client
                .room,
        )?)? != 0
        {
            return Ok(false);
        }
        Ok(self.screen_ptr(
            address,
            self.offsets
                .osu_game_screens_online_play_online_play_screen
                .api_k_backing_field,
        )? == self.api()?
            && self.screen_ptr(
                address,
                self.offsets
                    .osu_game_screens_online_play_multiplayer_multiplayer
                    .client_k_backing_field,
            )? == client)
    }
    pub(super) fn check_multi_spectator(&self, address: usize) -> Result<bool, MemoryError> {
        if address == 0 {
            return Ok(false);
        }
        let logo = self.screen_ptr(
            address,
            self.offsets
                .osu_game_screens_osu_screen
                .logo_k_backing_field,
        )?;
        let spectator = self.screen_ptr(
            address,
            self.offsets
                .osu_game_screens_spectate_spectator_screen
                .spectator_client_k_backing_field,
        )?;
        let multiplayer = self.screen_ptr(
            address,
            self.offsets
                .osu_game_screens_online_play_multiplayer_spectate_multi_spectator_screen
                .multiplayer_client_k_backing_field,
        )?;
        Ok(logo == self.root(GameRoot::OsuLogo)?
            && spectator == self.root(GameRoot::SpectatorClient)?
            && multiplayer == self.root(GameRoot::MultiplayerClient)?)
    }
    /// The current screen when it is a player. The classification is cached for the capture,
    /// keyed by screen, like the game roots it compares against (see `roots.rs`).
    pub(super) fn player(&self) -> Result<usize, MemoryError> {
        let screen = self.current_screen;
        if screen == 0 {
            return Ok(0);
        }
        if self.capture_player_screen.load(Ordering::Relaxed) == screen {
            return Ok(self.capture_player.load(Ordering::Relaxed));
        }
        let player = if self.check_player(screen)? {
            screen
        } else {
            0
        };
        self.capture_player.store(player, Ordering::Relaxed);
        self.capture_player_screen.store(screen, Ordering::Relaxed);
        Ok(player)
    }
    pub(super) fn current_score(&self, player: usize) -> Result<usize, MemoryError> {
        if player == 0 {
            Ok(0)
        } else {
            self.screen_ptr(
                player,
                self.offsets
                    .osu_game_screens_play_player
                    .score_k_backing_field,
            )
        }
    }
    pub(super) fn score_info(&self, player: usize) -> Result<usize, MemoryError> {
        let score = self.current_score(player)?;
        if score == 0 {
            Ok(0)
        } else {
            self.ptr(add(score, 8)?)
        }
    }

    pub(super) fn mods(&self, score_info: usize) -> Result<CalculateMods, MemoryError> {
        if score_info == 0 {
            return Ok(default_mods());
        }
        let text = self.string_at(add(
            score_info,
            self.offsets
                .osu_game_scoring_score_info
                .mods_json_k_backing_field,
        )?)?;
        mods_from_json(score_info, &text)
    }

    pub(super) fn statistics(
        &self,
        score_info: usize,
        maximum: bool,
    ) -> Result<Statistics, MemoryError> {
        let o = &self.offsets.osu_game_scoring_score_info;
        let dict = self.ptr(add(
            score_info,
            if maximum {
                o.maximum_statistics
            } else {
                o.statistics
            },
        )?)?;
        self.statistics_dict(dict)
    }
    pub(super) fn statistics_dict(&self, dict: usize) -> Result<Statistics, MemoryError> {
        let mut s = empty_statistics();
        if dict == 0 {
            return Ok(s);
        }
        let entries = self.ptr(add(dict, 0x10)?)?;
        if entries == 0 {
            return Ok(s);
        }
        let count = self.process.read_i32(add(dict, 0x38)?)?;
        if !(0..=64).contains(&count) {
            return Err(corrupt("statistics dictionary", dict, "invalid size"));
        }
        if count > 0 {
            let mut bytes = [0u8; 64 * 0x10];
            let length = count as usize * 0x10;
            self.process
                .read_many(&mut [ReadRequest::new(add(entries, 0x10)?, &mut bytes[..length])])?;
            for entry in bytes[..length].chunks_exact(0x10) {
                let key = i32::from_le_bytes(entry[8..12].try_into().unwrap());
                let val = i32::from_le_bytes(entry[12..16].try_into().unwrap()) as i64;
                set_stat(&mut s, key, val)
            }
        }
        Ok(s)
    }
}

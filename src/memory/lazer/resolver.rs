use super::*;

impl LazerMemory {
    pub fn new(process: ProcessHandle, offsets: Arc<LazerOffsets>, pattern: LazerPattern) -> Self {
        Self {
            process,
            offsets,
            pattern,
            game_base_address: RwLock::new(0),
            validated_game_base: Cell::new(0),
            capture_api: Cell::new(None),
            capture_spectator_client: Cell::new(None),
            current_screen: 0,
            precise_player: None,
            selected_gamemode: 0,
            scoring_display_mode: 0,
            watching_replay: false,
            spectating: false,
            player_loading: false,
            status: GameState::Menu.value() as i64,
            menu_mods: default_mods(),
            replay_settings_overlay: true,
            show_interface: true,
            leaderboard_visible: false,
            mod_mappings: BTreeMap::new(),
            mapped_modes: [false; 4],
        }
    }

    pub fn game_base_address(&self) -> Option<usize> {
        let base = *self.game_base_address.read().ok()?;
        (base != 0).then_some(base)
    }
    pub fn current_screen_address(&self) -> Option<usize> {
        (self.current_screen != 0).then_some(self.current_screen)
    }

    /// Supplies the reducer-captured mode to a dedicated precise decoder.
    pub fn set_read_context_mode(&mut self, mode: i64) {
        self.selected_gamemode = mode;
    }

    pub fn set_resolved_state(&mut self, game_base: usize, current_screen: usize) {
        *self
            .game_base_address
            .write()
            .expect("lazer base lock poisoned") = game_base;
        self.validated_game_base.set(0);
        self.capture_api.set(None);
        self.capture_spectator_client.set(None);
        self.current_screen = current_screen;
        self.precise_player = None;
    }

    pub fn resolve_game_base(&mut self) -> Result<bool, MemoryError> {
        let mut resolved = None;
        self.process.scan_first_valid(
            &self.pattern.scaling_container_target_draw_size,
            |found| {
                let anchor = if self.pattern.offset >= 0 {
                    add(found, self.pattern.offset as usize)?
                } else {
                    found.checked_sub((-self.pattern.offset) as usize).ok_or(
                        MemoryError::AddressOverflow {
                            base: found,
                            offset: (-self.pattern.offset) as usize,
                        },
                    )?
                };
                resolved = self.resolve_game_base_from_anchor(anchor)?;
                Ok(resolved.is_some())
            },
        )?;
        if let Some(base) = resolved {
            *self
                .game_base_address
                .write()
                .map_err(|_| corrupt("lazer game base", base, "lock poisoned"))? = base;
            self.validated_game_base.set(base);
            return Ok(true);
        }
        Ok(false)
    }

    pub fn resolve_game_base_from_anchor(
        &self,
        anchor: usize,
    ) -> Result<Option<usize>, MemoryError> {
        let expected = self.offsets.effective_game_base_vtable();
        for delta in GAME_BASE_DELTAS {
            let Some(candidate_address) = anchor.checked_sub(delta) else {
                continue;
            };
            let attempt: Result<Option<usize>, MemoryError> = (|| {
                let opener = self.ptr(candidate_address)?;
                if opener == 0 {
                    return Ok(None);
                }
                let api = self.ptr(add(
                    opener,
                    self.offsets
                        .osu_game_online_chat_external_link_opener
                        .api_k_backing_field,
                )?)?;
                if api == 0 {
                    return Ok(None);
                }
                let game_base =
                    self.ptr(add(api, self.offsets.osu_game_online_api_apiaccess.game)?)?;
                if game_base == 0 {
                    return Ok(None);
                }
                let vtable = self.ptr(game_base)?;
                Ok(
                    (vtable != 0 && self.process.read_i64(vtable)? as usize == expected)
                        .then_some(game_base),
                )
            })();
            if let Ok(Some(base)) = attempt {
                return Ok(Some(base));
            }
        }
        Ok(None)
    }

    pub(super) fn game_base(&self) -> Result<usize, MemoryError> {
        let validated = self.validated_game_base.get();
        if validated != 0 {
            return Ok(validated);
        }

        let cached = *self
            .game_base_address
            .read()
            .map_err(|_| corrupt("lazer game base", 0, "lock poisoned"))?;
        if cached != 0
            && let Ok(vtable) = self.ptr(cached)
            && vtable != 0
            && self
                .process
                .read_i64(vtable)
                .ok()
                .map(|value| value as usize)
                == Some(self.offsets.effective_game_base_vtable())
        {
            self.validated_game_base.set(cached);
            return Ok(cached);
        }
        let mut resolved = None;
        self.process.scan_first_valid(
            &self.pattern.scaling_container_target_draw_size,
            |found| {
                let anchor = if self.pattern.offset >= 0 {
                    add(found, self.pattern.offset as usize)?
                } else {
                    found.checked_sub((-self.pattern.offset) as usize).ok_or(
                        MemoryError::AddressOverflow {
                            base: found,
                            offset: (-self.pattern.offset) as usize,
                        },
                    )?
                };
                resolved = self.resolve_game_base_from_anchor(anchor)?;
                Ok(resolved.is_some())
            },
        )?;
        if let Some(base) = resolved {
            *self
                .game_base_address
                .write()
                .map_err(|_| corrupt("lazer game base", base, "lock poisoned"))? = base;
            self.validated_game_base.set(base);
            return Ok(base);
        }
        Err(corrupt("lazer game base", cached, "failed to resolve"))
    }
    pub(super) fn ptr(&self, address: usize) -> Result<usize, MemoryError> {
        self.process.read_pointer(address)
    }
    pub(super) fn i32(&self, address: usize) -> Result<i64, MemoryError> {
        Ok(self.process.read_i32(address)? as i64)
    }
    pub(super) fn string_at(&self, address: usize) -> Result<String, MemoryError> {
        match self.process.read_managed_string_pointer(address)? {
            ReadStatus::Ready(s) => Ok(s),
            ReadStatus::NotReady => Ok(String::new()),
            _ => unreachable!(),
        }
    }
    pub(super) fn not_ready<T>(&self) -> MemoryRead<T> {
        Ok(ReadStatus::NotReady)
    }

    pub(super) fn get_current_screen(&self) -> Result<usize, MemoryError> {
        let base = self.game_base()?;
        let game = &self.offsets.osu_game_osu_game;
        let stack = self.ptr(add(
            base,
            game.screen_stack
                .unwrap_or(game.screen_stack_k_backing_field),
        )?)?;
        let list = self.ptr(add(
            stack,
            self.offsets.osu_framework_screens_screen_stack.stack,
        )?)?;
        let (size, items) = self.list_info(list)?;
        let mut current = 0;
        for i in 0..size {
            let slot = add(
                items,
                i.checked_mul(8)
                    .ok_or(MemoryError::AddressOverflow { base: i, offset: 8 })?,
            )?;
            current = self.ptr(slot)?;
        }
        Ok(current)
    }

    pub(super) fn list_info(&self, list: usize) -> Result<(usize, usize), MemoryError> {
        self.hybrid_list_info(list)
    }
    pub(super) fn hybrid_list_info(&self, list: usize) -> Result<(usize, usize), MemoryError> {
        if list == 0 {
            return Err(corrupt("managed collection", list, "null pointer"));
        }
        let marker = self.process.read_i32(add(list, 0x10)?)?;
        let (size, items) = if marker > 10_000_000 {
            (self.process.read_i32(add(list, 8)?)?, add(list, 0x10)?)
        } else {
            let array = self.ptr(add(list, 8)?)?;
            (marker, add(array, 0x10)?)
        };
        if !(0..=MAX_COLLECTION as i32).contains(&size) {
            return Err(corrupt("managed collection", list, "invalid size"));
        }
        Ok((size as usize, items))
    }

    pub(super) fn list_items(
        &self,
        list: usize,
        inline: bool,
        stride: usize,
    ) -> Result<Vec<usize>, MemoryError> {
        self.list_items_bounded(list, inline, stride, MAX_COLLECTION)
    }

    pub(super) fn list_items_bounded(
        &self,
        list: usize,
        inline: bool,
        stride: usize,
        max_size: usize,
    ) -> Result<Vec<usize>, MemoryError> {
        let (size, items) = self.list_info(list)?;
        if size > max_size {
            return Err(corrupt("managed collection", list, "invalid size"));
        }
        let mut out = Vec::with_capacity(size);
        for i in 0..size {
            let slot = add(
                items,
                i.checked_mul(stride).ok_or(MemoryError::AddressOverflow {
                    base: i,
                    offset: stride,
                })?,
            )?;
            out.push(if inline { slot } else { self.ptr(slot)? });
        }
        Ok(out)
    }

    pub(super) fn current_beatmap(&self) -> Result<(usize, usize), MemoryError> {
        let bindable = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_game_osu_game_base.beatmap_k_backing_field,
        )?)?;
        let working = self.ptr(add(bindable, 0x20)?)?;
        let o = &self
            .offsets
            .osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap;
        Ok((
            self.ptr(add(working, o.beatmap_info)?)?,
            self.ptr(add(working, o.beatmap_set_info)?)?,
        ))
    }

    pub(super) fn beatmap_clock(&self) -> Result<usize, MemoryError> {
        self.ptr(add(
            self.game_base()?,
            self.offsets.osu_game_osu_game_base.beatmap_clock,
        )?)
    }

    pub(super) fn current_time_for_player(&self, player: usize) -> Result<f64, MemoryError> {
        match self.gameplay_clock_time_for_player(player) {
            Ok(Some(time)) => Ok(time),
            Ok(None) | Err(_) => self.clock_current_time(self.beatmap_clock()?),
        }
    }

    pub(super) fn gameplay_clock_time_for_player(
        &self,
        player: usize,
    ) -> Result<Option<f64>, MemoryError> {
        let Some(clock) = self.gameplay_clock_for_player(player)? else {
            return Ok(None);
        };
        match self.clock_current_time(clock) {
            Ok(time) => Ok(Some(time)),
            Err(_) => Ok(None),
        }
    }

    pub(super) fn gameplay_clock_for_player(
        &self,
        player: usize,
    ) -> Result<Option<usize>, MemoryError> {
        if player == 0 {
            return Ok(None);
        }
        let Some(container_offset) = self
            .offsets
            .osu_game_screens_play_player
            .gameplay_clock_container_k_backing_field
            .filter(|offset| *offset != 0)
        else {
            return Ok(None);
        };
        let Some(clock_offset) = self
            .offsets
            .osu_game_screens_play_gameplay_clock_container
            .as_ref()
            .map(|group| group.gameplay_clock)
            .filter(|offset| *offset != 0)
        else {
            return Ok(None);
        };
        let Ok(container) = self.ptr(add(player, container_offset)?) else {
            return Ok(None);
        };
        if container == 0 {
            return Ok(None);
        }
        let Ok(clock) = self.ptr(add(container, clock_offset)?) else {
            return Ok(None);
        };
        Ok((clock != 0).then_some(clock))
    }

    pub(super) fn clock_current_time(&self, clock: usize) -> Result<f64, MemoryError> {
        // Gameplay FramedBeatmapClocks advance their DecouplingFramedClock during
        // lead-in even when the shared track/menu beatmapClock is still stopped.
        if let Ok(decoupled) = self.ptr(add(
            clock,
            self.offsets
                .osu_game_beatmaps_framed_beatmap_clock
                .decoupled_track,
        )?) {
            // osu.Framework.Timing.DecouplingFramedClock.<CurrentTime>k__BackingField
            // is a stable framework layout offset (not versioned in offsets.json).
            if decoupled != 0
                && let Ok(time) = self.process.read_f64(add(decoupled, 0x20)?)
            {
                return Ok(time);
            }
        }

        let source = self.ptr(add(
            clock,
            self.offsets
                .osu_game_beatmaps_framed_beatmap_clock
                .final_clock_source,
        )?)?;
        self.process.read_f64(add(
            source,
            self.offsets
                .osu_framework_timing_framed_clock
                .current_time_k_backing_field,
        )?)
    }

    /// Reads lazer's authoritative pause bindable when player gameplay is loaded.
    pub(super) fn gameplay_paused_for_player(
        &self,
        player: usize,
    ) -> Result<Option<bool>, MemoryError> {
        if player == 0 || self.player_loading {
            return Ok(None);
        }

        if let Some(is_paused_offset) = self
            .offsets
            .osu_game_rulesets_ui_drawable_ruleset
            .is_paused
            .filter(|offset| *offset != 0)
            && let Ok(drawable) = self.ptr(add(
                player,
                self.offsets
                    .osu_game_screens_play_player
                    .drawable_ruleset_k_backing_field,
            )?)
            && drawable != 0
            && let Ok(bind) = self.ptr(add(drawable, is_paused_offset)?)
            && bind != 0
            && let Ok(value) = self.process.read_u8(add(bind, 0x40)?)
        {
            return Ok(Some(value == 1));
        }

        let Some(container_offset) = self
            .offsets
            .osu_game_screens_play_player
            .gameplay_clock_container_k_backing_field
            .filter(|offset| *offset != 0)
        else {
            return Ok(None);
        };
        let Some(is_paused_offset) = self
            .offsets
            .osu_game_screens_play_gameplay_clock_container
            .as_ref()
            .map(|group| group.is_paused)
            .filter(|offset| *offset != 0)
        else {
            return Ok(None);
        };
        let Ok(container) = self.ptr(add(player, container_offset)?) else {
            return Ok(None);
        };
        if container == 0 {
            return Ok(None);
        }
        let Ok(bind) = self.ptr(add(container, is_paused_offset)?) else {
            return Ok(None);
        };
        if bind == 0 {
            return Ok(None);
        }
        let Ok(value) = self.process.read_u8(add(bind, 0x40)?) else {
            return Ok(None);
        };
        Ok(Some(value == 1))
    }

    pub(super) fn config_store(&self, config: usize) -> Result<IndexMap<i32, usize>, MemoryError> {
        if config == 0 {
            return Err(corrupt("config store", config, "null config"));
        }
        let store = self.ptr(add(config, 0x20)?)?;
        if store == 0 {
            return Err(corrupt("config store", config, "null store"));
        }
        let entries = self.ptr(add(store, 0x10)?)?;
        let count = self.process.read_i32(add(store, 0x38)?)?;
        if !(0..=MAX_COLLECTION as i32).contains(&count) {
            return Err(corrupt("config store", store, "invalid count"));
        }
        if count > 0 && entries == 0 {
            return Err(corrupt("config store", store, "null entries"));
        }
        let mut result = IndexMap::new();
        for index in 0..count as usize {
            let entry = add(entries, 0x10 + index * 0x18)?;
            let key = self.process.read_i32(add(entry, 0x10)?)?;
            let value = self.ptr(entry)?;
            if value != 0 {
                result.insert(key, add(value, 0x40)?);
            }
        }
        Ok(result)
    }
}

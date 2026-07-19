use super::*;

impl LazerMemory {
    pub fn new(process: ProcessHandle, offsets: Arc<LazerOffsets>, pattern: LazerPattern) -> Self {
        Self {
            process,
            offsets,
            pattern,
            game_base_address: RwLock::new(0),
            current_screen: 0,
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
        self.current_screen = current_screen;
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
        let items = self.list_items(list, false, 8)?;
        Ok(items.last().copied().unwrap_or(0))
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
    pub(super) fn current_time(&self) -> Result<f64, MemoryError> {
        let clock = self.beatmap_clock()?;
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

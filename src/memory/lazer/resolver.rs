use super::*;

impl LazerMemory {
    pub fn new(process: ProcessHandle, offsets: Arc<LazerOffsets>, pattern: LazerPattern) -> Self {
        let screen_span = screen_span(&offsets);
        Self {
            process,
            offsets,
            pattern,
            game_base_address: RwLock::new(0),
            validated_game_base: AtomicUsize::new(0),
            capture_roots: std::array::from_fn(|_| AtomicUsize::new(UNCACHED_POINTER)),
            capture_roots_loaded: AtomicBool::new(false),
            capture_player_screen: AtomicUsize::new(0),
            capture_player: AtomicUsize::new(0),
            screen_span,
            capture_screen: AtomicUsize::new(0),
            capture_screen_pending: AtomicBool::new(false),
            capture_screen_words: std::array::from_fn(|_| AtomicU64::new(0)),
            capture_multiplayer_room: AtomicUsize::new(UNCACHED_POINTER),
            capture_track: AtomicUsize::new(UNCACHED_POINTER),
            capture_user: AtomicUsize::new(UNCACHED_POINTER),
            capture_ranked_state: AtomicUsize::new(UNCACHED_POINTER),
            captured_personal: None,
            captured_beatmap: None,
            captured_track_length: None,
            menu_global: MenuGlobal::default(),
            captured_room: None,
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
            config_scratch: Vec::new(),
            batched_skin: None,
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
        self.validated_game_base.store(0, Ordering::Relaxed);
        self.reset_capture_roots(false);
        self.captured_personal = None;
        self.captured_beatmap = None;
        self.captured_track_length = None;
        self.menu_global = MenuGlobal::default();
        self.captured_room = None;
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
            self.validated_game_base.store(base, Ordering::Relaxed);
            self.reset_capture_roots(false);
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
        let validated = self.validated_game_base.load(Ordering::Relaxed);
        if validated != 0 {
            return Ok(validated);
        }

        let cached = *self
            .game_base_address
            .read()
            .map_err(|_| corrupt("lazer game base", 0, "lock poisoned"))?;
        let valid = cached != 0
            && self.ptr(cached).is_ok_and(|vtable| {
                vtable != 0
                    && match self.vtable_check_with_roots(cached, vtable) {
                        Some(valid) => valid,
                        None => {
                            self.process
                                .read_i64(vtable)
                                .ok()
                                .map(|value| value as usize)
                                == Some(self.offsets.effective_game_base_vtable())
                        }
                    }
            });
        if valid {
            self.validated_game_base.store(cached, Ordering::Relaxed);
            return Ok(cached);
        }
        self.resolve_game_base_by_scan(cached)
    }

    fn resolve_game_base_by_scan(&self, cached: usize) -> Result<usize, MemoryError> {
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
            self.validated_game_base.store(base, Ordering::Relaxed);
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
        let object = self.ptr(address)?;
        if let Some(value) = self.string_prefetched(object) {
            return Ok(value);
        }
        // The exact reader reproduces every error the prefetch declined to classify.
        match self.process.read_managed_string(object)? {
            ReadStatus::Ready(s) => Ok(s),
            ReadStatus::NotReady => Ok(String::new()),
            _ => unreachable!(),
        }
    }

    /// Reads a managed string's length and leading UTF-16 units in one native read;
    /// `None` defers failures and invalid strings to the exact reader.
    pub(super) fn string_prefetched(&self, object: usize) -> Option<String> {
        if object == 0 {
            return Some(String::new());
        }
        let (address, len) = self.string_head_span(object)?;
        let mut head = [0u8; STRING_HEAD];
        self.process.read_exact(address, &mut head[..len]).ok()?;
        self.string_from_head(object, &head[..len])
    }
    pub(super) fn not_ready<T>(&self) -> MemoryRead<T> {
        Ok(ReadStatus::NotReady)
    }

    pub(super) fn get_current_screen(&self) -> Result<usize, MemoryError> {
        let stack = self.root(GameRoot::ScreenStack)?;
        let list = self.ptr(add(
            stack,
            self.offsets.osu_framework_screens_screen_stack.stack,
        )?)?;
        let (size, items) = self.list_info(list)?;
        if let Some(current) = self.last_slot_pointer_batched(items, size) {
            return Ok(current);
        }
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

    /// Reads every pointer slot of a list in one native read, as the sequential loop does
    /// one slot at a time, and returns the last (0 when empty).
    fn last_slot_pointer_batched(&self, items: usize, size: usize) -> Option<usize> {
        let mut bytes = [0u8; BATCHED_LIST_BYTES];
        let len = size.checked_mul(8).filter(|len| *len <= bytes.len())?;
        if self.process.pointer_size() != 8 {
            return None;
        }
        if len > 0 {
            self.process.read_exact(items, &mut bytes[..len]).ok()?;
        }
        let mut current = 0;
        for slot in bytes[..len].chunks_exact(8) {
            current = le_pointer(slot)?;
        }
        Some(current)
    }

    pub(super) fn list_info(&self, list: usize) -> Result<(usize, usize), MemoryError> {
        self.hybrid_list_info(list)
    }
    pub(super) fn hybrid_list_info(&self, list: usize) -> Result<(usize, usize), MemoryError> {
        if list == 0 {
            return Err(corrupt("managed collection", list, "null pointer"));
        }
        let marker_address = add(list, 0x10)?;
        let value_address = add(list, 8)?;
        let mut header = [0u8; 12];
        let (marker, value) = if usize::BITS == 64
            && self.process.pointer_size() == 8
            && self.process.read_exact(value_address, &mut header).is_ok()
        {
            (
                i32::from_le_bytes(header[8..12].try_into().unwrap()),
                u64::from_le_bytes(header[..8].try_into().unwrap()),
            )
        } else {
            // Sparse backends may expose only individual fields. Preserve the
            // original scalar sequence and its error when the span is unavailable.
            let marker = self.process.read_i32(marker_address)?;
            let value = if marker > 10_000_000 {
                self.process.read_i32(value_address)? as u64
            } else {
                self.ptr(value_address)? as u64
            };
            (marker, value)
        };
        list_shape(list, marker, value)
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
        if !inline && self.slot_pointers_batched(items, size, stride, &mut out) {
            return Ok(out);
        }
        out.clear();
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

    /// Reads the pointer at the start of each `stride`-byte slot from one native read of the
    /// slot span; `false` defers to per-slot reads.
    pub(super) fn slot_pointers_batched(
        &self,
        items: usize,
        size: usize,
        stride: usize,
        out: &mut Vec<usize>,
    ) -> bool {
        if size == 0 {
            return true;
        }
        if self.process.pointer_size() != 8 || stride < 8 {
            return false;
        }
        let mut bytes = [0u8; BATCHED_LIST_BYTES];
        let Some(len) = (size - 1)
            .checked_mul(stride)
            .and_then(|len| len.checked_add(8))
            .filter(|len| *len <= bytes.len())
        else {
            return false;
        };
        if self.process.read_exact(items, &mut bytes[..len]).is_err() {
            return false;
        }
        for index in 0..size {
            match le_pointer(&bytes[index * stride..]) {
                Some(pointer) => out.push(pointer),
                None => return false,
            }
        }
        true
    }

    pub(super) fn current_beatmap(&self) -> Result<(usize, usize), MemoryError> {
        let bindable = self.root(GameRoot::Beatmap)?;
        let working = self.ptr(add(bindable, 0x20)?)?;
        let o = &self
            .offsets
            .osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap;
        if usize::BITS == 64
            && self.process.pointer_size() == 8
            && let (Some(info), Some(set)) = (
                working.checked_add(o.beatmap_info),
                working.checked_add(o.beatmap_set_info),
            )
            && info.checked_add(8) == Some(set)
        {
            let mut pointers = [0; 16];
            if self.process.read_exact(info, &mut pointers).is_ok() {
                return Ok((
                    u64::from_le_bytes(pointers[..8].try_into().unwrap()) as usize,
                    u64::from_le_bytes(pointers[8..].try_into().unwrap()) as usize,
                ));
            }
        }
        // Preserve scalar sequencing for other layouts and field/address errors.
        Ok((
            self.ptr(add(working, o.beatmap_info)?)?,
            self.ptr(add(working, o.beatmap_set_info)?)?,
        ))
    }

    pub(super) fn beatmap_clock(&self) -> Result<usize, MemoryError> {
        self.root(GameRoot::BeatmapClock)
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
        add(player, container_offset)?;
        let Ok(container) = self.screen_ptr(player, container_offset) else {
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
        if let Some((prefetched, time)) = self.menu_global.beatmap_time
            && prefetched == clock
        {
            return Ok(time);
        }
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
            && let Ok(drawable) = {
                let offset = self
                    .offsets
                    .osu_game_screens_play_player
                    .drawable_ruleset_k_backing_field;
                add(player, offset)?;
                self.screen_ptr(player, offset)
            }
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
        add(player, container_offset)?;
        let Ok(container) = self.screen_ptr(player, container_offset) else {
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

/// Decodes a hybrid list header: a marker above ten million means the size is stored at +8
/// and the items inline at +0x10; otherwise the marker is the size and +8 the items array.
pub(super) fn list_shape(
    list: usize,
    marker: i32,
    value: u64,
) -> Result<(usize, usize), MemoryError> {
    let (size, items) = if marker > 10_000_000 {
        (value as i32, add(list, 0x10)?)
    } else {
        (marker, add(value as usize, 0x10)?)
    };
    if !(0..=MAX_COLLECTION as i32).contains(&size) {
        return Err(corrupt("managed collection", list, "invalid size"));
    }
    Ok((size as usize, items))
}

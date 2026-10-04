use super::*;

impl LazerMemory {
    pub(super) fn beatmap_files(
        &self,
        set: usize,
    ) -> Result<IndexMap<String, String>, MemoryError> {
        let mut result = IndexMap::new();
        if set == 0 {
            return Ok(result);
        }
        let files = self.ptr(add(set, 0x20)?)?;
        if files == 0 {
            return Ok(result);
        }
        let (size, items) = self.list_info(files)?;
        for index in 0..size {
            let current = self.ptr(add(items, index * 8)?)?;
            if current == 0 {
                return Err(corrupt("beatmap file", files, "null entry"));
            }
            let realm = self.ptr(add(current, 0x18)?)?;
            if realm == 0 {
                return Err(corrupt("beatmap file", current, "null file info"));
            }
            let hash = self.string_at(add(realm, 0x18)?)?;
            let name = self.string_at(add(current, 0x20)?)?;
            result.insert(name, hash);
        }
        Ok(result)
    }
    pub(super) fn read_mod_types(&self, list: usize) -> Result<Vec<usize>, MemoryError> {
        const MAX_MOD_TYPES: usize = 256;
        let mut types = Vec::with_capacity(32);
        for current in self.list_items_bounded(list, false, 8, MAX_MOD_TYPES)? {
            if types.len() >= MAX_MOD_TYPES {
                return Err(corrupt("mod array", list, "aggregate type budget exceeded"));
            }
            let ty = self.ptr(current)?;
            let multi = ty != 0
                && self.process.read_i32(ty)? == 0x1000000
                && self.process.read_i32(add(ty, 3)?)? == 8193;
            if multi {
                let array = self.ptr(add(current, 0x10)?)?;
                if array == 0 {
                    return Err(corrupt("mod array", current, "null nested array"));
                }
                let count = self.process.read_i32(add(array, 8)?)?;
                if !(0..=MAX_COLLECTION as i32).contains(&count) {
                    return Err(corrupt("mod array", array, "invalid count"));
                }
                if types.len().saturating_add(count as usize) > MAX_MOD_TYPES {
                    return Err(corrupt(
                        "mod array",
                        array,
                        "aggregate type budget exceeded",
                    ));
                }
                for i in 0..count as usize {
                    let item = self.ptr(add(array, 0x10 + i * 8)?)?;
                    types.push(self.ptr(item)?);
                }
            } else {
                types.push(ty)
            }
        }
        Ok(types)
    }

    pub(super) fn ensure_mod_mapping(&mut self, mode: i64) -> Result<(), MemoryError> {
        let Some(mapped) = self.mapped_modes.get(mode as usize) else {
            return Ok(());
        };
        if *mapped {
            return Ok(());
        }
        let available = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_desktop_osu_game_desktop.available_mods,
        )?)?;
        if available == 0 {
            return Ok(());
        }
        let dict = self.ptr(add(available, 0x20)?)?;
        if dict == 0 {
            return Ok(());
        }
        let entries = self.ptr(add(dict, 0x10)?)?;
        let count = self.process.read_i32(add(dict, 0x38)?)?;
        if !(0..=64).contains(&count) {
            return Err(corrupt("available mods dictionary", dict, "invalid count"));
        }
        if count < 6 {
            return Err(corrupt(
                "available mods dictionary",
                dict,
                "missing categories",
            ));
        }
        let mut types: [Vec<usize>; 6] = std::array::from_fn(|_| Vec::new());
        for (slot, ty) in types.iter_mut().enumerate().take((count as usize).min(6)) {
            let value = self.ptr(add(entries, 0x10 + slot * 0x18)?)?;
            *ty = self.read_mod_types(value)?
        }
        let old_osu = self
            .offsets
            .osu_version
            .replace('.', "")
            .parse::<u64>()
            .is_ok_and(|v| v <= 20_261_190);
        let categories = match mode {
            0 if old_osu => &OSU_OLD_MOD_CATEGORIES,
            0 => &OSU_MOD_CATEGORIES,
            1 => &TAIKO_MOD_CATEGORIES,
            2 => &FRUITS_MOD_CATEGORIES,
            3 => &MANIA_MOD_CATEGORIES,
            _ => return Ok(()),
        };
        for category in 0..6 {
            for (index, &acronym) in categories[category].iter().enumerate() {
                if let Some(&ty) = types[category].get(index) {
                    self.mod_mappings.insert((mode, ty), acronym);
                }
            }
        }
        self.mapped_modes[mode as usize] = true;
        Ok(())
    }

    pub(super) fn bind_ptr(&self, object: usize, offset: usize) -> Result<usize, MemoryError> {
        let bind = self.ptr(add(object, offset)?)?;
        if bind == 0 {
            return Err(corrupt("mod bindable", object, "null pointer"));
        }
        Ok(bind)
    }
    pub(super) fn bind_bool(&self, object: usize, offset: usize) -> Result<Value, MemoryError> {
        Ok(Value::Bool(
            self.process
                .read_u8(add(self.bind_ptr(object, offset)?, 0x40)?)?
                == 1,
        ))
    }
    pub(super) fn bind_i32(&self, object: usize, offset: usize) -> Result<Value, MemoryError> {
        Ok(Value::from(
            self.process
                .read_i32(add(self.bind_ptr(object, offset)?, 0x40)?)?,
        ))
    }
    pub(super) fn bind_i32_string(
        &self,
        object: usize,
        offset: usize,
    ) -> Result<Value, MemoryError> {
        Ok(Value::String(
            self.process
                .read_i32(add(self.bind_ptr(object, offset)?, 0x40)?)?
                .to_string(),
        ))
    }
    pub(super) fn bind_f32(&self, object: usize, offset: usize) -> Result<Value, MemoryError> {
        Ok(Value::from(
            self.process
                .read_f32(add(self.bind_ptr(object, offset)?, 0x40)?)? as f64,
        ))
    }
    pub(super) fn bind_f64(&self, object: usize, offset: usize) -> Result<Value, MemoryError> {
        Ok(Value::from(
            self.process
                .read_f64(add(self.bind_ptr(object, offset)?, 0x40)?)?,
        ))
    }
    pub(super) fn setting(settings: &mut IndexMap<String, Value>, key: &str, value: Value) {
        settings.insert(key.to_owned(), value);
    }

    pub(super) fn read_mod(&self, acronym: &str, object: usize) -> Result<Mod, MemoryError> {
        let mut settings = IndexMap::new();
        match acronym {
            "EZ" if self.selected_gamemode != 1 => Self::setting(
                &mut settings,
                "retries",
                Value::from(self.process.read_i32(add(object, 0x20)?)?),
            ),
            "HT" | "DT" => {
                Self::setting(&mut settings, "speed_change", self.bind_f64(object, 0x10)?);
                Self::setting(&mut settings, "adjust_pitch", self.bind_bool(object, 0x18)?);
            }
            "DC" | "NC" => {
                Self::setting(&mut settings, "speed_change", self.bind_f64(object, 0x10)?)
            }
            "SD" => {
                if self.selected_gamemode == 0 {
                    Self::setting(
                        &mut settings,
                        "fail_on_slider_tail",
                        self.bind_bool(object, 0x20)?,
                    )
                }
                Self::setting(&mut settings, "restart", self.bind_bool(object, 0x10)?);
            }
            "PF" => {
                if self.selected_gamemode == 3 {
                    Self::setting(
                        &mut settings,
                        "require_perfect_hits",
                        self.bind_bool(object, 0x20)?,
                    )
                }
                Self::setting(&mut settings, "restart", self.bind_bool(object, 0x10)?);
            }
            "HD" if self.selected_gamemode == 0 => Self::setting(
                &mut settings,
                "only_fade_approach_circles",
                self.bind_bool(object, 0x20)?,
            ),
            "FL" => {
                if self.selected_gamemode == 0 {
                    Self::setting(&mut settings, "follow_delay", self.bind_f64(object, 0x18)?);
                    Self::setting(
                        &mut settings,
                        "combo_based_size",
                        self.bind_bool(object, 0x28)?,
                    );
                    Self::setting(
                        &mut settings,
                        "size_multiplier",
                        self.bind_f32(object, 0x20)?,
                    )
                } else {
                    Self::setting(
                        &mut settings,
                        "size_multiplier",
                        self.bind_f32(object, 0x18)?,
                    );
                    Self::setting(
                        &mut settings,
                        "combo_based_size",
                        self.bind_bool(object, 0x20)?,
                    )
                }
            }
            "AC" => {
                Self::setting(
                    &mut settings,
                    "minimum_accuracy",
                    self.bind_f64(object, 0x20)?,
                );
                Self::setting(
                    &mut settings,
                    "accuracy_judge_mode",
                    self.bind_i32_string(object, 0x28)?,
                );
                Self::setting(&mut settings, "restart", self.bind_bool(object, 0x10)?);
            }
            "TP" => {
                let seed = self.bind_ptr(object, 0x20)?;
                Self::setting(
                    &mut settings,
                    "seed",
                    Value::from(self.process.read_i32(add(seed, 0x48)?)?),
                );
                Self::setting(&mut settings, "metronome", self.bind_bool(object, 0x28)?);
            }
            "DA" => {
                let current = |offset| -> Result<usize, MemoryError> {
                    let outer = self.bind_ptr(object, offset)?;
                    self.ptr(add(outer, 0x60)?)
                };
                let od_offset = if self.selected_gamemode == 3 {
                    0x28
                } else {
                    0x18
                };
                if self.selected_gamemode == 0 || self.selected_gamemode == 2 {
                    let ar = current(0x30)?;
                    let cs = current(0x28)?;
                    Self::setting(
                        &mut settings,
                        "approach_rate",
                        Value::from(self.process.read_f32(add(ar, 0x40)?)? as f64),
                    );
                    Self::setting(
                        &mut settings,
                        "circle_size",
                        Value::from(self.process.read_f32(add(cs, 0x40)?)? as f64),
                    );
                    if self.selected_gamemode == 2 {
                        Self::setting(
                            &mut settings,
                            "hard_rock_offsets",
                            self.bind_bool(object, 0x38)?,
                        )
                    }
                } else if self.selected_gamemode == 1 {
                    let scroll = current(0x28)?;
                    Self::setting(
                        &mut settings,
                        "scroll_speed",
                        Value::from(self.process.read_f32(add(scroll, 0x40)?)? as f64),
                    );
                }
                let drain = current(0x10)?;
                let od = current(od_offset)?;
                Self::setting(
                    &mut settings,
                    "drain_rate",
                    Value::from(self.process.read_f32(add(drain, 0x40)?)? as f64),
                );
                Self::setting(
                    &mut settings,
                    "overall_difficulty",
                    Value::from(self.process.read_f32(add(od, 0x40)?)? as f64),
                );
                Self::setting(
                    &mut settings,
                    "extended_limits",
                    self.bind_bool(object, 0x20)?,
                );
            }
            "CL" if self.selected_gamemode == 0 => {
                for (key, offset) in [
                    ("no_slider_head_accuracy", 0x10),
                    ("classic_note_lock", 0x18),
                    ("always_play_tail_sample", 0x20),
                    ("fade_hit_circle_early", 0x28),
                    ("classic_health", 0x30),
                ] {
                    Self::setting(&mut settings, key, self.bind_bool(object, offset)?)
                }
            }
            "RD" => {
                let seed = self.bind_ptr(object, 0x10)?;
                if self.selected_gamemode != 3 && self.selected_gamemode != 1 {
                    Self::setting(
                        &mut settings,
                        "angle_sharpness",
                        self.bind_f32(object, 0x18)?,
                    )
                }
                Self::setting(
                    &mut settings,
                    "seed",
                    Value::from(self.process.read_i32(add(seed, 0x48)?)?),
                );
            }
            "MR" if self.selected_gamemode < 2 => Self::setting(
                &mut settings,
                "reflection",
                self.bind_i32_string(object, 0x10)?,
            ),
            "WG" => Self::setting(&mut settings, "strength", self.bind_f64(object, 0x20)?),
            "GR" | "DF" => {
                Self::setting(&mut settings, "start_scale", self.bind_f32(object, 0x20)?)
            }
            "WU" | "WD" => {
                Self::setting(&mut settings, "initial_rate", self.bind_f64(object, 0x30)?);
                Self::setting(&mut settings, "final_rate", self.bind_f64(object, 0x38)?);
                Self::setting(&mut settings, "adjust_pitch", self.bind_bool(object, 0x40)?);
            }
            "BR" => {
                Self::setting(&mut settings, "spin_speed", self.bind_f64(object, 0x10)?);
                Self::setting(
                    &mut settings,
                    "direction",
                    self.bind_i32_string(object, 0x18)?,
                );
            }
            "AD" => {
                Self::setting(&mut settings, "scale", self.bind_f32(object, 0x10)?);
                Self::setting(&mut settings, "style", self.bind_i32_string(object, 0x18)?);
            }
            "MU" => {
                Self::setting(
                    &mut settings,
                    "inverse_muting",
                    self.bind_bool(object, 0x28)?,
                );
                Self::setting(
                    &mut settings,
                    "enable_metronome",
                    self.bind_bool(object, 0x30)?,
                );
                Self::setting(
                    &mut settings,
                    "mute_combo_count",
                    self.bind_i32(object, 0x38)?,
                );
                Self::setting(
                    &mut settings,
                    "affects_hit_sounds",
                    self.bind_bool(object, 0x40)?,
                );
            }
            "NS" => Self::setting(
                &mut settings,
                "hidden_combo_count",
                self.bind_i32(
                    object,
                    if self.selected_gamemode == 2 {
                        0x28
                    } else {
                        0x30
                    },
                )?,
            ),
            "MG" => Self::setting(
                &mut settings,
                "attraction_strength",
                self.bind_f32(object, 0x10)?,
            ),
            "RP" => Self::setting(
                &mut settings,
                "repulsion_strength",
                self.bind_f32(object, 0x10)?,
            ),
            "AS" => {
                Self::setting(&mut settings, "initial_rate", self.bind_f64(object, 0x10)?);
                Self::setting(&mut settings, "adjust_pitch", self.bind_bool(object, 0x18)?);
            }
            "DP" => {
                Self::setting(&mut settings, "max_depth", self.bind_f32(object, 0x20)?);
                Self::setting(
                    &mut settings,
                    "show_approach_circles",
                    self.bind_bool(object, 0x28)?,
                );
            }
            "CO" => {
                Self::setting(&mut settings, "coverage", self.bind_f32(object, 0x20)?);
                Self::setting(
                    &mut settings,
                    "direction",
                    self.bind_i32_string(object, 0x28)?,
                );
            }
            "BM" => {
                Self::setting(
                    &mut settings,
                    "max_size_combo_count",
                    self.bind_i32(object, 0x20)?,
                );
                Self::setting(
                    &mut settings,
                    "max_cursor_size",
                    self.bind_f32(object, 0x28)?,
                );
            }
            "SR" => {
                Self::setting(
                    &mut settings,
                    "one_third_conversion",
                    self.bind_bool(object, 0x10)?,
                );
                Self::setting(
                    &mut settings,
                    "one_sixth_conversion",
                    self.bind_bool(object, 0x18)?,
                );
                Self::setting(
                    &mut settings,
                    "one_eighth_conversion",
                    self.bind_bool(object, 0x20)?,
                );
            }
            _ => {}
        }
        Ok(Mod {
            acronym: acronym.to_owned(),
            settings: (!settings.is_empty()).then_some(settings),
        })
    }

    pub(super) fn selected_mods(&mut self) -> Result<Option<CalculateMods>, MemoryError> {
        let _ = self.ensure_mod_mapping(self.selected_gamemode);
        let selected = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_desktop_osu_game_desktop.selected_mods,
        )?)?;
        if selected == 0 || self.process.read_u8(add(selected, 0x50)?)? == 1 {
            return Ok(None);
        }
        let list = self.ptr(add(selected, 0x20)?)?;
        let mut mods = Vec::new();
        for object in self.list_items(list, false, 8)? {
            let ty = self.ptr(object)?;
            if let Some(&acronym) = self.mod_mappings.get(&(self.selected_gamemode, ty)) {
                mods.push(self.read_mod(acronym, object)?)
            }
        }
        Ok(Some(calculate_mods(mods)))
    }
    #[cfg(windows)]
    pub(super) fn read_windows_input_settings(
        &self,
        values: &mut Settings,
    ) -> Result<(), MemoryError> {
        let host = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_framework_game.host_k_backing_field,
        )?)?;
        let config = self.ptr(add(
            host,
            self.offsets
                .osu_framework_platform_game_host
                .input_config_k_backing_field,
        )?)?;
        let handlers = self.ptr(add(
            config,
            self.offsets
                .osu_framework_configuration_input_config_manager
                .input_handlers_k_backing_field,
        )?)?;
        let array = self.ptr(add(handlers, 8)?)?;
        let count = self.process.read_i32(add(array, 8)?)?;
        if !(0..=64).contains(&count) {
            return Err(corrupt("input handlers", array, "invalid count"));
        }
        for index in 0..count as usize {
            let current = self.ptr(add(array, 0x10 + index * 8)?)?;
            let enabled = self
                .process
                .read_u8(add(self.ptr(add(current, 0x18)?)?, 0x40)?)?
                == 1;
            if index == 1 {
                let offsets = &self
                    .offsets
                    .osu_framework_input_handlers_tablet_open_tablet_driver_handler;
                let area_offset = self.ptr(add(current, offsets.area_offset_k_backing_field)?)?;
                let area_size = self.ptr(add(current, offsets.area_size_k_backing_field)?)?;
                let rotation = self.ptr(add(current, offsets.rotation_k_backing_field)?)?;
                let pressure =
                    self.ptr(add(current, offsets.pressure_threshold_k_backing_field)?)?;
                values.insert("tablet.enabled".into(), Value::from(enabled));
                values.insert(
                    "tablet.x".into(),
                    Value::from(self.process.read_f32(add(area_offset, 0x44)?)?),
                );
                values.insert(
                    "tablet.y".into(),
                    Value::from(self.process.read_f32(add(area_offset, 0x48)?)?),
                );
                values.insert(
                    "tablet.width".into(),
                    Value::from(self.process.read_f32(add(area_size, 0x44)?)?),
                );
                values.insert(
                    "tablet.height".into(),
                    Value::from(self.process.read_f32(add(area_size, 0x48)?)?),
                );
                values.insert(
                    "tablet.rotation".into(),
                    Value::from(self.process.read_f32(add(rotation, 0x40)?)?),
                );
                values.insert(
                    "tablet.pressureThreshold".into(),
                    Value::from(self.process.read_f32(add(pressure, 0x40)?)?),
                );
            }
            if index == 6 {
                let relative = self.ptr(add(
                    current,
                    self.offsets
                        .osu_framework_input_handlers_mouse_mouse_handler
                        .use_relative_mode_k_backing_field,
                )?)?;
                values.insert(
                    "mouse.rawInput".into(),
                    Value::from(self.process.read_u8(add(relative, 0x40)?)? == 1),
                );
            }
        }
        Ok(())
    }
    pub(super) fn multiplayer_room(&self) -> Result<usize, MemoryError> {
        let c = self.ptr(add(
            self.game_base()?,
            self.offsets
                .osu_game_osu_game_base
                .multiplayer_client_k_backing_field,
        )?)?;
        if c == 0 {
            return Ok(0);
        }
        self.ptr(add(
            c,
            self.offsets
                .osu_game_online_multiplayer_multiplayer_client
                .room,
        )?)
    }
    pub(super) fn multi_room(&self) -> Result<usize, MemoryError> {
        let c = self.ptr(add(
            self.game_base()?,
            self.offsets
                .osu_game_osu_game_base
                .multiplayer_client_k_backing_field,
        )?)?;
        if c == 0 {
            return Ok(0);
        }
        let connected = self.ptr(add(
            c,
            self.offsets
                .osu_game_online_multiplayer_online_multiplayer_client
                .is_connected_k_backing_field,
        )?)?;
        if connected == 0 || self.process.read_u8(add(connected, 0x40)?)? != 1 {
            return Ok(0);
        }
        self.ptr(add(
            c,
            self.offsets
                .osu_game_online_multiplayer_multiplayer_client
                .room,
        )?)
    }
}

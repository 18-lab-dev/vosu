use super::*;

impl LazerMemory {
    pub(super) fn read_osu_config(&mut self) -> Result<Settings, MemoryError> {
        let mut out = IndexMap::new();
        let local = self.ptr(add(
            self.game_base()?,
            self.offsets
                .osu_game_osu_game_base
                .local_config_k_backing_field,
        )?)?;
        let values = self.config_store(local)?;
        let address = |map: &IndexMap<i32, usize>, key: i32, context: &'static str| {
            map.get(&key)
                .copied()
                .ok_or_else(|| corrupt(context, key as usize, "missing required key"))
        };
        let i32v = |this: &Self, key: i32| {
            this.process
                .read_i32(address(&values, key, "local config")?)
        };
        let f64v = |this: &Self, key: i32| {
            this.process
                .read_f64(address(&values, key, "local config")?)
        };
        let f32v = |this: &Self, key: i32| {
            this.process
                .read_f32(address(&values, key, "local config")?)
        };
        let boolv = |this: &Self, key: i32| {
            Ok::<_, MemoryError>(
                this.process
                    .read_u8(address(&values, key, "local config")?)?
                    == 1,
            )
        };
        out.insert("client.branch".into(), Value::from(i32v(self, 34)?));
        self.scoring_display_mode = i32v(self, 58)?;
        out.insert("leaderboardType".into(), Value::from(i32v(self, 30)?));
        out.insert("sortType".into(), Value::from(i32v(self, 40)?));
        out.insert("groupType".into(), Value::from(i32v(self, 39)?));
        self.leaderboard_visible = boolv(self, 12)?;
        self.replay_settings_overlay = boolv(self, 88)?;
        for (key, name, mul) in [
            (6, "background.dim", 100.0),
            (7, "background.blur", 100.0),
            (22, "audio.offset.universal", 1.0),
            (23, "audio.volume.masterInactive", 100.0),
        ] {
            out.insert(name.into(), Value::from(js_fixed2(f64v(self, key)? * mul)));
        }
        for (key, name) in [(2, "cursor.menuSize"), (3, "cursor.size")] {
            out.insert(name.into(), Value::from(js_fixed2(f32v(self, key)? as f64)));
        }
        for (key, name) in [
            (4, "cursor.autoSize"),
            (10, "background.storyboard"),
            (19, "mouse.disableButtons"),
            (20, "mouse.disableWheel"),
        ] {
            out.insert(name.into(), Value::from(boolv(self, key)?));
        }
        out.insert("background.video".into(), Value::from(boolv(self, 10)?));
        let beatmap_hitsounds = boolv(self, 56)?;
        out.insert(
            "audio.ignoreBeatmapSounds".into(),
            Value::from(!beatmap_hitsounds),
        );
        out.insert(
            "audio.useSkinSamples".into(),
            Value::from(!beatmap_hitsounds),
        );
        out.insert(
            "skin.ignoreBeatmapSkins".into(),
            Value::from(!boolv(self, 54)?),
        );
        let framework = self.ptr(add(
            self.game_base()?,
            self.offsets
                .osu_game_osu_game
                .framework_config_k_backing_field,
        )?)?;
        let fv = self.config_store(framework)?;
        let fa = |key| address(&fv, key, "framework config");
        out.insert(
            "resolution.fullscreen".into(),
            Value::from(self.process.read_i32(fa(14)?)? == 2),
        );
        let windowed = fa(7)?;
        out.insert(
            "resolution.width".into(),
            Value::from(self.process.read_i32(add(windowed, 4)?)?),
        );
        out.insert(
            "resolution.height".into(),
            Value::from(self.process.read_i32(add(windowed, 8)?)?),
        );
        let fullscreen = fa(11)?;
        out.insert(
            "resolution.widthFullscreen".into(),
            Value::from(self.process.read_i32(add(fullscreen, 4)?)?),
        );
        out.insert(
            "resolution.heightFullscreen".into(),
            Value::from(self.process.read_i32(add(fullscreen, 8)?)?),
        );
        for (key, name) in [
            (3, "audio.volume.master"),
            (5, "audio.volume.music"),
            (4, "audio.volume.effect"),
        ] {
            out.insert(
                name.into(),
                Value::from(js_fixed2(self.process.read_f64(fa(key)?)? * 100.0)),
            );
        }
        out.insert(
            "mouse.sensitivity".into(),
            Value::from(self.process.read_f64(fa(21)?)?),
        );
        let cache = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_game_osu_game_base.ruleset_config_cache,
        )?)?;
        if cache == 0 {
            return Err(corrupt("ruleset config cache", cache, "null cache"));
        }
        let dict = self.ptr(add(
            cache,
            self.offsets
                .osu_game_rulesets_ruleset_config_cache
                .config_cache,
        )?)?;
        if dict == 0 {
            return Err(corrupt("ruleset config cache", cache, "null dictionary"));
        }
        let entries = self.ptr(add(dict, 0x10)?)?;
        let count = self.process.read_i32(add(dict, 0x38)?)?;
        if !(0..=64).contains(&count) {
            return Err(corrupt("ruleset config cache", dict, "invalid count"));
        }
        if count > 0 && entries == 0 {
            return Err(corrupt("ruleset config cache", dict, "null entries"));
        }
        for index in 0..count as usize {
            let entry = add(entries, 0x10 + index * 0x18)?;
            if self.string_at(entry)? != "mania" {
                continue;
            }
            let config = self.ptr(add(entry, 8)?)?;
            if config == 0 {
                continue;
            }
            let mania = self.config_store(config)?;
            out.insert(
                "mania.scrollSpeed".into(),
                Value::from(self.process.read_f64(address(&mania, 0, "mania config")?)?),
            );
            out.insert(
                "mania.scrollDirection".into(),
                Value::from(self.process.read_i32(address(&mania, 1, "mania config")?)?),
            );
            break;
        }
        Ok(out)
    }

    pub(super) fn read_gamemode(&self) -> Result<i64, MemoryError> {
        let bindable = self.ptr(add(
            self.game_base()?,
            self.offsets.osu_desktop_osu_game_desktop.ruleset,
        )?)?;
        let info = self.ptr(add(bindable, 0x20)?)?;
        self.i32(add(
            info,
            self.offsets
                .osu_game_rulesets_ruleset_info
                .online_id_k_backing_field,
        )?)
    }

    pub(super) fn api(&self) -> Result<usize, MemoryError> {
        self.ptr(add(
            self.game_base()?,
            self.offsets.osu_game_osu_game_base.api_k_backing_field,
        )?)
    }
    pub(super) fn spectator_client(&self) -> Result<usize, MemoryError> {
        self.ptr(add(
            self.game_base()?,
            self.offsets
                .osu_game_osu_game_base
                .spectator_client_k_backing_field,
        )?)
    }
}

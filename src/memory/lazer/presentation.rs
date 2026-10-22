use super::*;
use crate::model::memory::{ClientRenderState, PlaybackClock};

impl LazerMemory {
    pub(super) fn set_playback_source(&mut self, replay: bool, spectator: bool) {
        self.watching_replay = replay && !spectator;
        self.spectating = replay && spectator;
    }
    pub(super) fn render_state(&self, player: usize) -> ClientRenderState {
        ClientRenderState {
            spectating: self.spectating,
            clock: self.playback_clock(player).ok(),
        }
    }

    pub(super) fn playback_clock(&self, player: usize) -> Result<PlaybackClock, MemoryError> {
        let missing = || corrupt("playback clock", 0, "unmeasured clock fields");
        let o = &self.offsets;
        let component = o.audio_component.as_ref().ok_or_else(missing)?;
        let adjustments = o.audio_adjustments.as_ref().ok_or_else(missing)?;
        let aggregate = o.aggregate_double.as_ref().ok_or_else(missing)?;
        let bindable = o.bindable_double.as_ref().ok_or_else(missing)?;
        let running_offset = o.osu_framework_timing_decoupling_framed_clock.running;
        if running_offset == 0 {
            return Err(missing());
        }
        let clock = match self.gameplay_clock_for_player(player)? {
            Some(clock) => clock,
            None => self.beatmap_clock()?,
        };
        let decoupled = self.ptr(add(
            clock,
            o.osu_game_beatmaps_framed_beatmap_clock.decoupled_track,
        )?)?;
        let running = self.process.read_u8(add(decoupled, running_offset)?)?;
        if running > 1 {
            return Err(corrupt("playback clock", decoupled, "invalid running flag"));
        }
        let track = self.ptr(add(
            decoupled,
            o.osu_framework_timing_decoupling_framed_clock
                .source_k_backing_field,
        )?)?;
        let audio = self.ptr(add(track, component.adjustments)?)?;
        let value = |offset| -> Result<f64, MemoryError> {
            let source = self.ptr(add(audio, offset)?)?;
            let result = self.ptr(add(source, aggregate.result)?)?;
            self.process.read_f64(add(result, bindable.value)?)
        };
        let rate = value(adjustments.frequency)? * value(adjustments.tempo)?;
        if !rate.is_finite() {
            return Err(corrupt("playback clock", track, "non-finite track rate"));
        }
        Ok(PlaybackClock {
            rate,
            running: running == 1,
        })
    }

    pub(super) fn ruleset_visual_settings(&self, out: &mut Settings) -> Result<(), MemoryError> {
        for key in [
            "visual.snakingInSliders",
            "visual.snakingOutSliders",
            "visual.hitAnimations",
            "visual.rateAdjustedHitAnimation",
            "mania.timingBasedNoteColouring",
        ] {
            out.insert(key.into(), Value::Null);
        }
        let cache = self.root(GameRoot::RulesetConfigCache)?;
        let dict = self.ptr(add(
            cache,
            self.offsets
                .osu_game_rulesets_ruleset_config_cache
                .config_cache,
        )?)?;
        let entries = self.ptr(add(dict, 0x10)?)?;
        let count = self.process.read_i32(add(dict, 0x38)?)?;
        if !(0..=64).contains(&count) {
            return Err(corrupt("ruleset config cache", dict, "invalid count"));
        }
        for i in 0..count as usize {
            let entry = add(entries, 0x10 + i * 0x18)?;
            let name = self.string_at(entry)?;
            let fields: &[(i32, &str)] = match name.as_str() {
                "mania" => &[(2, "mania.timingBasedNoteColouring")],
                "osu" => &[
                    (0, "visual.snakingInSliders"),
                    (1, "visual.snakingOutSliders"),
                    (2, "visual.hitAnimations"),
                ],
                "taiko" => &[
                    (1, "visual.rateAdjustedHitAnimation"),
                    (2, "visual.hitAnimations"),
                ],
                _ => continue,
            };
            // Visual toggles belong to the selected ruleset, not whichever cached
            // config was enumerated last. Mania colouring can be retained independently.
            if name != "mania"
                && !matches!(
                    (name.as_str(), self.selected_gamemode),
                    ("osu", 0) | ("taiko", 1)
                )
            {
                continue;
            }
            let config = self.ptr(add(entry, 8)?)?;
            if config == 0 {
                continue;
            }
            let store = self.config_store(config)?;
            for (key, path) in fields {
                if let Some(address) = store.get(key) {
                    out.insert(
                        (*path).into(),
                        Value::from(self.process.read_u8(*address)? == 1),
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn skin_base(
        &self,
        manager: usize,
        skin: usize,
    ) -> Result<Option<&'static str>, MemoryError> {
        let o = &self.offsets.osu_game_skinning_skin_manager;
        if [o.legacy_skin, o.argon_skin, o.triangles_skin, o.retro_skin].contains(&0) {
            return Ok(None);
        }
        let ty = self.ptr(skin)?;
        // CoreCLR MethodTable parent at +0x10, measured against BaseType.TypeHandle
        // on the pinned AppImage CLR for ArgonProSkin and DefaultLegacySkin.
        let parent = self.ptr(add(ty, 0x10)?)?;
        for (offset, family) in [
            (o.legacy_skin, "legacy"),
            (o.argon_skin, "argon"),
            (o.triangles_skin, "triangles"),
            (o.retro_skin, "retro"),
        ] {
            let builtin = self.ptr(add(manager, offset)?)?;
            let builtin_type = self.ptr(builtin)?;
            if ty == builtin_type {
                return Ok(Some(family));
            }
            if family == "argon" && parent == builtin_type {
                return Ok(Some("argon-pro"));
            }
            if family == "legacy" && ty == self.ptr(add(builtin_type, 0x10)?)? {
                return Ok(Some("legacy"));
            }
        }
        Ok(None)
    }
}

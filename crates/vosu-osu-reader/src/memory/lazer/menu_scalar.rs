use super::*;

impl LazerMemory {
    pub(super) fn menu_scalar(&mut self, previous: &str) -> MemoryRead<Menu> {
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
            filename: lazer_path(&self.string_at(add(info, b.hash_k_backing_field)?)?)?,
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
            audio_filename: lazer_path(audio_hash)?,
            audio_file_mimetype: mime(&audio_name),
            background_filename: lazer_path(background_hash)?,
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
            resources: Some(Arc::new(vosu_osu_model::memory::BeatmapResources {
                files,
                audio_name,
                background_name,
            })),
        })))
    }
}

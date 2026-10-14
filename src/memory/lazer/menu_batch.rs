//! Batched full [`MemoryReader::menu`] read: each pointer level's independent fields share
//! one native batch. `None` defers to the sequential decoder (see `batch.rs`).
use super::resolver::list_shape;
use super::*;

/// Beatmap files the batched menu read decodes; a list of pointer slots this long fills one
/// batched list read.
pub(super) const BATCHED_MENU_FILES: usize = BATCHED_LIST_BYTES / 8;

/// A managed string: its object and the page-clamped head (`Self::request`) read next.
struct Text {
    object: usize,
    address: usize,
    head: [u8; STRING_HEAD],
    len: usize,
}

impl Text {
    fn new(memory: &LazerMemory, pointer: &[u8; 8]) -> Option<Self> {
        let object = le_pointer(pointer)?;
        let (address, len) = if object == 0 {
            (0, 0)
        } else {
            memory.string_head_span(object)?
        };
        Some(Self {
            object,
            address,
            head: [0; STRING_HEAD],
            len,
        })
    }
    fn request(&mut self) -> Option<ReadRequest<'_>> {
        (self.object != 0).then(|| ReadRequest::new(self.address, &mut self.head[..self.len]))
    }
    fn decode(&self, memory: &LazerMemory) -> Option<String> {
        if self.object == 0 {
            return Some(String::new());
        }
        memory.string_from_head(self.object, &self.head[..self.len])
    }
}

fn le_f32(bytes: &[u8; 4]) -> f64 {
    f32::from_le_bytes(*bytes) as f64
}

impl LazerMemory {
    /// Reads independent requests as one native batch, in address order so the Windows
    /// backend can merge neighbouring fields.
    fn read_sorted(&self, requests: &mut [ReadRequest<'_>]) -> Option<()> {
        requests.sort_unstable_by_key(|request| request.address);
        match requests {
            [] => Some(()),
            [request] => self.process.read_exact(request.address, request.dst).ok(),
            requests => self.process.read_many(requests).ok(),
        }
    }

    /// `menu(previous)` from `current_beatmap` on, as the sequential decoder reads it.
    pub(super) fn menu_full_batched(&self, previous: &str) -> Option<MemoryRead<Menu>> {
        if self.process.pointer_size() != 8 || usize::BITS != 64 {
            return None;
        }
        let w = &self
            .offsets
            .osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap;
        let beatmap = self.root(GameRoot::Beatmap).ok()?;
        let ruleset = self.root(GameRoot::Ruleset).ok()?;

        let [working, ruleset_info] = pointers(read_fixed(
            &self.process,
            [
                Some(add(beatmap, 0x20).ok()?),
                Some(add(ruleset, 0x20).ok()?),
            ],
        )?)?;

        let (mut info, mut set, mut gamemode) = ([0u8; 8], [0u8; 8], [0u8; 4]);
        self.read_sorted(&mut [
            ReadRequest::new(add(working, w.beatmap_info).ok()?, &mut info),
            ReadRequest::new(add(working, w.beatmap_set_info).ok()?, &mut set),
            ReadRequest::new(
                add(
                    ruleset_info,
                    self.offsets
                        .osu_game_rulesets_ruleset_info
                        .online_id_k_backing_field,
                )
                .ok()?,
                &mut gamemode,
            ),
        ])?;
        let (info, set) = (le_pointer(&info)?, le_pointer(&set)?);
        let gamemode = le_i32(&gamemode) as i64;
        if info == 0 {
            return Some(self.not_ready());
        }
        self.menu_full_from(info, set, gamemode, previous)
    }

    /// [`Self::menu_full_batched`] from a non-null beatmap info and its set, as
    /// `current_beatmap` reads them, and the gamemode `read_gamemode` returns.
    pub(super) fn menu_full_from(
        &self,
        info: usize,
        set: usize,
        gamemode: i64,
        previous: &str,
    ) -> Option<MemoryRead<Menu>> {
        if self.process.pointer_size() != 8 || usize::BITS != 64 {
            return None;
        }
        let b = &self.offsets.osu_game_beatmaps_beatmap_info;
        let m = &self.offsets.osu_game_beatmaps_beatmap_metadata;
        let d = &self.offsets.osu_game_beatmaps_beatmap_difficulty;
        let (mut status, mut map_id, mut object_count, mut set_id) =
            ([0u8; 4], [0u8; 4], [0u8; 4], [0u8; 4]);
        let (mut md5, mut metadata, mut difficulty, mut file_hash, mut difficulty_name, mut files) =
            ([0u8; 8], [0u8; 8], [0u8; 8], [0u8; 8], [0u8; 8], [0u8; 8]);
        {
            let set_online_id = self
                .offsets
                .osu_game_beatmaps_beatmap_set_info
                .online_id_k_backing_field;
            let mut requests = vec![
                ReadRequest::new(add(info, b.status_int_k_backing_field).ok()?, &mut status),
                ReadRequest::new(add(info, b.md5_hash_k_backing_field).ok()?, &mut md5),
                ReadRequest::new(add(info, b.metadata_k_backing_field).ok()?, &mut metadata),
                ReadRequest::new(
                    add(info, b.difficulty_k_backing_field).ok()?,
                    &mut difficulty,
                ),
                ReadRequest::new(add(info, b.hash_k_backing_field).ok()?, &mut file_hash),
                ReadRequest::new(
                    add(info, b.difficulty_name_k_backing_field).ok()?,
                    &mut difficulty_name,
                ),
                ReadRequest::new(add(info, b.online_id_k_backing_field).ok()?, &mut map_id),
                ReadRequest::new(
                    add(info, b.total_object_count_k_backing_field).ok()?,
                    &mut object_count,
                ),
                ReadRequest::new(add(set, set_online_id).ok()?, &mut set_id),
            ];
            // `beatmap_files` reads nothing for a null set.
            if set != 0 {
                requests.push(ReadRequest::new(add(set, 0x20).ok()?, &mut files));
            }
            self.read_sorted(&mut requests)?;
        }
        let ranked_status = lazer_ranked_status(le_i32(&status));
        let (metadata, difficulty, files) = (
            le_pointer(&metadata)?,
            le_pointer(&difficulty)?,
            le_pointer(&files)?,
        );

        let mut md5 = Text::new(self, &md5)?;
        let mut file_hash = Text::new(self, &file_hash)?;
        let mut difficulty_name = Text::new(self, &difficulty_name)?;
        let mut fields = [[0u8; 8]; 7];
        let mut floats = [[0u8; 4]; 4];
        let mut header = [0u8; 12];
        {
            let [
                author,
                audio,
                background,
                artist,
                artist_unicode,
                title,
                title_unicode,
            ] = &mut fields;
            let [approach_rate, circle_size, drain_rate, overall_difficulty] = &mut floats;
            let mut requests = Vec::with_capacity(15);
            requests.extend(md5.request());
            requests.extend(file_hash.request());
            requests.extend(difficulty_name.request());
            for (offset, dst) in [
                (m.author_k_backing_field, author),
                (m.audio_file_k_backing_field, audio),
                (m.background_file_k_backing_field, background),
                (m.artist_k_backing_field, artist),
                (m.artist_unicode_k_backing_field, artist_unicode),
                (m.title_k_backing_field, title),
                (m.title_unicode_k_backing_field, title_unicode),
            ] {
                requests.push(ReadRequest::new(add(metadata, offset).ok()?, dst));
            }
            for (offset, dst) in [
                (d.approach_rate_k_backing_field, approach_rate),
                (d.circle_size_k_backing_field, circle_size),
                (d.drain_rate_k_backing_field, drain_rate),
                (d.overall_difficulty_k_backing_field, overall_difficulty),
            ] {
                requests.push(ReadRequest::new(add(difficulty, offset).ok()?, dst));
            }
            if files != 0 {
                requests.push(ReadRequest::new(add(files, 8).ok()?, &mut header));
            }
            self.read_sorted(&mut requests)?;
        }
        let hash = md5.decode(self)?;
        if hash == previous {
            return Some(Ok(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
                gamemode,
                ranked_status,
            }))));
        }
        let author = le_pointer(&fields[0])?;
        if author == 0 {
            return None;
        }
        let (size, items) = if files == 0 {
            (0, 0)
        } else {
            list_shape(
                files,
                le_i32(&header[8..]),
                u64::from_le_bytes(header[..8].try_into().unwrap()),
            )
            .ok()?
        };
        // Per-file buffers are allocated up front; larger (or corrupt) lists take the
        // sequential decoder, which reads one file at a time.
        if size > BATCHED_MENU_FILES {
            return None;
        }

        let mut username = [0u8; 8];
        let mut texts = fields[1..]
            .iter()
            .map(|pointer| Text::new(self, pointer))
            .collect::<Option<Vec<_>>>()?;
        let mut slots = vec![0u8; size * 8];
        {
            let mut requests = Vec::with_capacity(texts.len() + 2);
            requests.push(ReadRequest::new(
                add(
                    author,
                    self.offsets
                        .osu_game_models_realm_user
                        .username_k_backing_field,
                )
                .ok()?,
                &mut username,
            ));
            requests.extend(texts.iter_mut().filter_map(Text::request));
            if size != 0 {
                requests.push(ReadRequest::new(items, &mut slots));
            }
            self.read_sorted(&mut requests)?;
        }

        // Each file entry holds its realm file at +0x18 and its name at +0x20.
        let mut creator = Text::new(self, &username)?;
        let mut entries = vec![[0u8; 16]; size];
        {
            let mut requests = Vec::with_capacity(size + 1);
            requests.extend(creator.request());
            for (slot, entry) in slots.chunks_exact(8).zip(entries.iter_mut()) {
                let current = le_pointer(slot)?;
                if current == 0 {
                    return None;
                }
                requests.push(ReadRequest::new(add(current, 0x18).ok()?, entry));
            }
            self.read_sorted(&mut requests)?;
        }

        let mut names = Vec::with_capacity(size);
        let mut realm_hashes = vec![[0u8; 8]; size];
        {
            let mut requests = Vec::with_capacity(2 * size);
            for entry in &entries {
                let realm = le_pointer(&entry[..8])?;
                if realm == 0 {
                    return None;
                }
                names.push(Text::new(self, entry[8..].try_into().unwrap())?);
            }
            for (entry, hash) in entries.iter().zip(realm_hashes.iter_mut()) {
                requests.push(ReadRequest::new(
                    add(le_pointer(&entry[..8])?, 0x18).ok()?,
                    hash,
                ));
            }
            requests.extend(names.iter_mut().filter_map(Text::request));
            self.read_sorted(&mut requests)?;
        }
        let mut hashes = realm_hashes
            .iter()
            .map(|pointer| Text::new(self, pointer))
            .collect::<Option<Vec<_>>>()?;
        {
            let mut requests = hashes
                .iter_mut()
                .filter_map(Text::request)
                .collect::<Vec<_>>();
            self.read_sorted(&mut requests)?;
        }
        let mut file_map = IndexMap::with_capacity(size);
        for (name, hash) in names.iter().zip(&hashes) {
            file_map.insert(name.decode(self)?, hash.decode(self)?);
        }

        let creator = creator.decode(self)?;
        let [
            audio_name,
            background_name,
            artist,
            artist_original,
            title,
            title_original,
        ] = [0, 1, 2, 3, 4, 5].map(|index| texts[index].decode(self));
        let (audio_name, background_name) = (audio_name?, background_name?);
        let audio_hash = file_map.get(&audio_name).map(String::as_str).unwrap_or("");
        let background_hash = file_map
            .get(&background_name)
            .map(String::as_str)
            .unwrap_or("");
        Some(Ok(ReadStatus::Ready(Menu::Full(MenuFull {
            gamemode,
            checksum: hash,
            filename: lazer_path(&file_hash.decode(self)?).ok()?,
            plays: 0,
            artist: artist?,
            artist_original: artist_original?,
            title: title?,
            title_original: title_original?,
            ar: le_f32(&floats[0]),
            cs: le_f32(&floats[1]),
            hp: le_f32(&floats[2]),
            od: le_f32(&floats[3]),
            audio_filename: lazer_path(audio_hash).ok()?,
            audio_file_mimetype: mime(&audio_name),
            background_filename: lazer_path(background_hash).ok()?,
            background_file_mimetype: mime(&background_name),
            folder: String::new(),
            creator,
            difficulty: difficulty_name.decode(self)?,
            map_id: le_i32(&map_id) as i64,
            set_id: le_i32(&set_id) as i64,
            ranked_status,
            object_count: le_i32(&object_count) as i64,
        }))))
    }
}
